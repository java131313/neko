//! Room state: sessions, host, settings, broadcast bus, grace timers.
//! Port of server/internal/session + the room pieces of
//! server/internal/websocket (manager + handler control semantics).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::info;

use crate::config::Config;
use crate::messages::{self, ControlHost, Envelope, MemberProfile, ScreenSize, Session, SessionState, Settings};
use crate::uid;

/// Abnormal websocket closures keep the session (and host) alive this long,
/// awaiting a reconnect — the client's normal-close comment says 5 s, and
/// merciful_reconnect governs whether replacement/altogether is allowed.
const GRACE_PERIOD: Duration = Duration::from_secs(5);

const BUS_CAP: usize = 256;

/// Shared server state, handed to axum handlers and the ws loop.
#[derive(Clone)]
pub struct State {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: Config,
    pub sessions: RwLock<HashMap<String, SessionEntry>>,
    pub tokens: RwLock<HashMap<String, String>>, // token -> session id
    pub host: RwLock<Option<String>>,
    pub settings: RwLock<Settings>,
    pub screen_size: RwLock<ScreenSize>,
    /// room-wide events (session/created, control/host, clipboard, ...)
    pub bus: broadcast::Sender<Envelope>,
    /// webrtc peer manager (phase C)
    pub webrtc: crate::webrtc::Manager,
}

pub struct SessionEntry {
    pub session: Session,
    pub token: String,
    /// live websocket writer, when connected
    pub socket: Option<mpsc::UnboundedSender<Envelope>>,
    /// set when an abnormal close started the grace timer
    pub grace: Option<tokio::task::AbortHandle>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid password")]
    InvalidPassword,
    #[error("not authenticated")]
    Unauthenticated,
    #[error("connection disabled")]
    ConnectionDisabled,
    #[error("session expired")]
    SessionExpired,
}

impl State {
    pub fn new(config: Config, webrtc_events: tokio::sync::mpsc::UnboundedSender<crate::webrtc::PeerEvent>) -> Self {
        let settings = Settings {
            private_mode: config.room.private_mode,
            locked_logins: config.room.locked_logins,
            locked_controls: config.room.locked_controls,
            control_protection: config.room.control_protection,
            implicit_hosting: config.room.implicit_hosting,
            inactive_cursors: config.room.inactive_cursors,
            merciful_reconnect: config.room.merciful_reconnect,
            heartbeat_interval: Some(config.room.heartbeat_interval),
            plugins: None,
        };
        let screen_size = ScreenSize {
            width: config.desktop.screen_width,
            height: config.desktop.screen_height,
            rate: config.desktop.screen_rate,
        };
        let (bus, _) = broadcast::channel(BUS_CAP);
        let webrtc = crate::webrtc::Manager::new(config.clone(), webrtc_events);
        State {
            inner: Arc::new(Inner {
                config,
                sessions: RwLock::new(HashMap::new()),
                tokens: RwLock::new(HashMap::new()),
                host: RwLock::new(None),
                settings: RwLock::new(settings),
                screen_size: RwLock::new(screen_size),
                bus,
                webrtc,
            }),
        }
    }

    pub fn bus(&self) -> broadcast::Sender<Envelope> {
        self.inner.bus.clone()
    }

    /// POST /api/login — multiuser provider: the password picks the profile,
    /// the username is the display name and the id's stem.
    pub async fn login(&self, username: &str, password: &str) -> Result<(Session, String), AuthError> {
        let config = &self.inner.config;
        let is_admin = if !config.member.admin_password.is_empty()
            && password == config.member.admin_password
        {
            true
        } else if !config.member.user_password.is_empty()
            && password == config.member.user_password
        {
            false
        } else {
            return Err(AuthError::InvalidPassword);
        };

        let id = format!("{}-{}", uid::safe_id(username), uid::new_uid(5));
        let mut profile = profile_for(is_admin);
        profile.name = if username.is_empty() {
            profile.name
        } else {
            username.to_owned()
        };

        let session = new_session(id.clone(), profile);
        let token = uid::new_uid(64);

        let mut sessions = self.inner.sessions.write().await;
        sessions.insert(
            id.clone(),
            SessionEntry {
                session: session.clone(),
                token: token.clone(),
                socket: None,
                grace: None,
            },
        );
        drop(sessions);
        self.inner.tokens.write().await.insert(token.clone(), id.clone());

        info!(session_id = %id, is_admin, %username, "session created");
        self.broadcast(&messages::outgoing(
            messages::server_events::SESSION_CREATED,
            &session,
        ));

        Ok((session, token))
    }

    pub async fn session_by_token(&self, token: &str) -> Option<Session> {
        let id = self.inner.tokens.read().await.get(token)?.clone();
        self.inner
            .sessions
            .read()
            .await
            .get(&id)
            .map(|e| e.session.clone())
    }

    pub async fn session_by_id(&self, id: &str) -> Option<Session> {
        self.inner
            .sessions
            .read()
            .await
            .get(id)
            .map(|e| e.session.clone())
    }

    /// Attach a live socket to the session; returns the receiver the ws
    /// writer task pumps. Merciful reconnect replaces an existing socket.
    pub async fn connect_socket(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<Envelope>, AuthError> {
        // implicit hosting: a newly connected session with hosting rights
        // takes an empty driver's seat
        {
            let settings = self.inner.settings.read().await;
            let implicit = settings.implicit_hosting;
            drop(settings);
            if implicit {
                let mut host = self.inner.host.write().await;
                if host.is_none() {
                    let can_host = self
                        .inner
                        .sessions
                        .read()
                        .await
                        .get(session_id)
                        .map(|e| e.session.profile.can_host)
                        .unwrap_or(false);
                    if can_host {
                        *host = Some(session_id.to_owned());
                    }
                }
            }
        }

        let mut sessions = self.inner.sessions.write().await;
        let entry = sessions
            .get_mut(session_id)
            .ok_or(AuthError::SessionExpired)?;

        if entry.socket.is_some() && !self.inner.config.room.merciful_reconnect {
            return Err(AuthError::ConnectionDisabled);
        }

        // cancel any pending grace disconnect
        if let Some(grace) = entry.grace.take() {
            grace.abort();
        }

        let (tx, rx) = mpsc::unbounded_channel();
        entry.socket = Some(tx);
        entry.session.state.is_connected = true;
        entry.session.state.connected_since = Some(now_rfc3339());
        entry.session.state.not_connected_since = None;

        Ok(rx)
    }

    /// Normal close: session drops immediately (unless a grace timer from an
    /// earlier abnormal close is pending — close_path handles that).
    pub async fn disconnect_socket(&self, session_id: &str, delayed: bool) {
        if delayed {
            let state = self.clone();
            let id = session_id.to_owned();
            let mut sessions = self.inner.sessions.write().await;
            if let Some(entry) = sessions.get_mut(session_id) {
                let handle = tokio::spawn(async move {
                    tokio::time::sleep(GRACE_PERIOD).await;
                    state.expire_session(&id).await;
                });
                entry.grace = Some(handle.abort_handle());
                entry.session.state.is_connected = false;
                entry.session.state.not_connected_since = Some(now_rfc3339());
                // the socket itself is already gone (reader loop ended)
                entry.socket = None;
            }
            return;
        }
        self.expire_session(session_id).await;
    }

    /// Grace expired or clean logout: session (and host) are gone.
    pub async fn expire_session(&self, session_id: &str) {
        let removed = self.inner.sessions.write().await.remove(session_id);
        if removed.is_none() {
            return;
        }
        self.inner.tokens.write().await.retain(|_, id| id != session_id);

        let mut host = self.inner.host.write().await;
        let was_host = *host == Some(session_id.to_owned());
        if was_host {
            *host = None;
        }
        drop(host);

        if was_host {
            self.broadcast(&messages::outgoing(
                messages::server_events::CONTROL_HOST,
                &messages::HostPayload {
                    id: session_id.to_owned(),
                    has_host: false,
                    host_id: None,
                },
            ));
        }

        self.broadcast(&messages::outgoing(
            messages::server_events::SESSION_DELETED,
            &messages::SessionDeleted {
                id: session_id.to_owned(),
            },
        ));
        self.inner.webrtc.destroy_peer(session_id).await;
        info!(session_id, "session expired");
    }

    pub async fn logout(&self, token: &str) {
        let id = self.inner.tokens.read().await.get(token).cloned();
        if let Some(id) = id {
            self.expire_session(&id).await;
        }
    }

    // ---- host ----

    pub async fn control_host(&self) -> ControlHost {
        match self.inner.host.read().await.as_ref() {
            Some(id) => ControlHost {
                has_host: true,
                host_id: Some(id.clone()),
            },
            None => ControlHost {
                has_host: false,
                host_id: None,
            },
        }
    }

    /// control/request: take the seat when free or unprotected; otherwise
    /// ask the current host (control/request event to the host only).
    pub async fn control_request(&self, session_id: &str) {
        let settings = self.inner.settings.read().await.clone();
        let host = self.inner.host.read().await.clone();

        match host {
            None => self.set_host(Some(session_id.to_owned()), session_id).await,
            Some(current) if current == session_id => {}
            Some(current) => {
                if settings.control_protection {
                    // ask the host; they release or ignore
                    self.send_to(
                        &current,
                        &messages::outgoing(
                            messages::server_events::CONTROL_REQUEST,
                            &serde_json::json!({ "id": session_id }),
                        ),
                    )
                    .await;
                } else {
                    self.set_host(Some(session_id.to_owned()), session_id).await;
                }
            }
        }
    }

    pub async fn control_release(&self, session_id: &str) {
        let host = self.inner.host.read().await.clone();
        if host.as_deref() == Some(session_id) {
            self.set_host(None, session_id).await;
        }
    }

    async fn set_host(&self, new_host: Option<String>, by: &str) {
        *self.inner.host.write().await = new_host.clone();
        self.broadcast(&messages::outgoing(
            messages::server_events::CONTROL_HOST,
            &messages::HostPayload {
                id: by.to_owned(),
                has_host: new_host.is_some(),
                host_id: new_host,
            },
        ));
        let current = self.inner.host.read().await.clone();
        info!(host = ?current, by, "host changed");
    }

    pub async fn host_is(&self, session_id: &str) -> bool {
        self.inner.host.read().await.as_deref() == Some(session_id)
    }

    // ---- messaging ----

    pub fn broadcast(&self, envelope: &Envelope) {
        let _ = self.inner.bus.send(envelope.clone());
    }

    /// unicast to one session's live socket; false when not connected.
    pub async fn send_to(&self, session_id: &str, envelope: &Envelope) -> bool {
        let sessions = self.inner.sessions.read().await;
        match sessions.get(session_id).and_then(|e| e.socket.as_ref()) {
            Some(tx) => tx.send(envelope.clone()).is_ok(),
            None => false,
        }
    }

    pub async fn sessions_snapshot(&self) -> HashMap<String, Session> {
        self.inner
            .sessions
            .read()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), e.session.clone()))
            .collect()
    }

    pub async fn settings(&self) -> Settings {
        self.inner.settings.read().await.clone()
    }
}

/// Default profile for the multiuser provider.
pub fn profile_for(is_admin: bool) -> MemberProfile {
    MemberProfile {
        name: if is_admin {
            "Administrator".to_owned()
        } else {
            "User".to_owned()
        },
        avatar: None,
        is_admin,
        can_login: true,
        can_connect: true,
        can_watch: true,
        can_host: true,
        can_share_media: true,
        can_access_clipboard: true,
        sends_inactive_cursor: false,
        can_see_inactive_cursors: false,
        plugins: None,
    }
}

pub fn new_session(id: String, profile: MemberProfile) -> Session {
    Session {
        id,
        profile,
        state: SessionState {
            is_connected: true,
            connected_since: Some(now_rfc3339()),
            not_connected_since: None,
            is_watching: false,
            watching_since: None,
            not_watching_since: None,
        },
    }
}

pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_rfc3339(secs)
}

pub fn format_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::format_rfc3339;

    #[test]
    fn rfc3339_epoch() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_known() {
        let secs = 1_735_689_600 + 316 * 86_400 + 7_680;
        assert_eq!(format_rfc3339(secs), "2025-11-13T02:08:00Z");
    }
}
