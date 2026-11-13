//! WebSocket endpoint (`/api/ws`): authentication, session lifecycle, the
//! event pump, heartbeat, and message dispatch. Port of
//! server/internal/websocket (manager.connect/handle + handler/).

use axum::extract::ws::{CloseFrame, Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::messages::{self, Envelope};
use crate::room::{AuthError, State};

/// Per-connection context handed to the writer task.
pub struct Connection {
    pub rx: mpsc::UnboundedReceiver<Envelope>,
    pub bus_rx: tokio::sync::broadcast::Receiver<Envelope>,
}

/// Entry from the axum upgrade. `token` comes from ?token= (the client's
/// url param) — the axum handler extracts it from the query.
pub async fn handle(socket: WebSocket, state: State, token: Option<String>) {
    let token = match token {
        Some(t) if !t.is_empty() => t,
        _ => {
            destroy(socket, "not authenticated").await;
            return;
        }
    };

    let session = match state.session_by_token(&token).await {
        Some(s) => s,
        None => {
            destroy(socket, "session expired").await;
            return;
        }
    };

    if !session.profile.can_connect {
        destroy(socket, "connection disabled").await;
        return;
    }

    let session_id = session.id.clone();
    info!(%session_id, "websocket connection started");

    let rx = match state.connect_socket(&session_id).await {
        Ok(rx) => rx,
        Err(AuthError::ConnectionDisabled) => {
            destroy(socket, "already connected").await;
            return;
        }
        Err(_) => {
            destroy(socket, "session expired").await;
            return;
        }
    };

    // subscribe AFTER attach; init is queued on the session channel and the
    // writer is biased toward it, so system/init always precedes room events
    let bus_rx = state.bus().subscribe();
    let init = build_init(&state, &session_id).await;
    let init_env = Envelope {
        event: messages::server_events::SYSTEM_INIT.to_owned(),
        payload: serde_json::to_value(&init).unwrap_or_default(),
    };
    state.send_to(&session_id, &init_env).await;

    let heartbeat_secs = state
        .settings()
        .await
        .heartbeat_interval
        .unwrap_or(10)
        .max(1) as u64;

    let conn = Connection { rx, bus_rx };
    let (sink, stream) = socket.split();
    let writer = writer_task(sink, conn, heartbeat_secs);
    let reader = reader_task(stream, state.clone(), session_id.clone());

    // the reader drives termination; a clean close (1000/1001) drops the
    // session at once, anything else starts the merciful-reconnect grace
    let clean = tokio::select! {
        clean = reader => clean,
        _ = writer => false,
    };

    info!(%session_id, %clean, "websocket connection ended");
    state.disconnect_socket(&session_id, !clean).await;
}

async fn writer_task(
    mut sink: futures_util::stream::SplitSink<WebSocket, AxumMessage>,
    mut conn: Connection,
    heartbeat_secs: u64,
) {
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
    loop {
        tokio::select! {
            biased;
            // per-session unicast first: system/init and signalling
            Some(env) = conn.rx.recv() => {
                if send_envelope(&mut sink, env).await.is_err() { break; }
            }
            // room broadcast
            Ok(env) = conn.bus_rx.recv() => {
                if send_envelope(&mut sink, env).await.is_err() { break; }
            }
            _ = heartbeat.tick() => {
                let env = Envelope {
                    event: messages::server_events::SYSTEM_HEARTBEAT.to_owned(),
                    payload: serde_json::Value::Null,
                };
                if send_envelope(&mut sink, env).await.is_err() { break; }
            }
        }
    }
}

async fn send_envelope(
    sink: &mut futures_util::stream::SplitSink<WebSocket, AxumMessage>,
    env: Envelope,
) -> Result<(), ()> {
    let text = serde_json::to_string(&env).unwrap_or_default();
    sink.send(AxumMessage::Text(text)).await.map_err(|_| ())
}

/// Returns true when the socket closed cleanly (1000/1001) — no grace period.
async fn reader_task(
    mut stream: futures_util::stream::SplitStream<WebSocket>,
    state: State,
    session_id: String,
) -> bool {
    let mut clean = false;
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(AxumMessage::Text(text)) => {
                let Ok(env) = serde_json::from_str::<Envelope>(&text) else {
                    continue;
                };
                dispatch(&state, &session_id, env).await;
            }
            Ok(AxumMessage::Close(frame)) => {
                clean = matches!(
                    frame,
                    Some(CloseFrame { code: 1000 | 1001, .. })
                );
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    clean
}

/// Client -> server event dispatch. Media/signal events land in phase C;
/// plugin events in phase E.
async fn dispatch(state: &State, session_id: &str, env: Envelope) {
    use messages::client_events as ev;
    match env.event.as_str() {
        ev::HEARTBEAT => {}
        ev::CONTROL_REQUEST => state.control_request(session_id).await,
        ev::CONTROL_RELEASE => state.control_release(session_id).await,
        other => {
            debug!(%other, "unhandled client event (later phase)");
        }
    }
}

async fn build_init(state: &State, session_id: &str) -> messages::InitPayload {
    let settings = state.settings().await;
    let sessions = state.sessions_snapshot().await;
    let control_host = state.control_host().await;
    let screen_size = *state.inner.screen_size.read().await;

    // TODO(phase:capture): real screen size from X11; TODO(phase:webrtc): videos ids
    messages::InitPayload {
        session_id: session_id.to_owned(),
        control_host,
        screen_size,
        sessions,
        settings,
        touch_events: false,
        screencast_enabled: None,
        webrtc: Some(messages::WebrtcInfo {
            videos: Some(vec!["main".to_owned()]),
        }),
    }
}

/// Reject the socket before a session exists: one disconnect event + close.
async fn destroy(mut socket: WebSocket, message: &str) {
    warn!(%message, "websocket rejected");
    let env = Envelope {
        event: messages::server_events::SYSTEM_DISCONNECT.to_owned(),
        payload: serde_json::to_value(messages::DisconnectPayload {
            message: message.to_owned(),
        })
        .unwrap_or_default(),
    };
    let _ = socket
        .send(AxumMessage::Text(
            serde_json::to_string(&env).unwrap_or_default(),
        ))
        .await;
    let _ = socket
        .close(Some(CloseFrame {
            code: 1000,
            reason: message.to_owned().into(),
        }))
        .await;
}
