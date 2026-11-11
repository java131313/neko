#!/bin/bash
# Neko chromium profile state sync + browsing log.
#
# 1) Profile state: keeps the chromium profile (open tabs, history, cookies)
#    persisted in PostgreSQL so container restarts restore the last session.
# 2) Browsing log: reads new entries from chromium's History and upserts them
#    into a per-user log table (URL, search keyword, first/last opened time).
#    The username is resolved from neko's server log: websocket connect and
#    disconnect events carry session_id=<member>-<suffix>.
#
# Requires NEKO_CHROMIUM_STATE_URL (postgresql://...) at runtime; without it
# chromium simply runs without sync/logging.
#
# Tables (created automatically):
#   <state_table> (id text PRIMARY KEY, data text, updated_at timestamptz)
#   <log_table>   (id bigserial PRIMARY KEY, username text NOT NULL, url text NOT NULL,
#                  title text, search_keyword text,
#                  first_opened_at timestamptz, last_opened_at timestamptz, open_count int,
#                  UNIQUE (username, url))

set -u

PROFILE_DIR="${NEKO_CHROMIUM_PROFILE_DIR:-/home/neko/.config/chromium}"
DB_URL="${NEKO_CHROMIUM_STATE_URL:-}"
INTERVAL="${NEKO_CHROMIUM_STATE_INTERVAL:-30}"
TABLE="${NEKO_CHROMIUM_STATE_TABLE:-neko_chromium_state}"
LOG_TABLE="${NEKO_CHROMIUM_LOG_TABLE:-neko_browsing_log}"
ID="${NEKO_CHROMIUM_STATE_ID:-default}"
NEKO_LOG="${NEKO_SERVER_LOG:-/var/log/neko/neko.log}"

log() { echo "[profile-sync] $*"; }

# allow only safe characters
TABLE=$(echo "$TABLE" | tr -cd 'a-zA-Z0-9_')
LOG_TABLE=$(echo "$LOG_TABLE" | tr -cd 'a-zA-Z0-9_')
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

sql_escape() { printf '%s' "$1" | sed "s/'/''/g"; }

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
        --exclude='SingletonLock' \
        --exclude='SingletonCookie' \
        --exclude='SingletonSocket' \
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
                # stale inter-process locks from a previous container would
                # block chromium startup ("profile appears to be in use")
                rm -f "$PROFILE_DIR"/SingletonLock "$PROFILE_DIR"/SingletonCookie "$PROFILE_DIR"/SingletonSocket
                log "restored profile snapshot from database"
            fi
        fi
    fi
    rm -f "$raw" "$tgz"
}

sig() {
    stat -c %Y "$PROFILE_DIR/Default/Sessions" "$PROFILE_DIR/Default/History" 2>/dev/null | tr '\n' ' '
}

#
# browsing log
#

ONLINEF=""   # session ids currently online (temp file)
WATERMARK_FILE="$PROFILE_DIR/.neko_history_watermark"

online_members() {
    # session ids -> member names (strip last -suffix), deduplicated
    local sid
    [ -s "$ONLINEF" ] || return 0
    while IFS= read -r sid; do
        [ -n "$sid" ] && printf '%s\n' "${sid%-*}"
    done < "$ONLINEF" | sort -u
}

track_online() {
    # follow neko server log; "connection started/ended" carry session_id
    [ -r "$NEKO_LOG" ] || { log "neko log not readable, online tracking disabled"; return; }
    tail -F -n 0 "$NEKO_LOG" 2>/dev/null | while IFS= read -r line; do
        case "$line" in
            *connection\ started*session_id=*)
                sid=$(printf '%s' "$line" | sed -n 's/.*session_id=\([^ ]*\).*/\1/p')
                if [ -n "$sid" ] && ! grep -qx "$sid" "$ONLINEF" 2>/dev/null; then
                    printf '%s\n' "$sid" >> "$ONLINEF"
                fi
                ;;
            *connection\ ended*session_id=*)
                sid=$(printf '%s' "$line" | sed -n 's/.*session_id=\([^ ]*\).*/\1/p')
                if [ -n "$sid" ]; then
                    grep -vx "$sid" "$ONLINEF" > "$ONLINEF.tmp" 2>/dev/null \
                        && mv "$ONLINEF.tmp" "$ONLINEF"
                fi
                ;;
        esac
    done
}

urldecode() {
    local data="${1//+/ }" out="" i ch hex
    for ((i = 0; i < ${#data}; i++)); do
        ch="${data:i:1}"
        if [ "$ch" = "%" ] && [ $((i + 2)) -lt ${#data} ]; then
            hex="${data:i+1:2}"
            if [[ "$hex" =~ ^[0-9A-Fa-f]{2}$ ]]; then
                out+=$(printf '%b' "\\x$hex")
                i=$((i + 2))
                continue
            fi
        fi
        out+="$ch"
    done
    printf '%s' "$out"
}

search_keyword() {
    # extract query from known search engine URLs
    case "$1" in
        *google.*/search*|*bing.com/search*|*duckduckgo.com/*|*baidu.com/s?*|*sogou.com/web?*|*yahoo.com/search*|*yandex.*/search*|*ecosia.org/search*|*startpage.com/sp/search*)
            local url="$1" name q=""
            for name in q query wd word text p; do
                q=$(printf '%s' "$url" | sed -n "s/.*[?&]${name}=\([^&]*\).*/\1/p")
                [ -n "$q" ] && break
            done
            [ -n "$q" ] && urldecode "$q"
            ;;
    esac
}

log_history() {
    local db="$PROFILE_DIR/Default/History"
    [ -s "$db" ] || return 0

    local members
    members=$(online_members)
    [ -n "$members" ] || return 0

    local wm=0
    [ -s "$WATERMARK_FILE" ] && wm=$(cat "$WATERMARK_FILE" 2>/dev/null || echo 0)

    # hot-copy the sqlite files so we don't fight chromium's WAL locks
    local tmpd
    tmpd=$(mktemp -d)
    cp "$db" "$tmpd/History" 2>/dev/null
    [ -f "$db-wal" ] && cp "$db-wal" "$tmpd/History-wal" 2>/dev/null
    [ -f "$db-shm" ] && cp "$db-shm" "$tmpd/History-shm" 2>/dev/null

    local rows
    rows=$(sqlite3 -readonly "$tmpd/History" \
        "SELECT visits.id, urls.url, COALESCE(urls.title, ''), datetime(visits.visit_time/1000000 - 11644473600, 'unixepoch') FROM visits JOIN urls ON urls.id = visits.url WHERE visits.id > $wm ORDER BY visits.id ASC LIMIT 500;" 2>/dev/null)
    rm -rf "$tmpd"
    [ -n "$rows" ] || return 0

    # build multi-row upsert, one row per (member, visit)
    local values="" vid url title ts kw url_e title_e member
    local max_id=$wm
    while IFS='|' read -r vid url title ts; do
        [ -n "$vid" ] || continue
        [ "$vid" -gt "$max_id" ] 2>/dev/null && max_id=$vid
        kw=$(search_keyword "$url")
        url_e=$(sql_escape "$url")
        title_e=$(sql_escape "$title")
        kw_e=$(sql_escape "$kw")
        while IFS= read -r member; do
            [ -n "$member" ] || continue
            member_e=$(sql_escape "$member")
            values+="\n('$member_e', '$url_e', '$title_e', NULLIF('$kw_e', ''), '$ts+00', '$ts+00', 1),"
        done <<< "$members"
    done <<< "$rows"

    [ -n "$values" ] || return 0
    printf "INSERT INTO %s (username, url, title, search_keyword, first_opened_at, last_opened_at, open_count) VALUES %s\nON CONFLICT (username, url) DO UPDATE SET last_opened_at = EXCLUDED.last_opened_at, open_count = %s.open_count + 1, title = EXCLUDED.title, search_keyword = COALESCE(NULLIF(EXCLUDED.search_keyword, ''), %s.search_keyword);" \
        "$LOG_TABLE" "${values%,}" "$LOG_TABLE" "$LOG_TABLE" | psql_c -f - \
        && { echo "$max_id" > "$WATERMARK_FILE"; log "logged history up to visit $max_id"; }
}

# init tables (best effort)
psql_c -c "CREATE TABLE IF NOT EXISTS $TABLE (id text PRIMARY KEY, data text, updated_at timestamptz NOT NULL DEFAULT now());" \
    || log "WARNING: state table unavailable, sync disabled"
psql_c -c "CREATE TABLE IF NOT EXISTS $LOG_TABLE (id bigserial PRIMARY KEY, username text NOT NULL, url text NOT NULL, title text, search_keyword text, first_opened_at timestamptz NOT NULL DEFAULT now(), last_opened_at timestamptz NOT NULL DEFAULT now(), open_count integer NOT NULL DEFAULT 1, UNIQUE (username, url));" \
    || log "WARNING: browsing log table unavailable"

# restore only when there is no local profile yet (fresh container)
if [ ! -s "$PROFILE_DIR/Default/History" ]; then
    restore
fi

ONLINEF=$(mktemp)
export ONLINEF

"$@" &
CHILD=$!

cleanup() {
    trap - INT TERM
    kill "$WATCHER_PID" "$TRACKER_PID" 2>/dev/null
    log "saving final snapshot"
    save
    log_history
    kill -INT "$CHILD" 2>/dev/null
    wait "$CHILD" 2>/dev/null
    rm -f "$ONLINEF"
    exit 0
}
trap cleanup INT TERM

# online tracker: who is connected, from neko server log
track_online &
TRACKER_PID=$!

# background watcher: save when the session or history changes
(
    LAST=""
    while true; do
        S=$(sig)
        if [ -n "$S" ] && [ "$S" != "$LAST" ]; then
            sleep 3
            save
            log_history
            LAST=$(sig)
        fi
        sleep "$INTERVAL"
    done
) &
WATCHER_PID=$!

wait "$CHILD"
EXIT=$?
kill "$WATCHER_PID" "$TRACKER_PID" 2>/dev/null
# chromium exited on its own, persist the final state
save
log_history
rm -f "$ONLINEF"
exit $EXIT
