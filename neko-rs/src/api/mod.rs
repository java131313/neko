//! HTTP API. Endpoints per server/openapi.yaml; phase B implements the
//! session/room core the client needs to enter a room. Everything else
//! stays 501 until its phase.

use axum::extract::{Query, State as AxumState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::messages::{self, Envelope};
use crate::room::State;

pub fn router(state: State) -> Router {
    let api = Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/whoami", get(whoami))
        .route("/sessions", get(sessions))
        .route("/room/settings", get(room_settings).post(set_room_settings))
        .route("/room/screen", get(screen))
        .route("/room/control/request", post(control_request))
        .route("/room/control/release", post(control_release));

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

// ---- auth ----

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Session token: Authorization: Bearer, or ?token= for endpoints the
/// browser reaches outside fetch (websocket, filetransfer downloads).
fn extract_token(headers: &HeaderMap, query: &Option<TokenQuery>) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value.to_str().unwrap_or_default();
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.to_owned());
        }
    }
    query
        .as_ref()
        .and_then(|q| q.token.clone())
        .filter(|t| !t.is_empty())
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

fn session_json(
    session: &messages::Session,
    token: Option<&str>,
) -> Value {
    let mut v = serde_json::to_value(session).unwrap_or_default();
    if let Some(token) = token {
        v["token"] = json!(token);
    }
    v
}

// ---- endpoints ----

async fn login(
    AxumState(state): AxumState<State>,
    Json(body): Json<LoginRequest>,
) -> Response {
    // cookie mode exists upstream (NEKO_SERVER_COOKIE); this fork's client
    // always takes the token, so token is always returned
    match state.login(&body.username, &body.password).await {
        Ok((session, token)) => Json(session_json(&session, Some(&token))).into_response(),
        Err(crate::room::AuthError::InvalidPassword) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "invalid password" })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": err.to_string() })),
        )
            .into_response(),
    }
}

async fn logout(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
) -> StatusCode {
    if let Some(token) = extract_token(&headers, &None) {
        state.logout(&token).await;
    }
    StatusCode::OK
}

async fn whoami(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    let Some(token) = extract_token(&headers, &Some(query)) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "unauthorized" })),
        )
            .into_response();
    };
    match state.session_by_token(&token).await {
        Some(session) => Json(session_json(&session, None)).into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "unauthorized" })),
        )
            .into_response(),
    }
}

async fn sessions(AxumState(state): AxumState<State>) -> Json<Value> {
    // TODO(phase:E): private_mode filters non-admins to themselves
    let sessions = state.sessions_snapshot().await;
    Json(serde_json::to_value(sessions).unwrap_or_default())
}

async fn room_settings(AxumState(state): AxumState<State>) -> Json<messages::Settings> {
    Json(state.settings().await)
}

async fn set_room_settings(
    AxumState(state): AxumState<State>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // admin only; merges the posted flags into current settings
    let Some(token) = extract_token(&headers, &None) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(session) = state.session_by_token(&token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !session.profile.is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut settings = state.settings().await;
    merge_settings(&mut settings, &body);
    *state.inner.settings.write().await = settings;

    // system/settings carries the changer id plus the new flags
    let mut payload = body;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("id".to_owned(), json!(session.id));
    }
    state.broadcast(&Envelope {
        event: messages::server_events::SYSTEM_SETTINGS.to_owned(),
        payload,
    });
    StatusCode::OK.into_response()
}

fn merge_settings(settings: &mut messages::Settings, body: &Value) {
    macro_rules! flag {
        ($field:ident) => {
            if let Some(v) = body[stringify!($field)].as_bool() {
                settings.$field = v;
            }
        };
    }
    flag!(private_mode);
    flag!(locked_logins);
    flag!(locked_controls);
    flag!(control_protection);
    flag!(implicit_hosting);
    flag!(inactive_cursors);
    flag!(merciful_reconnect);
    if let Some(v) = body["heartbeat_interval"].as_u64() {
        settings.heartbeat_interval = Some(v as u32);
    }
}

async fn screen(AxumState(state): AxumState<State>) -> Json<messages::ScreenSize> {
    Json(*state.inner.screen_size.read().await)
}

async fn control_request(AxumState(state): AxumState<State>, headers: HeaderMap) -> StatusCode {
    let Some(token) = extract_token(&headers, &None) else {
        return StatusCode::UNAUTHORIZED;
    };
    match state.session_by_token(&token).await {
        Some(s) => {
            state.control_request(&s.id).await;
            StatusCode::OK
        }
        None => StatusCode::UNAUTHORIZED,
    }
}

async fn control_release(AxumState(state): AxumState<State>, headers: HeaderMap) -> StatusCode {
    let Some(token) = extract_token(&headers, &None) else {
        return StatusCode::UNAUTHORIZED;
    };
    match state.session_by_token(&token).await {
        Some(s) => {
            state.control_release(&s.id).await;
            StatusCode::OK
        }
        None => StatusCode::UNAUTHORIZED,
    }
}

// ---- websocket upgrade ----

use axum::extract::WebSocketUpgrade;

async fn ws_upgrade(
    AxumState(state): AxumState<State>,
    Query(query): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| crate::ws::handle(socket, state, query.token))
}
