//! neko-rs: the neko server in Rust.
//!
//! One process replaces the Go `neko serve`: HTTP + WebSocket control plane,
//! session/host management, GStreamer capture, WebRTC, X11 desktop control.
//! The web client (client/) is unchanged — this binary speaks the v3 API.

mod api;
mod capture;
mod config;
mod desktop;
mod messages;
mod payload;
mod plugins;
mod room;
mod uid;
mod webrtc;
mod ws;

use anyhow::{Context, Result};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let config = config::Config::from_env().context("read configuration")?;
    info!(bind = %config.server.bind, "neko-rs starting");

    // gstreamer must be initialized once before any pipeline exists
    gstreamer::init().context("gstreamer init")?;

    // peer events (ICE candidates, connection state, data channel input)
    // forward into session sockets; the task lives as long as the server
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = room::State::new(config.clone(), events_tx);
    tokio::spawn(webrtc::forward_events(events_rx, state.clone()));

    // TODO(phase:desktop): open X11 connection, screen size, clipboard watch

    let app = api::router(state.clone());

    let bind = &config.server.bind;
    let listener = tokio::net::TcpListener::bind(normalize_bind(bind))
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, "listening");
    axum::serve(listener, app).await.context("http server")
}

/// ":8080" (go style) -> "0.0.0.0:8080"
fn normalize_bind(bind: &str) -> String {
    if let Some(port) = bind.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        bind.to_owned()
    }
}

fn init_logging() {
    let filter = std::env::var("NEKO_LOG_LEVEL").unwrap_or_else(|_| "info".to_owned());
    let json = matches!(
        std::env::var("NEKO_LOG_JSON").map(|v| v.to_lowercase()).as_deref(),
        Ok("1") | Ok("true")
    );
    let env_filter = tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_default();
    if json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}
