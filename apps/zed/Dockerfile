ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

#
# The all-in-one development desktop: Zed editor, VS Code, three
# chromium-based browsers (Google Chrome, Brave — plus the Tor Browser),
# VLC, Go toolchain + gopls, bun, and the pi coding agent's bun monorepo.
# All apps share one X display; openbox maximizes every window and
# Alt-Tab (A-Tab) cycles them — same recipe as the per-app images.
RUN set -eux; apt-get update; \
    apt-get install -y --no-install-recommends openbox wget curl xz-utils git unzip \
        apt-transport-https libdbus-glib-1-2 \
        libvulkan1 mesa-vulkan-drivers libxkbcommon0 libasound2t64 fonts-noto-color-emoji \
        golang-go vlc; \
    #
    # Zed, matching the image architecture
    ARCH=$(dpkg --print-architecture); \
    case "${ARCH}" in \
        amd64) ZED_ARCH="x86_64"; BUN_ARCH="x64" ;; \
        arm64) ZED_ARCH="aarch64"; BUN_ARCH="aarch64" ;; \
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
    # bun runtime
    wget -O /tmp/bun.zip "https://github.com/oven-sh/bun/releases/latest/download/bun-linux-${BUN_ARCH}.zip"; \
    unzip -q /tmp/bun.zip -d /tmp; \
    install -m 0755 "/tmp/bun-linux-${BUN_ARCH}/bun" /usr/local/bin/bun; \
    rm -rf /tmp/bun.zip "/tmp/bun-linux-${BUN_ARCH}"; \
    #
    # Tor Browser: official per-arch tarball (torproject update API);
    # the dir must belong to the runtime user
    case "${ARCH}" in \
        amd64) TOR_JSON="https://aus1.torproject.org/torbrowser/update_3/release/download-linux-x86_64.json" ;; \
        arm64) TOR_JSON="https://aus1.torproject.org/torbrowser/update_3/release/download-linux-aarch64.json" ;; \
    esac; \
    TOR_URI=$(curl -s -N "$TOR_JSON" | grep -Po '"binary"\s*:\s*"\K[^"]+'); \
    curl -sSL -o /tmp/tor.tar.xz "$TOR_URI"; \
    tar -xJf /tmp/tor.tar.xz -C /opt; \
    mv /opt/tor-browser* /opt/tor-browser_en-US; \
    chown -R neko:neko /opt/tor-browser_en-US; \
    rm -f /tmp/tor.tar.xz; \
    #
    # default project directory + vlc media dir
    mkdir -p /home/neko/workspace /media; \
    chown -R neko:neko /home/neko/workspace /media; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# VS Code (Microsoft ships per-arch debs)
RUN set -eux; \
    ARCH=$(dpkg --print-architecture); \
    case "${ARCH}" in \
        amd64) VSCODE_ARCH="x64" ;; \
        arm64) VSCODE_ARCH="arm64" ;; \
        *) echo "unsupported arch: ${ARCH}" >&2; exit 1 ;; \
    esac; \
    wget -O /tmp/code.deb "https://update.code.visualstudio.com/latest/linux-deb-${VSCODE_ARCH}/stable"; \
    apt-get update; \
    apt-get install -y --no-install-recommends /tmp/code.deb; \
    rm -f /tmp/code.deb; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# Google Chrome (per-arch deb; ARM64 CDM from the runtime's widevine)
RUN set -eux; \
    ARCH="$(dpkg --print-architecture)"; \
    wget -O /tmp/google-chrome.deb "https://dl.google.com/linux/direct/google-chrome-stable_current_${ARCH}.deb"; \
    apt-get update; \
    apt-get install -y --no-install-recommends /tmp/google-chrome.deb; \
    if [ "${ARCH}" = "arm64" ]; then \
        mkdir -p /opt/google/chrome/WidevineCdm; \
        find /var/lib/widevine/WidevineCdm/ ! -name "WidevineCdm" -exec cp -RL {} /opt/google/chrome/WidevineCdm/ \; ; \
    fi; \
    rm -f /tmp/google-chrome.deb; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# Brave (official apt repository)
RUN set -eux; \
    ARCH=$(dpkg --print-architecture); \
    curl -fsSLo /usr/share/keyrings/brave-browser-archive-keyring.gpg https://brave-browser-apt-release.s3.brave.com/brave-browser-archive-keyring.gpg; \
    echo "deb [signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg arch=${ARCH}] https://brave-browser-apt-release.s3.brave.com/ stable main" \
        > /etc/apt/sources.list.d/brave-browser-release.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends brave-browser; \
    if [ "${ARCH}" = "arm64" ]; then \
        mkdir -p /opt/brave.com/brave/WidevineCdm; \
        find /var/lib/widevine/WidevineCdm/ ! -name "WidevineCdm" -exec cp -RL {} /opt/brave.com/brave/WidevineCdm/ \; ; \
    fi; \
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

# pi-mono (Pi agent harness) source in the workspace, ready to hack on
RUN set -eux; \
    git clone --depth 1 https://github.com/badlogic/pi-mono.git /home/neko/workspace/pi-mono; \
    cd /home/neko/workspace/pi-mono; \
    bun install; \
    chown -R neko:neko /home/neko/workspace

# vscode's Go extension + the vscode CLI on PATH for the container user
USER neko
RUN set -eux; code --no-sandbox --install-extension golang.Go; \
    go env -w GOPROXY=https://goproxy.cn,direct
USER root

# vlc's default media location (a bare UI when empty, like the vlc app)
ENV VLC_MEDIA="/media"

#
# copy configuation files
COPY supervisord.conf /etc/neko/supervisord/zed.conf
COPY openbox.xml /etc/neko/openbox.xml
