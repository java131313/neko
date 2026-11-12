# neko-dataplane

Rust media dataplane for neko. Owns the media path — GStreamer
capture/encoding and WebRTC peer connections — and is driven by the Go
control plane (`server/`) over a Unix socket gRPC API.

This is Phase 1 (skeleton): the control-plane <-> dataplane plumbing,
configuration push and peer lifecycle over gRPC. Media (GStreamer +
webrtc-rs), ICE negotiation, data channels and events arrive in Phase 2.

## Layout

- `proto/neko.proto` — gRPC API, the single source of truth for both sides
- `src/main.rs` — entrypoint: Unix socket listener, logging, graceful shutdown
- `src/service.rs` — `Dataplane` service implementation
- `src/pb.rs` — generated protobuf code (via `build.rs` + tonic-build)

## Running

```sh
cargo build
NEKO_DATAPLANE_LISTEN=/run/neko/dataplane.sock ./target/debug/neko-dataplane
```

Configuration:

- `NEKO_DATAPLANE_LISTEN` — socket path (or `unix://` URL), default
  `/run/neko/dataplane.sock`; must match the Go side's `NEKO_WEBRTC_DATAPLANE`
- `NEKO_LOG_LEVEL` — tracing filter, default `info`
- `NEKO_LOG_JSON` — `1`/`true` for JSON logs (matches neko's `NEKO_LOG_JSON`)

## Deployment note: UDP port range

pion's `NEKO_WEBRTC_EPR` has no equivalent in webrtc-rs (no version supports
pinning the ICE ephemeral port pool). Pin the container's ephemeral range at
the kernel level instead — the OS then hands out only ports inside your
published range:

```sh
docker run ... \
  -p 53000-53100:53000-53100/udp \
  --sysctl net.ipv4.ip_local_port_range=53000-53100 \
  ...
```

A 101-port pool supports ~50 concurrent peer connections (one UDP socket per
component, RTCP muxed), which is plenty for neko. `NEKO_WEBRTC_EPR` is still
parsed by the Go control plane (for the embedded mode) but ignored by the
dataplane, with a warning logged.

## Regenerating the gRPC code

Go: `proto/gen.sh` (requires protoc-gen-go and protoc-gen-go-grpc).
Rust: happens automatically on `cargo build` via `build.rs`.

Whenever `proto/neko.proto` changes, regenerate both sides and commit the
results — generated code is checked in.
