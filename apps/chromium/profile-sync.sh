#!/bin/bash
# Neko chromium profile state sync.
#
# Keeps the chromium profile (open tabs, history, cookies) persisted in a
# PostgreSQL table so that container restarts can restore the last session.
# Requires NEKO_CHROMIUM_STATE_URL (postgresql://...) at runtime; without it
# chromium simply runs without state sync.
#
# Table layout (created automatically):
#   CREATE TABLE <table> (id text PRIMARY KEY, data text, updated_at timestamptz);

set -u

PROFILE_DIR="${NEKO_CHROMIUM_PROFILE_DIR:-/home/neko/.config/chromium}"
DB_URL="${NEKO_CHROMIUM_STATE_URL:-}"
INTERVAL="${NEKO_CHROMIUM_STATE_INTERVAL:-30}"
TABLE="${NEKO_CHROMIUM_STATE_TABLE:-neko_chromium_state}"
ID="${NEKO_CHROMIUM_STATE_ID:-default}"

log() { echo "[profile-sync] $*"; }

# allow only safe characters in table/id
TABLE=$(echo "$TABLE" | tr -cd 'a-zA-Z0-9_')
ID=$(echo "$ID" | tr -cd 'a-zA-Z0-9_-')

if [ -z "$DB_URL" ]; then
    log "NEKO_CHROMIUM_STATE_URL not set, running without state sync"
    exec "$@"
fi

# Render and most cloud Postgres require SSL.
case "$DB_URL" in
    *\?*) DB_URL="$DB_URL&sslmode=require&connect_timeout=10" ;;
    *)    DB_URL="$DB_URL?sslmode=require&connect_timeout=10" ;;
esac

psql_c() { psql "$DB_URL" -v ON_ERROR_STOP=1 "$@" >/dev/null 2>&1; }

save() {
    # never snapshot an empty profile over a good one
    [ -s "$PROFILE_DIR/Default/History" ] || return 0
    local b64
    b64=$(tar -czf - -C "$PROFILE_DIR" \
        --exclude='Default/Cache' \
        --exclude='Default/Code Cache' \
        --exclude='Default/GPUCache' \
        --exclude='Default/Service Worker/CacheStorage' \
        --exclude='Default/DawnGraphiteCache' \
        --exclude='Default/DawnWebGPUCache' \
        --exclude='GrShaderCache' \
        --exclude='ShaderCache' \
        --exclude='GraphiteDawnCache' \
        --exclude='component_crx_cache' \
        --exclude='CertificateRevocation' \
        . 2>/dev/null | base64 -w0) || return 0
    [ -n "$b64" ] || return 0
    printf "INSERT INTO %s (id, data, updated_at) VALUES ('%s', '%s', now()) ON CONFLICT (id) DO UPDATE SET data = EXCLUDED.data, updated_at = EXCLUDED.updated_at;" \
        "$TABLE" "$ID" "$b64" | psql_c -f - \
        && log "saved snapshot (${#b64} bytes base64)"
}

restore() {
    local raw tgz
    raw=$(mktemp) && tgz=$(mktemp)
    if psql "$DB_URL" -v ON_ERROR_STOP=1 -At \
        -c "SELECT data FROM $TABLE WHERE id = '$ID';" >"$raw" 2>/dev/null \
        && [ -s "$raw" ]; then
        base64 -d "$raw" >"$tgz" 2>/dev/null
        if [ -s "$tgz" ]; then
            mkdir -p "$PROFILE_DIR"
            if tar -xzf "$tgz" -C "$PROFILE_DIR" 2>/dev/null; then
                log "restored profile snapshot from database"
            fi
        fi
    fi
    rm -f "$raw" "$tgz"
}

sig() {
    stat -c %Y "$PROFILE_DIR/Default/Sessions" "$PROFILE_DIR/Default/History" 2>/dev/null | tr '\n' ' '
}

# init table (best effort)
psql_c -c "CREATE TABLE IF NOT EXISTS $TABLE (id text PRIMARY KEY, data text, updated_at timestamptz NOT NULL DEFAULT now());" \
    || log "WARNING: state table unavailable, sync disabled"

# restore only when there is no local profile yet (fresh container)
if [ ! -s "$PROFILE_DIR/Default/History" ]; then
    restore
fi

"$@" &
CHILD=$!

cleanup() {
    trap - INT TERM
    kill "$WATCHER_PID" 2>/dev/null
    log "saving final snapshot"
    save
    kill -INT "$CHILD" 2>/dev/null
    wait "$CHILD" 2>/dev/null
    exit 0
}
trap cleanup INT TERM

# background watcher: save when the session or history changes
(
    LAST=""
    while true; do
        S=$(sig)
        if [ -n "$S" ] && [ "$S" != "$LAST" ]; then
            sleep 3
            save
            LAST=$(sig)
        fi
        sleep "$INTERVAL"
    done
) &
WATCHER_PID=$!

wait "$CHILD"
EXIT=$?
kill "$WATCHER_PID" 2>/dev/null
# chromium exited on its own, persist the final state
save
exit $EXIT
