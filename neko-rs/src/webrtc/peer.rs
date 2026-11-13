//! One viewer's peer connection: tracks, the "data" channel, and the
//! event wiring. Ported from dataplane/src/peer.rs (compile-verified);
//! proto types replaced by in-process event channels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::{RTCConfiguration, RTCIceServer};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use crate::capture::{EncodedFrame, MediaSource};
use crate::config::Config;
use crate::webrtc::PeerEvent;

/// Peer state toggles from SetPaused / SetVideo / SetAudio.
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
        config: &Config,
        media: Option<Arc<MediaSource>>,
        events: mpsc::UnboundedSender<PeerEvent>,
    ) -> Result<(Arc<Self>, String)> {
        let mut media_engine = MediaEngine::default();
        let video_capability = register_codecs(&mut media_engine, &config.capture.video_codec)?;

        // no webrtc-rs equivalent of pion's ephemeral port range:
        // deployments pin net.ipv4.ip_local_port_range instead
        let registry = Registry::new();
        let registry = register_default_interceptors(registry, &mut media_engine)?;

        let mut setting_engine = SettingEngine::default();
        // ICE-Lite cannot gather relay candidates — when TURN servers are
        // configured (the embedded coturn / SealOS path), full ICE wins
        let ice_lite = config.webrtc.ice_lite && config.webrtc.ice_servers.is_empty();
        if config.webrtc.ice_lite && !ice_lite {
            info!("TURN servers configured: ICE-Lite disabled (relay needs full ICE)");
        }
        setting_engine.set_lite(ice_lite);
        setting_engine.set_nat_1to1_ips(config.webrtc.nat_1to1_ips.clone(), RTCIceCandidateType::Host);

        let rtc_config = RTCConfiguration {
            ice_servers: config
                .webrtc
                .ice_servers
                .iter()
                .map(|s| RTCIceServer {
                    urls: s.urls.clone(),
                    username: s.username.clone(),
                    credential: s.credential.clone(),
                })
                .collect(),
            ..Default::default()
        };

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(setting_engine)
            .build();

        let pc = api.new_peer_connection(rtc_config).await?;

        // "data" channel, server-side, like the embedded implementation:
        // carries input events and cursor/pong replies
        let data_channel = pc.create_data_channel("data", None).await?;

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

        pc.add_track(video_track.clone() as Arc<dyn TrackLocal + Send + Sync>).await?;
        pc.add_track(audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>).await?;

        let video_gate = Arc::new(AtomicBool::new(true));
        let audio_gate = Arc::new(AtomicBool::new(true));

        let writers = match &media {
            Some(source) => vec![
                spawn_track_writer(video_track.clone(), source.subscribe_video(), video_gate.clone()),
                spawn_track_writer(audio_track.clone(), source.subscribe_audio(), audio_gate.clone()),
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

        // the server generates the offer (neko v3 flow: signal/provide)
        let offer = peer.pc.create_offer(None).await?;
        peer.pc.set_local_description(offer.clone()).await?;

        Ok((peer, offer.sdp))
    }

    fn wire_events(&self, events: mpsc::UnboundedSender<PeerEvent>) -> Result<()> {
        let session_id = self.session_id.clone();

        // ICE candidates -> client via signal/candidate
        let events_ice = events.clone();
        self.pc.on_ice_candidate(Box::new(move |candidate| {
            let events = events_ice.clone();
            let session_id = session_id.clone();
            Box::pin(async move {
                let Some(candidate) = candidate else { return };
                match candidate.to_json() {
                    Ok(init) => {
                        let value = serde_json::to_value(init).unwrap_or(Value::Null);
                        let _ = events.send(PeerEvent::IceCandidate { session_id, candidate: value });
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
                    info!(%session_id, %state, "peer connection state changed");
                    let _ = events.send(PeerEvent::Connection {
                        session_id,
                        state: state.to_string(),
                    });
                })
            },
        ));

        // data channel: input events from the browser
        let session_msg = self.session_id.clone();
        self.data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let events = events.clone();
            let session_id = session_msg.clone();
            Box::pin(async move {
                let _ = events.send(PeerEvent::DataMessage {
                    session_id,
                    data: msg.data.to_vec(),
                });
            })
        }));

        Ok(())
    }

    pub async fn set_remote(&self, type_: &str, sdp: &str) -> Result<()> {
        let desc = match type_ {
            "offer" => RTCSessionDescription::offer(sdp.to_owned())
                .map_err(|e| anyhow::anyhow!("invalid offer: {e}"))?,
            "answer" => RTCSessionDescription::answer(sdp.to_owned())
                .map_err(|e| anyhow::anyhow!("invalid answer: {e}"))?,
            other => bail!("unknown sdp type: {other}"),
        };
        self.pc
            .set_remote_description(desc)
            .await
            .context("set remote description")
    }

    /// Client renegotiation (microphone): answer their offer.
    pub async fn create_answer(&self) -> Result<String> {
        let answer = self.pc.create_answer(None).await?;
        self.pc.set_local_description(answer.clone()).await?;
        Ok(answer.sdp)
    }

    pub async fn create_offer(&self, ice_restart: bool) -> Result<String> {
        use webrtc::peer_connection::offer_answer_options::RTCOfferOptions;
        let offer = self
            .pc
            .create_offer(Some(RTCOfferOptions {
                ice_restart,
                ..Default::default()
            }))
            .await?;
        self.pc.set_local_description(offer.clone()).await?;
        Ok(offer.sdp)
    }

    pub async fn add_ice_candidate(&self, value: Value) -> Result<()> {
        let init: webrtc::ice_transport::ice_candidate::RTCIceCandidateInit =
            serde_json::from_value(value).context("candidate json")?;
        self.pc.add_ice_candidate(init).await.context("add candidate")
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

    pub async fn close(&self) {
        if let Err(err) = self.pc.close().await {
            warn!(?err, "peer close failed");
        }
    }
}

fn spawn_track_writer(
    track: Arc<TrackLocalStaticSample>,
    mut rx: broadcast::Receiver<EncodedFrame>,
    gate: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = match rx.recv().await {
                Ok(frame) => frame,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
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
            if track.write_sample(&sample).await.is_err() {
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
            RTCPFeedback { typ: "transport-cc".to_owned(), parameter: "".to_owned() },
            RTCPFeedback { typ: "goog-remb".to_owned(), parameter: "".to_owned() },
            RTCPFeedback { typ: "ccm".to_owned(), parameter: "fir".to_owned() },
            RTCPFeedback { typ: "nack".to_owned(), parameter: "pli".to_owned() },
            RTCPFeedback { typ: "nack".to_owned(), parameter: "".to_owned() },
        ]
    };

    let (mime, payload, fmtp) = match video_codec.to_lowercase().as_str() {
        "h264" => (
            MIME_TYPE_H264,
            102u8,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
        ),
        other => {
            if other != "vp8" && !other.is_empty() {
                // fall back to vp8 for anything unknown, like the Go default
            }
            (MIME_TYPE_VP8, 96u8, "")
        }
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
