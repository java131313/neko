//! HTTP API. Endpoints per server/openapi.yaml; the client reads a subset
//! (whoami/login/logout/profile/screen/clipboard/broadcast/upload...), the
//! admin UI uses the rest. Implemented in implementation phases; the router
//! wires what exists and returns 501 for the rest until then.

use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::room::State;

pub fn router(state: State) -> Router {
    let api = Router::new()
        .route("/whoami", get(not_implemented).post(not_implemented))
        .route("/login", get(not_implemented).post(not_implemented))
        .route("/logout", get(not_implemented).post(not_implemented))
        .route("/profile", get(not_implemented).post(not_implemented))
        .route("/sessions", get(not_implemented))
        .route("/stats", get(not_implemented))
        .route(
            "/room/settings",
            get(not_implemented).post(not_implemented),
        )
        .route(
            "/room/control",
            get(not_implemented).post(not_implemented),
        )
        .route("/room/screen", get(not_implemented).post(not_implemented))
        .route("/room/clipboard", get(not_implemented).post(not_implemented));

    Router::new()
        .route("/health", get(health))
        .route("/api/ws", get(ws_upgrade))
        .nest("/api", api)
        .fallback_service(tower_http::services::ServeDir::new(
            state.inner.config.server.r#static.clone(),
        ))
        .with_state(state)
}

async fn health() -> &'static str {
    "OK"
}

async fn not_implemented() -> impl IntoResponse {
    (StatusCode::NOT_IMPLEMENTED, "not implemented yet")
}

use axum::extract::WebSocketUpgrade;
use axum::response::Response;

/// GET /api/ws — upgraded in phase:room.
async fn ws_upgrade(
    AxumState(_state): AxumState<State>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(|_socket| async {
        let _ = crate::ws::handle_ws().await;
    })
}
