ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

#
# install VS Code (Microsoft ships per-arch debs for x64 and arm64) and a
# full Go development environment: Go toolchain + gopls + the golang.Go
# extension preinstalled for the container user.
RUN set -eux; apt-get update; \
    apt-get install -y --no-install-recommends openbox wget; \
    #
    # VS Code, matching the image architecture
    ARCH=$(dpkg --print-architecture); \
    case "${ARCH}" in \
        amd64) VSCODE_ARCH="x64" ;; \
        arm64) VSCODE_ARCH="arm64" ;; \
        *) echo "unsupported arch: ${ARCH}" >&2; exit 1 ;; \
    esac; \
    wget -O /tmp/code.deb "https://update.code.visualstudio.com/latest/linux-deb-${VSCODE_ARCH}/stable"; \
    apt-get install -y --no-install-recommends /tmp/code.deb; \
    rm -f /tmp/code.deb; \
    #
    # Go toolchain (Debian trixie ships a recent Go for both arches) plus
    # gopls so the Go extension works out of the box
    apt-get install -y --no-install-recommends golang-go; \
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
# install the VS Code Go extension as the container user (extensions land in
# ~/.vscode/extensions); CLI mode, no display needed
USER neko
RUN set -eux; code --no-sandbox --install-extension golang.Go; \
    # make the bundled Go/gopls discoverable to the extension
    go env -w GOPROXY=https://goproxy.cn,direct
USER root

#
# copy configuation files
COPY supervisord.conf /etc/neko/supervisord/vscode.conf
COPY openbox.xml /etc/neko/openbox.xml
