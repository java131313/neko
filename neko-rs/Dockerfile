# neko-rs all-in-one e2e image.
#
# Self-contained test image for the Rust server: Xvfb-style dummy X + pulse
# + openbox + chromium + neko-rs + neko-webshell under supervisord. For
# production the neko-rs binaries move into the regular image pipeline
# (Dockerfile.tmpl server stage replacement); this exists to e2e the Rust
# server against the unchanged client.
#
# Build:
#   docker build -t neko-rs:test .
# Run:
#   docker run -d --name neko-rs --shm-size=2g \
#     -p 8080:8080 -p 7681:7681 -p 53000-53100:53000-53100/udp \
#     --sysctl net.ipv4.ip_local_port_range=53000-53100 \
#     -e NEKO_MEMBER_MULTIUSER_USER_PASSWORD=neko \
#     -e NEKO_MEMBER_MULTIUSER_ADMIN_PASSWORD=admin \
#     neko-rs:test

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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release && \
    cp /src/target/release/neko-rs /usr/local/bin/neko-rs && \
    cp /src/target/release/neko-webshell /usr/local/bin/neko-webshell

FROM debian:trixie-slim

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
        chromium \
        fonts-noto-color-emoji fonts-wqy-zenhei fonts-wqy-microhei; \
    groupadd --gid 1000 neko; \
    useradd --uid 1000 --gid neko --shell /bin/bash --create-home neko; \
    adduser neko audio; \
    adduser neko video; \
    adduser neko pulse; \
    mkdir /tmp/.X11-unix; \
    chmod 1777 /tmp/.X11-unix; \
    mkdir -p /var/log/neko /run/neko; \
    chown neko /var/log/neko /run/neko; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# runtime configs from the regular image (relative to the repo root: build
# with `docker build -f neko-rs/Dockerfile .`)
COPY runtime/dbus /usr/bin/dbus
COPY runtime/default.pa /etc/pulse/default.pa
COPY neko-rs/supervisord.conf /etc/neko/supervisord.conf
COPY runtime/supervisord.dbus.conf /etc/neko/supervisord.dbus.conf
COPY runtime/xorg.conf /etc/neko/xorg.conf
COPY neko-rs/supervisord.chromium.conf /etc/neko/supervisord/chromium.conf
COPY apps/chromium/openbox.xml /etc/neko/openbox.xml
COPY --chown=neko:neko runtime/.Xresources /home/neko/.Xresources

COPY --from=builder /usr/local/bin/neko-rs /usr/bin/neko-rs
COPY --from=builder /usr/local/bin/neko-webshell /usr/bin/neko-webshell

ENV USER=neko
ENV DISPLAY=:99.0
ENV PULSE_SERVER=unix:/tmp/pulseaudio.socket
ENV XDG_RUNTIME_DIR=/tmp/runtime-neko
ENV NEKO_SERVER_BIND=:8080
ENV NEKO_CAPTURE_VIDEO_DISPLAY=:99.0
ENV NEKO_WEBRTC_ICELITE=1

EXPOSE 8080 7681

HEALTHCHECK --interval=10s --timeout=5s --retries=8 \
    CMD wget -O - http://localhost:8080/health || exit 1

CMD ["/usr/bin/supervisord", "-c", "/etc/neko/supervisord.conf"]
