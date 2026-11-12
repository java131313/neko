ARG BASE_IMAGE=debian:trixie-slim
FROM $BASE_IMAGE AS xorg-deps

WORKDIR /xorg

# Optional Debian mirror override (--build-arg DEBIAN_MIRROR=mirrors.ustc.edu.cn)
# for networks where deb.debian.org throttles large packages.
ARG DEBIAN_MIRROR=""

ENV DEBIAN_FRONTEND=noninteractive
RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 -o Acquire::http::Timeout=30 install -y \
        git gcc pkgconf autoconf automake libtool make xorg-dev xutils-dev \
    && rm -rf /var/lib/apt/lists/*;

COPY . /xorg/

# build xf86-video-dummy v0.3.8 with RandR support
RUN set -eux; \
    cd xf86-video-dummy/v0.3.8; \
    patch -p1 < ../01_v0.3.8_xdummy-randr.patch; \
    autoreconf -v --install; \
    ./configure; \
    make -j$(nproc); \
    make install;

# build custom input driver
RUN set -eux; \
    cd xf86-input-neko; \
    ./autogen.sh --prefix=/usr; \
    ./configure; \
    make -j$(nproc); \
    make install;

# artifacts from this stage
# COPY --from=xorg-deps /usr/local/lib/xorg/modules/drivers/dummy_drv.so /usr/lib/xorg/modules/drivers/dummy_drv.so
# COPY --from=xorg-deps /usr/local/lib/xorg/modules/input/neko_drv.so /usr/lib/xorg/modules/input/neko_drv.so
