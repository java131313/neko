//! Room state: sessions, host, settings, locks. Port of
//! server/internal/websocket/manager + server/internal/session semantics.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Config;
use crate::messages::{ControlHost, MemberProfile, ScreenSize, Session, SessionState, Settings};

/// Shared server state, handed to axum handlers and the ws loop.
#[derive(Clone)]
pub struct State {
    inner: Arc<Inner>,
}

pub struct Inner {
    pub config: Config,
    pub sessions: RwLock<HashMap<String, SessionEntry>>,
    pub host: RwLock<Option<String>>,
    pub settings: RwLock<Settings>,
    pub screen_size: RwLock<ScreenSize>,
}

pub struct SessionEntry {
    pub session: Session,
    /// live websocket sender, when connected
    pub socket: Option<crate::ws::Socket>,
}

impl State {
    pub fn new(config: Config) -> Self {
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
        State {
            inner: Arc::new(Inner {
                config,
                sessions: RwLock::new(HashMap::new()),
                host: RwLock::new(None),
                settings: RwLock::new(settings),
                screen_size: RwLock::new(screen_size),
            }),
        }
    }

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
}

/// Default profile for the multiuser provider (admin/user split by password).
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

pub fn new_session(id: String, is_admin: bool) -> Session {
    Session {
        id,
        profile: profile_for(is_admin),
        state: SessionState {
            is_connected: true,
            connected_since: Some(chrono_now()),
            not_connected_since: None,
            is_watching: false,
            watching_since: None,
            not_watching_since: None,
        },
    }
}

fn chrono_now() -> String {
    // rfc3339, seconds precision is enough for since markers
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_rfc3339(secs)
}

fn format_rfc3339(secs: i64) -> String {
    // civil-from-days algorithm, no chrono dependency for one timestamp
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
        // 2025-11-13T02:08:00Z
        let secs = 1_735_689_600 + 316 * 86_400 + 7_680;
        assert_eq!(format_rfc3339(secs), "2025-11-13T02:08:00Z");
    }
}
