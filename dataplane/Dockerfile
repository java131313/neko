ARG BASE_IMAGE=rust:1-slim-trixie
FROM $BASE_IMAGE AS builder

WORKDIR /src

# Optional Debian mirror override (--build-arg DEBIAN_MIRROR=mirrors.ustc.edu.cn)
# for networks where deb.debian.org throttles large packages.
ARG DEBIAN_MIRROR=""

# protoc for tonic-build (build.rs compiles proto/neko.proto);
# gstreamer dev libs for gstreamer-rs
RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 -o Acquire::http::Timeout=30 install -y --no-install-recommends \
        protobuf-compiler \
        pkg-config \
        libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# download dependencies first, so code changes don't invalidate the layer.
# cargo fetch needs valid target sources and a parseable proto for build.rs,
# hence the placeholders.
# (single-source COPYs: the utils/docker preprocessor mangles multi-source ones)
COPY Cargo.toml .
COPY Cargo.lock .
COPY build.rs .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    set -eux; \
    mkdir -p proto src; \
    echo 'syntax = "proto3";' > proto/neko.proto; \
    echo 'fn main() {}' > src/main.rs; \
    cargo fetch; \
    rm -rf proto src

COPY . .
# NB: the binary must be copied OUT of the target cache mount within the
# same RUN — cache mounts are discarded between steps.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release && \
    cp /src/target/release/neko-dataplane /usr/local/bin/neko-dataplane

FROM debian:trixie-slim

COPY --from=builder /usr/local/bin/neko-dataplane /usr/local/bin/neko-dataplane
