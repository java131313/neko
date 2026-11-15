ARG BASE_IMAGE=ghcr.io/m1k1o/neko/base:latest
FROM $BASE_IMAGE

ARG API_URL="https://packages.microsoft.com/repos/edge/pool/main/m/microsoft-edge-stable/"

#
# install microsoft edge
RUN set -eux; apt-get update; \
    #
    # fetch latest release for the BUILD architecture (the pool mixes archs
    # and the newest version may not ship an arm64 deb yet - a blind
    # "last href" picked the amd64 deb and apt failed with an arch mismatch)
    ARCH="$(dpkg --print-architecture)"; \
    DEB_NAME="$(wget -O - "${API_URL}" 2>/dev/null | sed -n 's/.*href="\([^"]*\).*/\1/p' | grep "_${ARCH}\.deb" | sort --version-sort | tail -1)"; \
    # the stable pool is amd64-only at the time of writing: fail loudly
    # instead of feeding the HTML listing to apt
    [ -n "${DEB_NAME}" ] || { echo "no microsoft-edge-stable ${ARCH} deb in pool"; exit 1; }; \
    SRC_URL="${API_URL}${DEB_NAME}"; \
    wget -O /tmp/microsoft-edge.deb "${SRC_URL}"; \
    # postgresql-client + sqlite3: profile state sync and browsing log
    # (neko-profile-sync restores/backs up the profile and mirrors History
    # entries into the configured postgres)
    apt-get install -y --no-install-recommends openbox postgresql-client sqlite3 /tmp/microsoft-edge.deb; \
    #
    # clean up
    apt-get clean -y; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/*

#
# copy configuation files
COPY supervisord.conf /etc/neko/supervisord/microsoft-edge.conf
COPY profile-sync.sh /usr/bin/neko-profile-sync
RUN chmod +x /usr/bin/neko-profile-sync
COPY --chown=neko preferences.json /home/neko/.config/microsoft-edge/Default/Preferences
COPY policies.json /etc/opt/edge/policies/managed/policies.json
COPY openbox.xml /etc/neko/openbox.xml
