ARG BASE_IMAGE=golang:1.26-trixie
FROM $BASE_IMAGE AS server

WORKDIR /src

#
# install dependencies
# Optional Debian mirror override (--build-arg DEBIAN_MIRROR=mirrors.ustc.edu.cn)
# for networks where deb.debian.org throttles large packages.
ARG DEBIAN_MIRROR=""
ENV DEBIAN_FRONTEND=noninteractive
RUN set -eux; \
    if [ -n "$DEBIAN_MIRROR" ]; then \
        sed -i "s|deb.debian.org|${DEBIAN_MIRROR}|g" /etc/apt/sources.list.d/debian.sources; \
    fi; \
    apt-get update; \
    apt-get -o Acquire::Retries=8 -o Acquire::http::Timeout=30 install -y --no-install-recommends \
        libx11-dev libxrandr-dev libxtst-dev libgtk-3-dev libxcvt-dev \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

ARG GIT_COMMIT
ARG GIT_BRANCH
ARG GIT_TAG

#
# Go module proxy (proxy.golang.org is unreliable from some networks;
# override at build time with --build-arg GOPROXY=...).
ARG GOPROXY=https://goproxy.cn,direct
ENV GOPROXY=${GOPROXY}

#
# download modules first, so code changes don't invalidate the layer
# (single-source COPYs: the utils/docker preprocessor mangles multi-source ones)
COPY go.mod .
COPY go.sum .
RUN go mod download

#
# build server
COPY . .
RUN ./build

#
# artifacts from this stage
# COPY --from=server /src/bin/plugins/ $NEKO_PLUGINS_DIR
# COPY --from=server /src/bin/neko /usr/bin/neko
