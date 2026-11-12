mod pb;
mod media;
mod peer;
mod service;

use std::path::PathBuf;

use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::pb::dataplane_server::DataplaneServer;
use crate::service::Service;

/// Address of the control plane <-> dataplane Unix socket.
///
/// Accepts a plain path or a `unix://` URL. Configured by the control
/// plane via `NEKO_WEBRTC_DATAPLANE` (both sides must agree).
fn listen_path() -> PathBuf {
    let raw = std::env::var("NEKO_DATAPLANE_LISTEN")
        .unwrap_or_else(|_| "/run/neko/dataplane.sock".to_string());

    let path = raw.strip_prefix("unix://").unwrap_or(&raw);

    PathBuf::from(path)
}

fn init_logging() {
    let json = matches!(
        std::env::var("NEKO_LOG_JSON").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );

    let filter =
        EnvFilter::try_from_env("NEKO_LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));

    if json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .compact()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    gstreamer::init()?;

    let path = listen_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Remove a stale socket from a previous run; ignore if absent.
    if let Err(err) = std::fs::remove_file(&path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            error!(?err, "failed to remove stale socket");
        }
    }

    let listener = UnixListener::bind(&path)?;
    info!(path = %path.display(), "neko-dataplane listening");

    let service = Service::new();

    // Clone into an Arc for the shutdown task and any future use; tonic
    // wraps the service in its own Arc internally.
    let service = std::sync::Arc::new(service);

    // Shut down cleanly on SIGTERM (supervisord stop) and Ctrl-C.
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        info!("shutting down");
    };

    tonic::transport::Server::builder()
        .add_service(DataplaneServer::from_arc(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown)
        .await?;

    // The socket file is unlinked on a clean exit; the remove at startup
    // covers unclean ones.
    if let Err(err) = std::fs::remove_file(&path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            error!(?err, "failed to remove socket on exit");
        }
    }

    Ok(())
}
