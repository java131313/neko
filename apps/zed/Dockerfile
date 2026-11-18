ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

#
# install Zed editor (official per-arch Linux tarball: x86_64 + aarch64),
# software Vulkan (lavapipe) since containers have no GPU, and a Go
# development environment (toolchain + gopls for Zed's Go extension).
RUN set -eux; apt-get update; \
    apt-get install -y --no-install-recommends openbox wget xz-utils git unzip \
        libvulkan1 mesa-vulkan-drivers libxkbcommon0 libasound2t64 fonts-noto-color-emoji \
        golang-go; \
    #
    # Zed, matching the image architecture
    ARCH=$(dpkg --print-architecture); \
    case "${ARCH}" in \
        amd64) ZED_ARCH="x86_64" ;; \
        arm64) ZED_ARCH="aarch64" ;; \
        *) echo "unsupported arch: ${ARCH}" >&2; exit 1 ;; \
    esac; \
    wget -O /tmp/zed.tar.gz "https://github.com/zed-industries/zed/releases/latest/download/zed-linux-${ZED_ARCH}.tar.gz"; \
    tar -xzf /tmp/zed.tar.gz -C /opt; \
    mv /opt/zed.app /opt/zed; \
    ln -sf /opt/zed/bin/zed /usr/local/bin/zed; \
    rm -f /tmp/zed.tar.gz; \
    #
    # Go toolchain + gopls (Zed's Go extension picks gopls from PATH)
    GOPROXY=https://goproxy.cn,direct GOBIN=/usr/local/bin go install golang.org/x/tools/gopls@latest; \
    #
    # bun runtime (pi-mono is a bun/TypeScript monorepo)
    case "${ARCH}" in \
        amd64) BUN_ARCH="x64" ;; \
        arm64) BUN_ARCH="aarch64" ;; \
        *) echo "unsupported arch: ${ARCH}" >&2; exit 1 ;; \
    esac; \
    wget -O /tmp/bun.zip "https://github.com/oven-sh/bun/releases/latest/download/bun-linux-${BUN_ARCH}.zip"; \
    unzip -q /tmp/bun.zip -d /tmp; \
    install -m 0755 "/tmp/bun-linux-${BUN_ARCH}/bun" /usr/local/bin/bun; \
    rm -rf /tmp/bun.zip "/tmp/bun-linux-${BUN_ARCH}"; \
    #
    # pi-mono (Pi agent harness) source in the workspace, ready to hack on from Zed
    git clone --depth 1 https://github.com/badlogic/pi-mono.git /home/neko/workspace/pi-mono; \
    cd /home/neko/workspace/pi-mono; \
    bun install; \
    #
    # default project directory
    mkdir -p /home/neko/workspace; \
    chown -R neko:neko /home/neko/workspace; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# copy configuation files
COPY supervisord.conf /etc/neko/supervisord/zed.conf
COPY openbox.xml /etc/neko/openbox.xml
