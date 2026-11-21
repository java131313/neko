ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

#
# VS Code + Brave 一体镜像:Go 工具链 + gopls(vscode 的 golang.Go 扩展)、
# 两个应用共享一个 X 桌面,apptab 前端 tab 切换(NEKO_DESKTOP_APPS),
# Alt-Tab(openbox A-Tab)同样可用。配方照搬单应用镜像。
RUN set -eux; apt-get update; \
    apt-get install -y --no-install-recommends openbox wget curl apt-transport-https \
        libasound2t64 fonts-noto-color-emoji golang-go; \
    #
    # Go toolchain + gopls (VS Code's Go extension picks gopls from PATH)
    GOPROXY=https://goproxy.cn,direct GOBIN=/usr/local/bin go install golang.org/x/tools/gopls@latest; \
    #
    # default project directory
    mkdir -p /home/neko/workspace; \
    chown -R neko:neko /home/neko/workspace; \
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
    # the deb leaves Microsoft's apt repo behind (deb822 .sources on trixie,
    # legacy .list elsewhere), whose signature sqv rejects ("not signed") and
    # fails every later `apt-get update`
    rm -f /etc/apt/sources.list.d/vscode.list /etc/apt/sources.list.d/vscode.sources; \
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

# vscode's Go extension, installed as the container user (extensions land in
# ~/.vscode/extensions); CLI mode, no display needed
USER neko
RUN set -eux; code --no-sandbox --install-extension golang.Go; \
    go env -w GOPROXY=https://goproxy.cn,direct
USER root

# apptab switcher: the front-end tabs and the WM_CLASS match per app
ENV NEKO_DESKTOP_APPS="vscode:code,brave:brave-browser"

#
# copy configuation files
COPY supervisord.conf /etc/neko/supervisord/zed.conf
COPY openbox.xml /etc/neko/openbox.xml
