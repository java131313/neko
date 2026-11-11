ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

COPY ./widevinecdm.sh /widevine.sh

#
# install neko chromium
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends chromium chromium-common chromium-sandbox openbox postgresql-client sqlite3; \
    #
    # install widevine module (only for amd64)
    CHROMIUM_DIR="/usr/lib/chromium"; \
    ARCH=$(dpkg --print-architecture); \
    if [ "${ARCH}" = "amd64" ]; then \
        apt-get install -y --no-install-recommends xz-utils; \
        ./widevine.sh "${CHROMIUM_DIR}/WidevineCdm"; \
        rm -f /widevine.sh; \
        apt-get --purge autoremove -y xz-utils; \
    elif [ "${ARCH}" = "arm64" ]; then \
        mkdir -p "${CHROMIUM_DIR}/WidevineCdm"; \
        find /var/lib/widevine/WidevineCdm/ ! -name "WidevineCdm" -exec cp -RL {} "${CHROMIUM_DIR}/WidevineCdm/" \; ; \
    else \
        echo "Widevine is not supported on ${ARCH}"; \
    fi; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# install selenium + chromium-driver to automate the in-container chromium over CDP
# (chromium-driver is version-locked to Debian's chromium; playwright 1.65, the latest on PyPI,
#  cannot drive Chromium 156: connect works but page inventory/new_page are broken)
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends python3-selenium chromium-driver; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# install the webshell server (raw WebSocket <-> PTY bridge for the client terminal tab) and pi.
# Node.js comes from the official tarball because Debian's nodejs is older than pi's requirement.
# publish port 7681 to reach the terminal from the browser (-p 7681:7681)
ARG NODE_VERSION=22.19.0
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends python3-websockets curl xz-utils ca-certificates; \
    #
    # nodejs for pi (>= 22.19)
    ARCH=$(dpkg --print-architecture); \
    case "${ARCH}" in \
        amd64) NODE_ARCH="x64" ;; \
        arm64) NODE_ARCH="arm64" ;; \
        *) echo "unsupported arch: ${ARCH}"; exit 1 ;; \
    esac; \
    curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz"; \
    tar -xJf "node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz" -C /usr/local --strip-components=1; \
    rm -f "node-v${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz"; \
    node --version; \
    #
    # pi - the AI agent CLI, run `pi` inside the webshell
    npm install -g --ignore-scripts @earendil-works/pi-coding-agent; \
    command -v pi; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# copy configuation files
COPY profile-sync.sh /usr/bin/neko-profile-sync
RUN chmod +x /usr/bin/neko-profile-sync
COPY webshell.py /usr/bin/neko-webshell
RUN chmod +x /usr/bin/neko-webshell
COPY supervisord.conf /etc/neko/supervisord/chromium.conf
COPY --chown=neko preferences.json /home/neko/.config/chromium/Default/Preferences
COPY policies.json /etc/chromium/policies/managed/policies.json
COPY openbox.xml /etc/neko/openbox.xml
