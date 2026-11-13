//! WebSocket event protocol (`/api/ws`, JSON `{event, payload}`), ported
//! from server/pkg/types/message + verified against client/core/src/client.ts.
//! Plugin events (chat/*, filetransfer/*) pass through as raw JSON values.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The envelope every websocket message travels in, both directions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub event: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberProfile {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub is_admin: bool,
    pub can_login: bool,
    pub can_connect: bool,
    pub can_watch: bool,
    pub can_host: bool,
    pub can_share_media: bool,
    pub can_access_clipboard: bool,
    pub sends_inactive_cursor: bool,
    pub can_see_inactive_cursors: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub is_connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_connected_since: Option<String>,
    pub is_watching: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watching_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_watching_since: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub profile: MemberProfile,
    pub state: SessionState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub private_mode: bool,
    pub locked_logins: bool,
    pub locked_controls: bool,
    pub control_protection: bool,
    pub implicit_hosting: bool,
    pub inactive_cursors: bool,
    pub merciful_reconnect: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_interval: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
    pub rate: u32,
}

// ---- server -> client ----

#[derive(Debug, Clone, Serialize)]
pub struct InitPayload {
    pub session_id: String,
    pub control_host: ControlHost,
    pub screen_size: ScreenSize,
    pub sessions: HashMap<String, Session>,
    pub settings: Settings,
    pub touch_events: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screencast_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webrtc: Option<WebrtcInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlHost {
    pub has_host: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebrtcInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub videos: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_sizes_list: Option<Vec<ScreenSize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcast_status: Option<BroadcastStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BroadcastStatus {
    pub is_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// session/created, session/profile, session/state payloads: the session
/// itself, with `id` embedded.
pub type SessionEvent = Session;

#[derive(Debug, Clone, Serialize)]
pub struct SessionDeleted {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostPayload {
    pub id: String,
    pub has_host: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenUpdated {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub rate: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipboardUpdated {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SentPayload {
    pub id: String, // sender
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalProvide {
    pub sdp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iceservers: Option<Value>,
    pub video: String,
    pub audio: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalSdp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub sdp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalCandidate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub candidate: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalClose {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisconnectPayload {
    pub message: String,
}

// ---- client -> server ----

#[derive(Debug, Clone, Deserialize)]
pub struct SignalRequest {
    #[serde(default)]
    pub video: Value,
    #[serde(default)]
    pub audio: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSettings {
    pub id: String,
    #[serde(flatten)]
    pub settings: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PastePayload {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClipboardSet {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyboardMap {
    pub layout: String,
    #[serde(default)]
    pub variant: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScreenSet {
    pub width: u32,
    pub height: u32,
    pub rate: u32,
}

/// The ws-fallback input events (control/*) — same semantics as the data
/// channel binary ops; client/core/src/input/websocket.ts.
#[derive(Debug, Clone, Deserialize)]
pub struct ScrollEvent {
    pub delta_x: i32,
    pub delta_y: i32,
    #[serde(default)]
    pub control_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyEvent {
    pub keysym: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ButtonEvent {
    pub code: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TouchEvent {
    pub touch_id: u32,
    pub x: i32,
    pub y: i32,
    pub pressure: u8,
}

/// Known client -> server event names (plugins extend this at runtime).
pub mod client_events {
    pub const HEARTBEAT: &str = "client/heartbeat";
    pub const CONTROL_REQUEST: &str = "control/request";
    pub const CONTROL_RELEASE: &str = "control/release";
    pub const CONTROL_SCROLL: &str = "control/scroll";
    pub const CONTROL_KEY_DOWN: &str = "control/keydown";
    pub const CONTROL_KEY_UP: &str = "control/keyup";
    pub const CONTROL_BUTTON_DOWN: &str = "control/buttondown";
    pub const CONTROL_BUTTON_UP: &str = "control/buttonup";
    pub const CONTROL_TOUCH_BEGIN: &str = "control/touchbegin";
    pub const CONTROL_TOUCH_UPDATE: &str = "control/touchupdate";
    pub const CONTROL_TOUCH_END: &str = "control/touchend";
    pub const CONTROL_PASTE: &str = "control/paste";
    pub const CLIPBOARD_SET: &str = "clipboard/set";
    pub const KEYBOARD_MAP: &str = "keyboard/map";
    pub const SCREEN_SET: &str = "screen/set";
    pub const SEND_BROADCAST: &str = "send/broadcast";
    pub const SEND_UNICAST: &str = "send/unicast";
    pub const SIGNAL_REQUEST: &str = "signal/request";
    pub const SIGNAL_OFFER: &str = "signal/offer";
    pub const SIGNAL_ANSWER: &str = "signal/answer";
    pub const SIGNAL_CANDIDATE: &str = "signal/candidate";
}

/// Known server -> client event names.
pub mod server_events {
    pub const SYSTEM_INIT: &str = "system/init";
    pub const SYSTEM_ADMIN: &str = "system/admin";
    pub const SYSTEM_SETTINGS: &str = "system/settings";
    pub const SYSTEM_DISCONNECT: &str = "system/disconnect";
    pub const SYSTEM_HEARTBEAT: &str = "system/heartbeat";
    pub const SESSION_CREATED: &str = "session/created";
    pub const SESSION_DELETED: &str = "session/deleted";
    pub const SESSION_PROFILE: &str = "session/profile";
    pub const SESSION_STATE: &str = "session/state";
    pub const SESSION_CURSORS: &str = "session/cursors";
    pub const CONTROL_HOST: &str = "control/host";
    pub const CONTROL_REQUEST: &str = "control/request";
    pub const SCREEN_UPDATED: &str = "screen/updated";
    pub const CLIPBOARD_UPDATED: &str = "clipboard/updated";
    pub const BROADCAST_STATUS: &str = "broadcast/status";
    pub const SEND_UNICAST: &str = "send/unicast";
    pub const SEND_BROADCAST: &str = "send/broadcast";
    pub const SIGNAL_PROVIDE: &str = "signal/provide";
    pub const SIGNAL_OFFER: &str = "signal/offer";
    pub const SIGNAL_ANSWER: &str = "signal/answer";
    pub const SIGNAL_CANDIDATE: &str = "signal/candidate";
    pub const SIGNAL_CLOSE: &str = "signal/close";
}

/// Serialize an outgoing event into the wire envelope.
pub fn outgoing<E: Serialize>(event: &str, payload: &E) -> Envelope {
    Envelope {
        event: event.to_owned(),
        payload: serde_json::to_value(payload).unwrap_or(Value::Null),
    }
}
