use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use tonic::{Request, Response, Status};
use tokio_stream::StreamExt;
use tracing::{info, warn};
use webrtc::peer_connection::offer_answer_options::RTCOfferOptions;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

use crate::media::MediaSource;
use crate::pb as dp;
use crate::peer::Peer;
use dp::dataplane_server::Dataplane;

const EVENTS_CHANNEL_CAP: usize = 256;

#[derive(Clone)]
struct Configured {
    webrtc: dp::WebRtcConfig,
    capture: dp::CaptureConfig,
}

pub struct Service {
    /// session_id -> peer
    peers: Mutex<HashMap<String, Arc<Peer>>>,
    /// resolved configuration pushed by the control plane
    config: RwLock<Option<Configured>>,
    /// capture pipelines, started on the first peer and stopped with the last
    media: RwLock<Option<Arc<MediaSource>>>,
    /// control-plane event fanout (ICE candidates, states, data channel)
    events_tx: tokio::sync::broadcast::Sender<dp::Event>,
}

impl Service {
    pub fn new() -> Self {
        let (events_tx, _) = tokio::sync::broadcast::channel(EVENTS_CHANNEL_CAP);
        Self {
            peers: Mutex::new(HashMap::new()),
            config: RwLock::new(None),
            media: RwLock::new(None),
            events_tx,
        }
    }

    fn configured(&self) -> Result<Configured, Status> {
        self.config
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| Status::failed_precondition("not configured yet"))
    }

    fn peer(&self, session_id: &str) -> Result<Arc<Peer>, Status> {
        self.peers
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("unknown session: {session_id}")))
    }

    /// Starts the capture pipelines on the first peer.
    async fn ensure_media(&self, config: &Configured) -> Result<Option<Arc<MediaSource>>, Status> {
        let existing = self.media.read().unwrap().clone();
        if existing.is_some() || config.capture.video_pipeline.is_empty() {
            return Ok(existing);
        }

        let mut media = self.media.write().unwrap();
        if media.is_none() {
            let video = config.capture.video_pipeline.clone();
            let audio = config.capture.audio_pipeline.clone();
            // gst set_state talks to X and can block indefinitely; keep it
            // off the tonic/tokio worker threads
            let source = tokio::task::spawn_blocking(move || MediaSource::start(&video, &audio))
                .await
                .map_err(|err| Status::internal(format!("media start task: {err}")))?
                .map_err(|err| Status::internal(format!("start media source: {err}")))?;

            *media = source.clone();
        }

        Ok(media.clone())
    }

    /// Stops the capture pipelines when the last peer goes away.
    async fn drop_media_if_idle(&self) {
        let mut media = self.media.write().unwrap();
        if self.peers.lock().unwrap().is_empty() {
            if let Some(source) = media.take() {
                info!("last peer gone, stopping media source");
                drop(source);
            }
        }
    }
}

#[tonic::async_trait]
impl Dataplane for Service {
    type EventsStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<dp::Event, Status>> + Send>>;

    async fn configure(
        &self,
        request: Request<dp::ConfigureRequest>,
    ) -> Result<Response<dp::ConfigureResponse>, Status> {
        let req = request.into_inner();
        let webrtc = req.config.ok_or_else(|| Status::invalid_argument("missing config"))?;
        let capture = req.capture.unwrap_or_default();

        if webrtc.ephemeral_min > 0 {
            warn!(
                epr = %format!("{}-{}", webrtc.ephemeral_min, webrtc.ephemeral_max),
                "NEKO_WEBRTC_EPR has no webrtc-rs equivalent and is ignored; \
                 pin the container ephemeral range instead: \
                 docker run --sysctl net.ipv4.ip_local_port_range=53000-53100"
            );
        }

        info!(
            ice_lite = webrtc.ice_lite,
            ice_trickle = webrtc.ice_trickle,
            nat1to1 = %webrtc.nat1to1_ips.join(","),
            epr = %format!("{}-{}", webrtc.ephemeral_min, webrtc.ephemeral_max),
            video_codec = %capture.video_codec,
            video_pipeline = %capture.video_pipeline,
            audio = !capture.audio_pipeline.is_empty(),
            "configured by control plane"
        );

        *self.config.write().unwrap() = Some(Configured { webrtc, capture });

        Ok(Response::new(dp::ConfigureResponse {}))
    }

    async fn create_peer(
        &self,
        request: Request<dp::CreatePeerRequest>,
    ) -> Result<Response<dp::CreatePeerResponse>, Status> {
        let session_id = request.into_inner().session_id;
        let config = self.configured()?;

        let media = self.ensure_media(&config).await?;

        let (peer, sdp) = Peer::create(
            &session_id,
            &config.webrtc,
            &config.capture.video_codec,
            media,
            self.events_tx.clone(),
        )
        .await
        .map_err(|err| Status::internal(format!("create peer: {err}")))?;

        self.peers.lock().unwrap().insert(session_id.clone(), peer);
        info!(session_id = %session_id, "peer created");

        Ok(Response::new(dp::CreatePeerResponse { sdp }))
    }

    async fn set_remote_description(
        &self,
        request: Request<dp::SetRemoteDescriptionRequest>,
    ) -> Result<Response<dp::SetRemoteDescriptionResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        let desc = match req.r#type.as_str() {
            "offer" => RTCSessionDescription::offer(req.sdp)
                .map_err(|err| Status::invalid_argument(format!("invalid offer sdp: {err}")))?,
            "answer" => RTCSessionDescription::answer(req.sdp)
                .map_err(|err| Status::invalid_argument(format!("invalid answer sdp: {err}")))?,
            other => return Err(Status::invalid_argument(format!("unknown sdp type: {other}"))),
        };

        peer.pc
            .set_remote_description(desc)
            .await
            .map_err(|err| Status::internal(format!("set remote description: {err}")))?;

        Ok(Response::new(dp::SetRemoteDescriptionResponse {}))
    }

    async fn create_offer(
        &self,
        request: Request<dp::CreateOfferRequest>,
    ) -> Result<Response<dp::CreateOfferResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        let offer = peer
            .pc
            .create_offer(Some(RTCOfferOptions {
                ice_restart: req.ice_restart,
                ..Default::default()
            }))
            .await
            .map_err(|err| Status::internal(format!("create offer: {err}")))?;

        peer.pc
            .set_local_description(offer.clone())
            .await
            .map_err(|err| Status::internal(format!("set local description: {err}")))?;

        Ok(Response::new(dp::CreateOfferResponse { sdp: offer.sdp }))
    }

    async fn create_answer(
        &self,
        request: Request<dp::CreateAnswerRequest>,
    ) -> Result<Response<dp::CreateAnswerResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        let answer = peer
            .pc
            .create_answer(None)
            .await
            .map_err(|err| Status::internal(format!("create answer: {err}")))?;

        peer.pc
            .set_local_description(answer.clone())
            .await
            .map_err(|err| Status::internal(format!("set local description: {err}")))?;

        Ok(Response::new(dp::CreateAnswerResponse { sdp: answer.sdp }))
    }

    async fn add_ice_candidate(
        &self,
        request: Request<dp::AddIceCandidateRequest>,
    ) -> Result<Response<dp::AddIceCandidateResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        let init = req.candidate.unwrap_or(dp::IceCandidateInit {
            candidate: String::new(),
            ..Default::default()
        });

        peer.pc
            .add_ice_candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                candidate: init.candidate,
                sdp_mid: init.sdp_mid,
                sdp_mline_index: init.sdp_mline_index.map(|v| v as u16),
                username_fragment: init.username_fragment,
            })
            .await
            .map_err(|err| Status::internal(format!("add ice candidate: {err}")))?;

        Ok(Response::new(dp::AddIceCandidateResponse {}))
    }

    async fn destroy_peer(
        &self,
        request: Request<dp::DestroyPeerRequest>,
    ) -> Result<Response<dp::DestroyPeerResponse>, Status> {
        let session_id = request.into_inner().session_id;

        let removed = { self.peers.lock().unwrap().remove(&session_id) };
        if let Some(peer) = removed {
            info!(session_id = %session_id, "destroy peer");
            if let Err(err) = peer.pc.close().await {
                warn!(?err, session_id = %session_id, "peer close failed");
            }
            drop(peer);
            self.drop_media_if_idle().await;
        }

        Ok(Response::new(dp::DestroyPeerResponse {}))
    }

    async fn set_paused(
        &self,
        request: Request<dp::SetPausedRequest>,
    ) -> Result<Response<dp::SetPausedResponse>, Status> {
        let req = request.into_inner();
        self.peer(&req.session_id)?.set_paused(req.paused);
        Ok(Response::new(dp::SetPausedResponse {}))
    }

    async fn set_video(
        &self,
        request: Request<dp::SetVideoRequest>,
    ) -> Result<Response<dp::SetVideoResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        if let Some(disabled) = req.disabled {
            peer.set_video_disabled(disabled);
        }
        // selector resolution (exact/auto against multiple streams) is
        // phase 3; a single "main" stream makes selectors a no-op for now

        Ok(Response::new(dp::SetVideoResponse {}))
    }

    async fn set_audio(
        &self,
        request: Request<dp::SetAudioRequest>,
    ) -> Result<Response<dp::SetAudioResponse>, Status> {
        let req = request.into_inner();
        let peer = self.peer(&req.session_id)?;

        if let Some(disabled) = req.disabled {
            peer.set_audio_disabled(disabled);
        }

        Ok(Response::new(dp::SetAudioResponse {}))
    }

    async fn send_data(
        &self,
        request: Request<dp::SendDataRequest>,
    ) -> Result<Response<dp::SendDataResponse>, Status> {
        let req = request.into_inner();
        let data = req.data;

        if req.session_id.is_empty() {
            let peers: Vec<Arc<Peer>> = {
                self.peers.lock().unwrap().values().cloned().collect()
            };
            for peer in peers {
                peer.send_data(&data).await;
            }
        } else {
            self.peer(&req.session_id)?.send_data(&data).await;
        }

        Ok(Response::new(dp::SendDataResponse {}))
    }

    async fn events(
        &self,
        _request: Request<dp::EventsRequest>,
    ) -> Result<Response<Self::EventsStream>, Status> {
        info!("control plane connected the events stream");

        let rx = self.events_tx.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).map(|result| {
            result.map_err(|err| Status::internal(format!("events stream: {err}")))
        });
        Ok(Response::new(Box::pin(stream)))
    }
}
