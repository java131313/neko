//! Peer manager: owns the peers map and the lazy media source, and turns
//! peer events into session-socket traffic. Port of the dataplane service
//! peer lifecycle fused with the Go websocket signal handler.

pub mod peer;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::capture::{self, MediaSource};
use crate::config::Config;
use crate::messages;
use crate::room::State;

use peer::Peer;

/// Events a peer pushes to the forwarder task (started in main).
#[derive(Debug, Clone)]
pub enum PeerEvent {
    IceCandidate {
        session_id: String,
        candidate: Value,
    },
    Connection {
        session_id: String,
        state: String,
    },
    DataMessage {
        session_id: String,
        data: Vec<u8>,
    },
}

pub struct Manager {
    config: Config,
    peers: RwLock<HashMap<String, Arc<Peer>>>,
    media: RwLock<Option<Arc<MediaSource>>>,
    events: mpsc::UnboundedSender<PeerEvent>,
}

impl Manager {
    pub fn new(config: Config, events: mpsc::UnboundedSender<PeerEvent>) -> Self {
        Manager {
            config,
            peers: RwLock::new(HashMap::new()),
            media: RwLock::new(None),
            events,
        }
    }

    /// signal/request: create the peer (the server offers immediately,
    /// neko v3 flow) and hand back the signal/provide envelope.
    pub async fn create_peer(
        &self,
        session_id: &str,
    ) -> Result<messages::Envelope> {
        let media = self.ensure_media().await?;
        let (peer, sdp) = Peer::create(
            session_id,
            &self.config,
            media,
            self.events.clone(),
        )
        .await
        .context("create peer")?;

        self.peers.write().await.insert(session_id.to_owned(), peer);
        info!(%session_id, "peer created");

        Ok(messages::Envelope {
            event: messages::server_events::SIGNAL_PROVIDE.to_owned(),
            payload: serde_json::to_value(messages::SignalProvide {
                sdp,
                iceservers: serde_json::to_value(&self.config.webrtc.ice_servers).ok(),
                video: "main".to_owned(),
                audio: "main".to_owned(),
            })
            .unwrap_or_default(),
        })
    }

    pub async fn peer(&self, session_id: &str) -> Option<Arc<Peer>> {
        self.peers.read().await.get(session_id).cloned()
    }

    pub async fn destroy_peer(&self, session_id: &str) {
        if let Some(peer) = self.peers.write().await.remove(session_id) {
            info!(%session_id, "destroy peer");
            peer.close().await;
            drop(peer);
            self.drop_media_if_idle().await;
        }
    }

    /// The server saw the media path fail first: drop our peer and tell the
    /// client (signal/close) so it re-requests with backoff.
    pub async fn close_peer(&self, state: &State, session_id: &str) {
        if self.peers.write().await.remove(session_id).is_some() {
            state
                .send_to(
                    session_id,
                    &messages::Envelope {
                        event: messages::server_events::SIGNAL_CLOSE.to_owned(),
                        payload: serde_json::json!({ "id": session_id }),
                    },
                )
                .await;
            info!(%session_id, "signal/close sent");
        }
        self.drop_media_if_idle().await;
    }

    async fn ensure_media(&self) -> Result<Option<Arc<MediaSource>>> {
        {
            let media = self.media.read().await;
            if media.is_some() {
                return Ok(media.clone());
            }
        }

        let mut media = self.media.write().await;
        if media.is_none() {
            let config = &self.config.capture;
            let video_pipeline = if config.video_pipeline.is_empty() {
                capture::default_video_pipeline(
                    &config.display,
                    config.video_show_pointer,
                    &config.video_codec,
                )
            } else {
                config.video_pipeline.replace("{display}", &config.display)
            };
            let audio_pipeline = if config.audio_pipeline.is_empty() {
                capture::default_audio_pipeline(&config.audio_device)
            } else {
                config
                    .audio_pipeline
                    .replace("{device}", &config.audio_device)
            };

            let source = MediaSource::start(&video_pipeline, &audio_pipeline)?;
            info!(%video_pipeline, %audio_pipeline, "media source started");
            *media = source;
        }
        Ok(media.clone())
    }

    async fn drop_media_if_idle(&self) {
        let mut media = self.media.write().await;
        if self.peers.read().await.is_empty() {
            if let Some(source) = media.take() {
                info!("last peer gone, stopping media source");
                drop(source);
            }
        }
    }
}

/// The forwarder: peer events -> session sockets + input dispatch.
/// Runs as a task for the server's lifetime; started in main.
pub async fn forward_events(mut rx: mpsc::UnboundedReceiver<PeerEvent>, state: State) {
    while let Some(event) = rx.recv().await {
        match event {
            PeerEvent::IceCandidate { session_id, candidate } => {
                // the candidate init travels flattened, like Go's embedded
                // SignalCandidate{ICECandidateInit}
                state
                    .send_to(
                        &session_id,
                        &messages::Envelope {
                            event: messages::server_events::SIGNAL_CANDIDATE.to_owned(),
                            payload: candidate,
                        },
                    )
                    .await;
            }
            PeerEvent::Connection { session_id, state: conn_state } => {
                match conn_state.as_str() {
                    "connected" => info!(%session_id, "peer connected"),
                    "failed" => {
                        state.inner.webrtc.close_peer(&state, &session_id).await;
                    }
                    other => debug_log(other),
                }
            }
            PeerEvent::DataMessage { session_id, data } => {
                handle_data_message(&state, &session_id, &data).await;
            }
        }
    }
}

fn debug_log(state: &str) {
    tracing::debug!(%state, "peer connection state");
}

/// Data channel input: decode and act. Phase C replies to pings; the
/// desktop routing (move/scroll/key/button/touch) is phase D.
async fn handle_data_message(state: &State, session_id: &str, data: &[u8]) {
    use crate::payload::{self, ClientMessage, Decoded, ServerMessage};
    match payload::decode_client(data) {
        Ok(Decoded::Msg(ClientMessage::Ping {
            client_ts1,
            client_ts2,
        })) => {
            // pong: echo the client timestamps + server time, like the Go impl
            let server_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let pong = ServerMessage::Pong {
                client_ts1,
                client_ts2,
                server_ts1: (server_ts / u32::MAX as u64) as u32,
                server_ts2: (server_ts % u32::MAX as u64) as u32,
            };
            if let Some(peer) = state.inner.webrtc.peer(session_id).await {
                peer.send_data(&pong.encode()).await;
            }
        }
        Ok(decoded) => {
            tracing::debug!(?decoded, "input event (phase D routes to desktop)");
        }
        Err(err) => {
            tracing::debug!(?err, "invalid data channel message");
        }
    }
}
