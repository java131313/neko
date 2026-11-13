# neko-rs

The neko server rewritten in Rust — one process: HTTP + WebSocket control
plane, sessions/host management, GStreamer capture, WebRTC, X11 desktop
control. The web client (`client/`, Vue/TS) is **unchanged**: this binary
speaks the exact v3 API the client core (`client/core/`) expects.

Supersedes `server/` (Go) and folds `dataplane/` in-process (the gRPC/UDS
split goes away; `media.rs`/`peer.rs`/`webshell.rs` are vendored and adapted).

## The wire contract (verified against client/core, the source of truth)

- REST under `/api` (Bearer token or cookie): whoami, login, logout,
  profile, sessions, members, room/{settings,control,screen,clipboard,
  broadcast,upload}, filetransfer plugin routes; `/health`, `/metrics`.
  Full list: `server/openapi.yaml`.
- WS `/api/ws?token=` — JSON `{event, payload}`. Server events:
  system/{init,admin,settings,disconnect,heartbeat},
  session/{created,deleted,profile,state,cursors}, control/{host,request},
  screen/updated, clipboard/updated, broadcast/status, send/{unicast,broadcast},
  signal/{provide,offer,answer,candidate,close,restart}.
  Client events: client/heartbeat, control/{request,release,paste,scroll,
  keydown,keyup,buttondown,buttonup,touchbegin,touchupdate,touchend},
  clipboard/set, keyboard/map, screen/set, send/{unicast,broadcast},
  signal/{request,offer,answer,candidate}, plugin events
  (chat/message, filetransfer/update, ...).
- Data channel (binary, big-endian, `[u8 op][u16 len][payload]`):
  client→server MOVE=1{u16 x,y} SCROLL=2{i16 dx,dy,u8 ctrl} KEY_DOWN/UP=3/4
  {u32 keysym} BTN_DOWN/UP=5/6{u32} PING=7{2×u32 ts} TOUCH_BEGIN/UPDATE/END=
  8/9/10{u32 id,i32 x,y,u8 pressure}; server→client CURSOR_POSITION=1{u16 x,y}
  CURSOR_IMAGE=2{u16 w,h,x,y,png} PONG=3{4×u32}. Legacy scroll: len==4 means
  two u16 (x,y), no control_key.
- WebRTC: client `signal/request{video,audio}` → server `signal/provide{sdp
  (offer), iceservers}` + trickled `signal/candidate` → client `signal/answer`;
  mic share → client `signal/offer` → server `signal/answer`.

## Port map (Go → Rust)

| Go (server/)                          | Rust (neko-rs/src/)      | phase |
|---------------------------------------|--------------------------|-------|
| pkg/types + pkg/types/message         | messages.rs (done)       | A     |
| internal/webrtc/payload               | payload.rs (done)        | A     |
| internal/config                       | config.rs (skeleton)     | A     |
| internal/websocket (manager+handler)  | ws/ + room/              | B     |
| internal/session + member (multiuser) | room/ + api/ auth        | B     |
| internal/api (openapi.yaml)           | api/                     | B–E   |
| internal/webrtc (peer, manager)       | webrtc/ (from peer.rs)   | C     |
| internal/capture (pipelines, streams) | capture/ (from media.rs) | C     |
| pkg/desktop (X11 via cgo)             | desktop/ (x11rb)         | D     |
| internal/plugins (chat, filetransfer) | plugins/                 | E     |
| broadcast (RTMP) / screencast (MJPEG) | capture/broadcast.rs     | E     |

## Phases

- **A — scaffold + protocol** (this commit): crate layout, payload codec with
  tests, message types, config skeleton, module stubs.
- **B — control plane**: sessions (multiuser auth, tokens, merciful
  reconnect), ws event pump, system/init, host logic, room settings,
  heartbeat. Milestone: client logs in, sees the room, sessions list works.
- **C — media**: port peer.rs/media.rs in-process; signal/* flow; pipelines
  assembled per config (VP8/H264 + Opus). Milestone: browser plays video/audio.
- **D — input**: x11rb desktop (move/scroll/key/button/touch/clipboard),
  data channel + ws input paths, control protection, keyboard map.
  Milestone: keyboard/mouse drive the remote desktop.
- **E — plugins + extras**: chat, filetransfer, broadcast, screencast,
  screen configurations, clipboard image, upload/drop.
- **F — container**: neko-rs binary in the runtime image (supervisord
  replaces `neko serve`), CI workflow (cargo check/test + image build),
  e2e against the unchanged client.

## Notes

- webrtc is pinned to `=0.14.0` (last classic-API release; 0.20+ is the
  driver rewrite). No pion EPR equivalent — deployments use
  `--sysctl net.ipv4.ip_local_port_range=...` (see dataplane/README.md).
- `src/bin/webshell.rs` is vendored from dataplane/src/bin/ for now; the
  dataplane crate still ships its own copy until neko-rs takes over the
  image (dedup then).
- Local `cargo check` needs system GStreamer (brew install gstreamer
  gst-plugins-base); CI check workflow lands in phase F.
