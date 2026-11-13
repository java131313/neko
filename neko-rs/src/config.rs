//! Environment-based configuration, mirroring the Go server's NEKO_* surface
//! (server/internal/config). Parity notes per field; details land with each
//! module's implementation phase.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub server: Server,
    pub member: Member,
    pub webrtc: WebRtc,
    pub capture: Capture,
    pub desktop: Desktop,
    pub room: RoomDefaults,
    pub plugins: Plugins,
}

#[derive(Debug, Clone)]
pub struct Server {
    /// NEKO_SERVER_BIND, default ":8080"
    pub bind: String,
    /// NEKO_SERVER_STATIC, the client dist (served at /)
    pub r#static: String,
    /// NEKO_SERVER_CERT/KEY — when both set, serve TLS
    pub cert: Option<String>,
    pub key: Option<String>,
    /// NEKO_SERVER_PROXY — trust X-Forwarded-For
    pub proxy: bool,
}

#[derive(Debug, Clone)]
pub struct Member {
    /// NEKO_MEMBER_MULTIUSER_USER_PASSWORD / ADMIN_PASSWORD
    /// (this fork's multiuser provider)
    pub user_password: String,
    pub admin_password: String,
    /// NEKO_MEMBER_PROVIDER — "multiuser" only in v1 (oauth/oidc later)
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct WebRtc {
    /// NEKO_WEBRTC_ICELITE
    pub ice_lite: bool,
    /// NEKO_WEBRTC_NAT1TO1 — comma separated IPs
    pub nat_1to1_ips: Vec<String>,
    /// NEKO_WEBRTC_EPR — no webrtc-rs equivalent; kept for the
    /// sysctl deployment contract (logged, ignored)
    pub ephemeral_min: u16,
    pub ephemeral_max: u16,
    /// NEKO_WEBRTC_TCPMUX/UDPMUX — unsupported by webrtc-rs v1
    pub tcp_mux: u16,
    pub udp_mux: u16,
    /// NEKO_WEBRTC_ICEBACKENDSTUN/TURN or ICESERVERS JSON — backend ICE servers
    pub ice_servers: Vec<IceServer>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

#[derive(Debug, Clone)]
pub struct Capture {
    /// NEKO_CAPTURE_VIDEO_DISPLAY (falls back to $DISPLAY)
    pub display: String,
    /// NEKO_CAPTURE_VIDEO_CODEC — vp8 (default) | h264
    pub video_codec: String,
    /// NEKO_CAPTURE_VIDEO_PIPELINE — full custom gst string; empty = assemble
    pub video_pipeline: String,
    /// NEKO_CAPTURE_VIDEO_IDS — "main" default
    pub video_ids: Vec<String>,
    pub video_show_pointer: bool,
    /// NEKO_CAPTURE_AUDIO_DEVICE
    pub audio_device: String,
    /// NEKO_CAPTURE_AUDIO_PIPELINE
    pub audio_pipeline: String,
    /// broadcast (RTMP)
    pub broadcast_url: String,
    pub broadcast_autostart: bool,
    pub broadcast_audio_bitrate: u32,
    pub broadcast_video_bitrate: u32,
    pub broadcast_preset: String,
    /// screencast (MJPEG over HTTP)
    pub screencast_enabled: bool,
    pub screencast_rate: u32,
    pub screencast_quality: u32,
}

#[derive(Debug, Clone)]
pub struct Desktop {
    /// NEKO_DESKTOP_SCREEN_SIZE / WIDTH / HEIGHT / RATE
    pub screen_width: u32,
    pub screen_height: u32,
    pub screen_rate: u32,
}

#[derive(Debug, Clone)]
pub struct RoomDefaults {
    pub private_mode: bool,
    pub locked_logins: bool,
    pub locked_controls: bool,
    pub control_protection: bool,
    pub implicit_hosting: bool,
    pub inactive_cursors: bool,
    pub merciful_reconnect: bool,
    pub heartbeat_interval: u32,
}

#[derive(Debug, Clone)]
pub struct Plugins {
    pub enabled: bool,
    pub dir: String,
    // chat
    pub chat_public_name_default: String,
    // filetransfer
    pub filetransfer_dir: String,
    pub filetransfer_path: String,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env(key).unwrap_or_else(|| default.to_owned())
}

fn env_bool(key: &str, default: bool) -> bool {
    env(key)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_csv(key: &str, default: &[&str]) -> Vec<String> {
    env(key)
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| default.iter().map(|s| s.to_string()).collect())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let display = env("NEKO_CAPTURE_VIDEO_DISPLAY")
            .or_else(|| std::env::var("DISPLAY").ok())
            .unwrap_or_default();

        // NEKO_WEBRTC_ICESERVERS: JSON array like pion's ICEServer
        let ice_servers = env("NEKO_WEBRTC_ICESERVERS")
            .map(|v| serde_json::from_str::<Vec<IceServer>>(&v).context("ICESERVERS json"))
            .transpose()?;

        // Convenience: NEKO_TURN_* synthesizes entries for the embedded
        // coturn (see neko-rs/turnserver.conf). NEKO_TURN_HOST is the
        // PUBLIC address browsers reach — required for the synthesis.
        let ice_servers = ice_servers.filter(|s: &Vec<IceServer>| !s.is_empty());
        let ice_servers = match ice_servers {
            Some(servers) => servers,
            None => std::env::var("NEKO_TURN_USERNAME")
                .ok()
                .filter(|v| !v.is_empty())
                .and_then(|username| {
                    let host = env("NEKO_TURN_HOST")?;
                    let port = env("NEKO_TURN_PORT").unwrap_or_else(|| "3478".into());
                    Some(vec![IceServer {
                        urls: vec![
                            format!("turn:{host}:{port}?transport=udp"),
                            format!("turn:{host}:{port}?transport=tcp"),
                        ],
                        username,
                        credential: env("NEKO_TURN_PASSWORD").unwrap_or_else(|| "neko".into()),
                    }])
                })
                .unwrap_or_default(),
        };

        Ok(Config {
            server: Server {
                bind: env_or("NEKO_SERVER_BIND", ":8080"),
                r#static: env_or("NEKO_SERVER_STATIC", "/var/www"),
                cert: env("NEKO_SERVER_CERT"),
                key: env("NEKO_SERVER_KEY"),
                proxy: env_bool("NEKO_SERVER_PROXY", false),
            },
            member: Member {
                provider: env_or("NEKO_MEMBER_PROVIDER", "multiuser"),
                // Go defaults both to "neko" with a warning; hard-failing
                // here leaves the container dead on platforms that drop
                // env vars, so match the embedded behavior.
                user_password: env("NEKO_MEMBER_MULTIUSER_USER_PASSWORD").unwrap_or_else(|| {
                    tracing::warn!("NEKO_MEMBER_MULTIUSER_USER_PASSWORD not set, using 'neko'");
                    "neko".to_owned()
                }),
                admin_password: env("NEKO_MEMBER_MULTIUSER_ADMIN_PASSWORD").unwrap_or_else(|| {
                    tracing::warn!("NEKO_MEMBER_MULTIUSER_ADMIN_PASSWORD not set, using 'neko'");
                    "neko".to_owned()
                }),
            },
            webrtc: WebRtc {
                ice_lite: env_bool("NEKO_WEBRTC_ICELITE", false),
                nat_1to1_ips: env_csv("NEKO_WEBRTC_NAT1TO1", &[]),
                ephemeral_min: env_u32("NEKO_WEBRTC_EPR_MIN", 0) as u16,
                ephemeral_max: env_u32("NEKO_WEBRTC_EPR_MAX", 0) as u16,
                tcp_mux: env_u32("NEKO_WEBRTC_TCPMUX", 0) as u16,
                udp_mux: env_u32("NEKO_WEBRTC_UDPMUX", 0) as u16,
                ice_servers,
            },
            capture: Capture {
                display,
                video_codec: env_or("NEKO_CAPTURE_VIDEO_CODEC", "vp8"),
                video_pipeline: env_or("NEKO_CAPTURE_VIDEO_PIPELINE", ""),
                video_ids: env_csv("NEKO_CAPTURE_VIDEO_IDS", &["main"]),
                video_show_pointer: env_bool("NEKO_CAPTURE_VIDEO_SHOW_POINTER", false),
                audio_device: env_or("NEKO_CAPTURE_AUDIO_DEVICE", ""),
                audio_pipeline: env_or("NEKO_CAPTURE_AUDIO_PIPELINE", ""),
                broadcast_url: env_or("NEKO_CAPTURE_BROADCAST_URL", ""),
                broadcast_autostart: env_bool("NEKO_CAPTURE_BROADCAST_AUTOSTART", false),
                broadcast_audio_bitrate: env_u32("NEKO_CAPTURE_BROADCAST_AUDIO_BITRATE", 128),
                broadcast_video_bitrate: env_u32("NEKO_CAPTURE_BROADCAST_VIDEO_BITRATE", 3072),
                broadcast_preset: env_or("NEKO_CAPTURE_BROADCAST_PRESET", "veryfast"),
                screencast_enabled: env_bool("NEKO_CAPTURE_SCREENCAST_ENABLED", false),
                screencast_rate: env_u32("NEKO_CAPTURE_SCREENCAST_RATE", 30),
                screencast_quality: env_u32("NEKO_CAPTURE_SCREENCAST_QUALITY", 70),
            },
            desktop: Desktop {
                screen_width: env_u32("NEKO_DESKTOP_SCREEN_WIDTH", 1920),
                screen_height: env_u32("NEKO_DESKTOP_SCREEN_HEIGHT", 1080),
                screen_rate: env_u32("NEKO_DESKTOP_SCREEN_RATE", 30),
            },
            room: RoomDefaults {
                private_mode: env_bool("NEKO_ROOM_PRIVATE_MODE", false),
                locked_logins: env_bool("NEKO_ROOM_LOCKED_LOGINS", false),
                locked_controls: env_bool("NEKO_ROOM_LOCKED_CONTROLS", false),
                control_protection: env_bool("NEKO_ROOM_CONTROL_PROTECTION", false),
                implicit_hosting: env_bool("NEKO_ROOM_IMPLICIT_HOSTING", false),
                inactive_cursors: env_bool("NEKO_ROOM_INACTIVE_CURSORS", false),
                merciful_reconnect: env_bool("NEKO_ROOM_MERCIFUL_RECONNECT", true),
                heartbeat_interval: env_u32("NEKO_ROOM_HEARTBEAT_INTERVAL", 10),
            },
            plugins: Plugins {
                enabled: env_bool("NEKO_PLUGINS_ENABLED", false),
                dir: env_or("NEKO_PLUGINS_DIR", "/etc/neko/plugins/"),
                chat_public_name_default: env_or(
                    "NEKO_PLUGINS_CHAT_PUBLIC_NAME_DEFAULT",
                    "Chat",
                ),
                filetransfer_dir: env_or("NEKO_PLUGINS_FILETRANSFER_DIR", "/uploads"),
                filetransfer_path: env_or(
                    "NEKO_PLUGINS_FILETRANSFER_PUBLIC_PATH",
                    "/filetransfer",
                ),
            },
        })
    }
}
