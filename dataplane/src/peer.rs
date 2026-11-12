use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::media::{EncodedFrame, MediaSource};
use crate::pb::{event, DataChannelEvent, DataMessage, Event, IceCandidateEvent, IceCandidateInit};

/// Mirrors the parts of types.WebRTCPeer the dataplane must honor.
#[derive(Debug, Default, Clone)]
pub struct PeerState {
    pub paused: bool,
    pub video_disabled: bool,
    pub audio_disabled: bool,
}

pub struct Peer {
    pub session_id: String,
    pub pc: webrtc::peer_connection::RTCPeerConnection,
    pub data_channel: Arc<webrtc::data_channel::RTCDataChannel>,
    video_track: Arc<TrackLocalStaticSample>,
    audio_track: Arc<TrackLocalStaticSample>,
    video_gate: Arc<AtomicBool>,
    audio_gate: Arc<AtomicBool>,
    state: Arc<std::sync::Mutex<PeerState>>,
    _writers: Vec<tokio::task::JoinHandle<()>>,
}

impl Peer {
    pub async fn create(
        session_id: &str,
        webrtc_config: &crate::pb::WebRtcConfig,
        video_codec: &str,
        media: Option<Arc<MediaSource>>,
        events: broadcast::Sender<Event>,
    ) -> Result<(Arc<Self>, String)> {
        let mut media_engine = MediaEngine::default();

        let video_capability = register_codecs(&mut media_engine, video_codec)?;

        // NOTE: webrtc-rs has no equivalent of pion's ephemeral UDP port
        // range; deployments pin the container's net.ipv4.ip_local_port_range
        // instead (see dataplane/README.md).
        let registry = Registry::new();
        let registry = register_default_interceptors(registry, &mut media_engine)?;

        let mut setting_engine = SettingEngine::default();
        setting_engine.set_lite(webrtc_config.ice_lite);
        setting_engine.set_nat_1to1_ips(
            webrtc_config.nat1to1_ips.clone(),
            RTCIceCandidateType::Host,
        );

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(setting_engine)
            .build();

        let pc = api.new_peer_connection(RTCConfiguration::default()).await?;

        // "data" channel, created by the server side (matches the embedded
        // implementation; carries input events and cursor state)
        let data_channel = pc.create_data_channel("data", None).await?;

        // outbound tracks; media source may be absent (smoke tests)
        let video_track = Arc::new(TrackLocalStaticSample::new(
            video_capability,
            "video".to_owned(),
            "neko".to_owned(),
        ));
        let audio_track = Arc::new(TrackLocalStaticSample::new(
            audio_capability(),
            "audio".to_owned(),
            "neko".to_owned(),
        ));

        pc.add_track(video_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        pc.add_track(audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        // gates are independent of the peer struct, so the track writers
        // can be spawned before the Arc and owned by it
        let video_gate = Arc::new(AtomicBool::new(true));
        let audio_gate = Arc::new(AtomicBool::new(true));

        let writers = match &media {
            Some(source) => vec![
                spawn_track_writer(
                    video_track.clone(),
                    source.subscribe_video(),
                    video_gate.clone(),
                ),
                spawn_track_writer(
                    audio_track.clone(),
                    source.subscribe_audio(),
                    audio_gate.clone(),
                ),
            ],
            None => vec![],
        };

        let peer = Arc::new(Self {
            session_id: session_id.to_owned(),
            pc,
            data_channel,
            video_track,
            audio_track,
            video_gate,
            audio_gate,
            state: Arc::new(std::sync::Mutex::new(PeerState::default())),
            _writers: writers,
        });

        peer.wire_events(events)?;

        // the server generates the offer (neko v3 flow: SIGNAL_PROVIDE)
        let offer = peer.pc.create_offer(None).await?;
        peer.pc.set_local_description(offer.clone()).await?;

        let sdp = offer.sdp;
        info!(session_id = %session_id, sdp_len = sdp.len(), "peer created, offer generated");

        Ok((peer, sdp))
    }

    fn wire_events(&self, events: broadcast::Sender<Event>) -> Result<()> {
        let session_id = self.session_id.clone();

        // ICE candidates -> control plane -> browser
        let events_dc = events.clone();
        self.pc.on_ice_candidate(Box::new(move |candidate| {
            let events = events_dc.clone();
            let session_id = session_id.clone();
            Box::pin(async move {
                let Some(candidate) = candidate else { return };
                match candidate.to_json() {
                    Ok(init) => {
                        let _ = events.send(Event {
                            kind: Some(event::Kind::IceCandidate(IceCandidateEvent {
                                session_id,
                                candidate: Some(IceCandidateInit {
                                    candidate: init.candidate,
                                    sdp_mid: init.sdp_mid,
                                    sdp_mline_index: init.sdp_mline_index.map(u32::from),
                                    username_fragment: init.username_fragment,
                                }),
                            })),
                        });
                    }
                    Err(err) => warn!(?err, "candidate.to_json failed"),
                }
            })
        }));

        let session_state = self.session_id.clone();
        let events_state = events.clone();
        self.pc.on_peer_connection_state_change(Box::new(
            move |state: RTCPeerConnectionState| {
                let events = events_state.clone();
                let session_id = session_state.clone();
                Box::pin(async move {
                    info!(session_id = %session_id, %state, "peer connection state changed");
                    let _ = events.send(Event {
                        kind: Some(event::Kind::ConnectionState(
                            crate::pb::ConnectionStateEvent {
                                session_id,
                                state: state.to_string(),
                            },
                        )),
                    });
                })
            },
        ));

        // data channel lifecycle and inbound messages (user input events)
        let session_open = self.session_id.clone();
        let events_open = events.clone();
        self.data_channel.on_open(Box::new(move || {
            let events = events_open.clone();
            let session_id = session_open.clone();
            Box::pin(async move {
                info!(session_id = %session_id, "data channel opened");
                let _ = events.send(Event {
                    kind: Some(event::Kind::DataChannel(DataChannelEvent {
                        session_id,
                        open: true,
                    })),
                });
            })
        }));

        let session_close = self.session_id.clone();
        let events_close = events.clone();
        self.data_channel.on_close(Box::new(move || {
            let events = events_close.clone();
            let session_id = session_close.clone();
            Box::pin(async move {
                debug!(session_id = %session_id, "data channel closed");
                let _ = events.send(Event {
                    kind: Some(event::Kind::DataChannel(DataChannelEvent {
                        session_id,
                        open: false,
                    })),
                });
            })
        }));

        let session_msg = self.session_id.clone();
        self.data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let events = events.clone();
            let session_id = session_msg.clone();
            Box::pin(async move {
                let _ = events.send(Event {
                    kind: Some(event::Kind::DataMessage(DataMessage {
                        session_id,
                        data: msg.data.to_vec(),
                    })),
                });
            })
        }));

        Ok(())
    }

    pub fn set_paused(&self, paused: bool) {
        self.state.lock().unwrap().paused = paused;
        self.update_gates();
    }

    pub fn set_video_disabled(&self, disabled: bool) {
        self.state.lock().unwrap().video_disabled = disabled;
        self.update_gates();
    }

    pub fn set_audio_disabled(&self, disabled: bool) {
        self.state.lock().unwrap().audio_disabled = disabled;
        self.update_gates();
    }

    fn update_gates(&self) {
        let state = self.state.lock().unwrap();
        self.video_gate
            .store(!state.paused && !state.video_disabled, Ordering::Relaxed);
        self.audio_gate
            .store(!state.paused && !state.audio_disabled, Ordering::Relaxed);
    }

    pub async fn send_data(&self, data: &[u8]) {
        use webrtc::data_channel::data_channel_state::RTCDataChannelState;
        if self.data_channel.ready_state() != RTCDataChannelState::Open {
            return;
        }
        if let Err(err) = self.data_channel.send(&bytes::Bytes::copy_from_slice(data)).await {
            debug!(?err, "data channel send failed");
        }
    }
}

fn self_session_id(id: &str) -> String {
    id.to_owned()
}

/// Writes broadcasted frames into one peer's track. Skips frames while the
/// gate is closed (paused/disabled) and drops lagged batches — real-time
/// semantics: show the newest, never queue.
fn spawn_track_writer(
    track: Arc<TrackLocalStaticSample>,
    mut rx: broadcast::Receiver<EncodedFrame>,
    gate: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = match rx.recv().await {
                Ok(frame) => frame,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!("lagged, skipped {skipped} frame(s)");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };

            if !gate.load(Ordering::Relaxed) {
                continue;
            }

            let sample = Sample {
                data: frame.data,
                duration: frame.duration,
                ..Default::default()
            };

            if let Err(err) = track.write_sample(&sample).await {
                // broken pipe / EOF happens when the peer connection closes
                debug!(?err, "track writer stopping");
                break;
            }
        }
    })
}

fn register_codecs(
    media_engine: &mut MediaEngine,
    video_codec: &str,
) -> Result<RTCRtpCodecCapability> {
    // mirrors pkg/types/codec.RTCPFeedback
    let feedback = || {
        vec![
            RTCPFeedback {
                typ: "transport-cc".to_owned(),
                parameter: "".to_owned(),
            },
            RTCPFeedback {
                typ: "goog-remb".to_owned(),
                parameter: "".to_owned(),
            },
            RTCPFeedback {
                typ: "ccm".to_owned(),
                parameter: "fir".to_owned(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "pli".to_owned(),
            },
            RTCPFeedback {
                typ: "nack".to_owned(),
                parameter: "".to_owned(),
            },
        ]
    };

    let (mime, payload, fmtp) = match video_codec.to_lowercase().as_str() {
        "vp8" | "" => (MIME_TYPE_VP8, 96u8, ""),
        "h264" => (
            MIME_TYPE_H264,
            102u8,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
        ),
        other => bail!("unsupported video codec: {other}"),
    };

    let video_capability = RTCRtpCodecCapability {
        mime_type: mime.to_owned(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line: fmtp.to_owned(),
        rtcp_feedback: feedback(),
    };

    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: video_capability.clone(),
                payload_type: payload,
                ..Default::default()
            },
            RTPCodecType::Video,
        )
        .context("register video codec")?;

    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: audio_capability(),
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )
        .context("register audio codec")?;

    Ok(video_capability)
}

fn audio_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "useinbandfec=1;stereo=1".to_owned(),
        rtcp_feedback: vec![],
    }
}
