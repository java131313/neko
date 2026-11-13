//! WebSocket endpoint (`/api/ws`): session lifecycle, the event pump, and
//! the signalling bridge into webrtc/. Port of server/internal/websocket.

use axum::extract::ws::Message as AxumMessage;
use tokio::sync::mpsc;

use crate::messages::Envelope;

/// A handle to a live session socket: broadcast channel into the ws task.
#[derive(Clone)]
pub struct Socket {
    pub tx: mpsc::UnboundedSender<Envelope>,
}

impl Socket {
    pub fn send(&self, envelope: Envelope) -> bool {
        self.tx.send(envelope).is_ok()
    }
}

/// TODO(phase:room): full connection handler —
/// - authenticate via ?token= / cookie
/// - create/resume session, broadcast session/created + system/init
/// - pump events (room:: broadcast -> socket, socket -> handlers)
/// - heartbeat ticker, merciful reconnect grace (5s), stale cleanup
pub async fn handle_ws() -> anyhow::Result<()> {
    Ok(())
}

pub fn axum_to_envelope(msg: AxumMessage) -> Option<Envelope> {
    let text = match msg {
        AxumMessage::Text(t) => t,
        _ => return None,
    };
    serde_json::from_str(&text).ok()
}
