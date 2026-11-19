# neko-rs all-in-one e2e image + reusable Rust-stack base.
#
# Stages:
#   builder  – compiles neko-rs + neko-webshell (Rust)
#   client   – builds the unchanged Vue client to /var/www
#   base     – Rust-stack neko base image: X + pulse + openbox + gstreamer
#              + neko-rs + neko-webshell + client under supervisord.
#              App images (apps/<app>/Dockerfile.rust) build FROM this;
#              they drop their [program:<app>] conf into
#              /etc/neko/supervisord/ and their openbox.xml into /etc/neko/.
#              No browser, no coturn. Tag: ghcr.io/java131313/neko/base-rs:<branch>
#   e2e      – (default target) base + chromium + coturn + profile sync,
#              self-contained test image for the Rust server
#              (tag ghcr.io/java131313/neko-rs:test).
#
# LAN / docker-run mode (ICE-Lite + pinned ephemeral ports):
#   docker run -d --name neko-rs --shm-size=2g \
#     -p 8080:8080 -p 7681:7681 -p 53000-53100:53000-53100/udp \
#     --sysctl "net.ipv4.ip_local_port_range=53000 53100" \
#     -e NEKO_WEBRTC_NAT1TO1=<client-reachable IP> \
#     -e NEKO_MEMBER_MULTIUSER_USER_PASSWORD=neko \
#     -e NEKO_MEMBER_MULTIUSER_ADMIN_PASSWORD=admin \
#     ghcr.io/java131313/neko-rs:test
#
# NEKO_WEBRTC_NAT1TO1 rewrites host-candidate IPs (webrtc-rs binds only
# container IPs, which are unreachable across -p port publishing; unlike
# the Go server it does not auto-detect the external address). Without it
# ICE stays in "checking" forever and the video never renders. Examples:
# docker run on the same machine -> 127.0.0.1; LAN/VPS -> the host IP
# clients connect to (published UDP ports must cover the pinned range).
#
# (sysctl value must be SPACE-separated — the kernel rejects "53000-53100"
# with EINVAL; and Docker Desktop may refuse net.* sysctls altogether —
# it is only a collision safeguard for the pinned port range, safe to drop)
#
# SealOS / no-sysctl mode (embedded coturn, fixed ports only) — expose:
#   8080 (tcp, HTTP/WS), 7681 (tcp, webshell), 3478 (udp+tcp, TURN),
#   53400-53420 (udp, TURN relay range)
#   env: NEKO_TURN_HOST=<public address>  (username/password default neko/neko,
#        override with NEKO_TURN_USERNAME / NEKO_TURN_PASSWORD)
# Both browser and neko-rs allocate relay candidates from coturn, so no
# ephemeral-port pinning is needed anywhere. ICE-Lite is auto-disabled when
# TURN servers are configured.

ARG BASE_IMAGE=rust:1-slim-trixie
FROM $BASE_IMAGE AS builder

WORKDIR /src

# Optional Debian mirror override (--build-arg DEBIAN_MIRROR=mirrors.ustc.edu.cn)
ARG DEBIAN_MIRROR=""

# gstreamer dev headers for gstreamer-rs (no protoc: no gRPC in neko-rs)
RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 install -y --no-install-recommends \
        pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# fetch dependencies first, so code changes don't invalidate the layer
# (build context is the repo root: docker build -f neko-rs/Dockerfile .)
COPY neko-rs/Cargo.toml neko-rs/Cargo.lock ./
RUN set -eux; \
    mkdir -p src/bin; \
    echo 'fn main() {}' > src/main.rs; \
    echo 'fn main() {}' > src/bin/webshell.rs; \
    cargo fetch; \
    rm -rf src

COPY neko-rs/ .
# NB: binaries must be copied OUT of the target cache mount in the same RUN
# (fetch first with retries: the registry cache may be cold/partial after a
# failed run, and crates.io downloads occasionally flake on CI)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    for i in 1 2 3; do cargo fetch --locked && break || sleep 15; done; \
    for i in 1 2 3; do cargo build --locked --release && break || sleep 15; done; \
    test -f /src/target/release/neko-rs || exit 1; \
    test -f /src/target/release/neko-webshell || exit 1; \
    cp /src/target/release/neko-rs /usr/local/bin/neko-rs && \
    cp /src/target/release/neko-webshell /usr/local/bin/neko-webshell

FROM node:24-trixie-slim AS client

WORKDIR /src

# the client resolves @m1k1o/neko straight from ./core/src (vite alias +
# tsconfig paths), so one npm ci + vite build produces /var/www content
COPY client/package.json client/package-lock.json ./
RUN npm ci
COPY client/ ./
RUN npm run build

FROM debian:trixie-slim AS base

ARG DEBIAN_MIRROR=""

RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 install -y --no-install-recommends \
        supervisor wget ca-certificates \
        pulseaudio dbus-x11 xserver-xorg-video-dummy \
        libxcb1 libxrandr2 libxv1 libopus0 libxcvt0 \
        xdotool xclip libgtk-3-0 openbox \
        gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
        gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
        gstreamer1.0-pulseaudio \
        fonts-noto-color-emoji fonts-wqy-zenhei fonts-wqy-microhei; \
    groupadd --gid 1000 neko; \
    useradd --uid 1000 --gid neko --shell /bin/bash --create-home neko; \
    adduser neko audio; \
    adduser neko video; \
    adduser neko pulse; \
    mkdir /tmp/.X11-unix; \
    chmod 1777 /tmp/.X11-unix; \
    mkdir -p /var/log/neko /run/neko /etc/neko/supervisord; \
    chown neko /var/log/neko /run/neko; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# runtime configs from the regular image (relative to the repo root: build
# with `docker build -f neko-rs/Dockerfile .`)
COPY runtime/dbus /usr/bin/dbus
COPY runtime/default.pa /etc/pulse/default.pa
COPY runtime/supervisord.dbus.conf /etc/neko/supervisord.dbus.conf
COPY runtime/xorg.conf /etc/neko/xorg.conf
COPY --chown=neko:neko runtime/.Xresources /home/neko/.Xresources

# main supervisord config: x-server + pulseaudio + neko-rs + webshell,
# includes /etc/neko/supervisord/*.conf for the app-specific programs
COPY neko-rs/supervisord.conf /etc/neko/supervisord.conf

COPY --from=builder /usr/local/bin/neko-rs /usr/bin/neko-rs
COPY --from=builder /usr/local/bin/neko-webshell /usr/bin/neko-webshell
COPY --from=client /src/dist /var/www

ENV USER=neko
ENV DISPLAY=:99.0
ENV PULSE_SERVER=unix:/tmp/pulseaudio.socket
ENV XDG_RUNTIME_DIR=/tmp/runtime-neko
ENV NEKO_SERVER_BIND=:8080

EXPOSE 8080 7681

HEALTHCHECK --interval=10s --timeout=5s --retries=8 \
    CMD wget -O - http://localhost:8080/health || exit 1

CMD ["/usr/bin/supervisord", "-c", "/etc/neko/supervisord.conf"]

# e2e test image: base + chromium + coturn + profile sync (all-in-one
# self-contained Rust-stack test image, default target)
FROM base AS e2e

ARG DEBIAN_MIRROR=""

RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 install -y --no-install-recommends \
        chromium coturn postgresql-client sqlite3; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

COPY neko-rs/supervisord.chromium.conf /etc/neko/supervisord/chromium.conf
COPY neko-rs/supervisord.coturn.conf /etc/neko/supervisord/coturn.conf
COPY neko-rs/turnserver.conf /etc/neko/turnserver.conf
COPY apps/chromium/profile-sync.sh /usr/bin/neko-profile-sync
RUN chmod +x /usr/bin/neko-profile-sync
COPY apps/chromium/openbox.xml /etc/neko/openbox.xml

ENV NEKO_CAPTURE_VIDEO_DISPLAY=:99.0
# capture chromium's audio: the monitor of default.pa's audio_output sink
# (explicit beats relying on pulse's default-source selection)
ENV NEKO_CAPTURE_AUDIO_DEVICE=audio_output.monitor
ENV NEKO_WEBRTC_ICELITE=1
# profile-sync.sh follows this log for online users (session_id markers)
ENV NEKO_SERVER_LOG=/var/log/neko/neko-rs.log
