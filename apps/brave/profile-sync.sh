#!/bin/bash
# Neko browser profile state sync + browsing log.
#
# 1) Profile state: keeps the browser profile (open tabs, history, cookies)
#    persisted in PostgreSQL so container restarts restore the last session.
# 2) Browsing log: reads new entries from the browser's History/places sqlite
#    and upserts them into a per-user log table (URL, search keyword,
#    first/last opened time). The username is resolved from neko's server
#    log: websocket connect and disconnect events carry session_id=<member>-<suffix>.
#
# Engines (NEKO_BROWSER_ENGINE, default chromium): chromium covers the whole
# chromium family (chromium, google-chrome, microsoft-edge, ...); firefox
# uses the nested *.default*/ profile dir and places.sqlite history.
#
# Requires NEKO_BROWSER_STATE_URL (postgresql://...) at runtime; without it
# the browser simply runs without sync/logging.
#
# Tables (created automatically):
#   <state_table> (id text PRIMARY KEY, data text, updated_at timestamptz)
#   <log_table>   (id bigserial PRIMARY KEY, username text NOT NULL, url text NOT NULL,
#                  title text, search_keyword text,
#                  first_opened_at timestamptz, last_opened_at timestamptz, open_count int,
#                  UNIQUE (username, url))

set -u

# NEKO_BROWSER_* is the generic name (google-chrome/microsoft-edge/firefox and
# future apps); the legacy NEKO_CHROMIUM_* spelling keeps existing chromium
# deployments working.
PROFILE_DIR="${NEKO_BROWSER_PROFILE_DIR:-${NEKO_CHROMIUM_PROFILE_DIR:-/home/neko/.config/chromium}}"
DB_URL="${NEKO_BROWSER_STATE_URL:-${NEKO_CHROMIUM_STATE_URL:-}}"
INTERVAL="${NEKO_BROWSER_STATE_INTERVAL:-${NEKO_CHROMIUM_STATE_INTERVAL:-30}}"
TABLE="${NEKO_BROWSER_STATE_TABLE:-${NEKO_CHROMIUM_STATE_TABLE:-neko_chromium_state}}"
LOG_TABLE="${NEKO_BROWSER_LOG_TABLE:-${NEKO_CHROMIUM_LOG_TABLE:-neko_browsing_log}}"
ID="${NEKO_BROWSER_STATE_ID:-${NEKO_CHROMIUM_STATE_ID:-default}}"
NEKO_LOG="${NEKO_SERVER_LOG:-/var/log/neko/neko.log}"

# browser engine: chromium covers the chromium family (chromium, google-chrome,
# microsoft-edge, ...); firefox has a different profile layout and history DB
ENGINE="${NEKO_BROWSER_ENGINE:-chromium}"

# per-engine history database path (chromium: History sqlite, firefox: places.sqlite)
history_db() {
    if [ "$ENGINE" = firefox ]; then
        local d
        for d in "$PROFILE_DIR"/*.default*/; do
            [ -d "$d" ] && { printf '%splaces.sqlite' "$d"; return; }
        done
        # matches the profiles.ini shipped by the firefox app (Path=profile.default)
        printf '%s/profile.default/places.sqlite' "$PROFILE_DIR"
    else
        printf '%s/Default/History' "$PROFILE_DIR"
    fi
}

log() { echo "[profile-sync] $*"; }

# allow only safe characters
TABLE=$(echo "$TABLE" | tr -cd 'a-zA-Z0-9_')
LOG_TABLE=$(echo "$LOG_TABLE" | tr -cd 'a-zA-Z0-9_')
ID=$(echo "$ID" | tr -cd 'a-zA-Z0-9_-')

if [ -z "$DB_URL" ]; then
    log "NEKO_BROWSER_STATE_URL (or legacy NEKO_CHROMIUM_STATE_URL) not set, running without state sync"
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
    [ -s "$(history_db)" ] || return 0
    local b64
    if [ "$ENGINE" = firefox ]; then
        # sessionstore.jsonlz4 is firefox's "open tabs" state: keep it
        b64=$(tar -czf - -C "$PROFILE_DIR" \
            --exclude='*/cache2' \
            --exclude='*/startupCache' \
            --exclude='*/thumbnails' \
            --exclude='*/minidumps' \
            --exclude='*/.parentlock' \
            --exclude='*/lock' \
            . 2>/dev/null | base64 -w0) || return 0
    else
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
    fi
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
                # block browser startup ("profile appears to be in use")
                if [ "$ENGINE" = firefox ]; then
                    rm -f "$PROFILE_DIR"/*/.parentlock "$PROFILE_DIR"/*/lock
                else
                    rm -f "$PROFILE_DIR"/SingletonLock "$PROFILE_DIR"/SingletonCookie "$PROFILE_DIR"/SingletonSocket
                fi
                log "restored profile snapshot from database"
            fi
        fi
    fi
    rm -f "$raw" "$tgz"
}

sig() {
    if [ "$ENGINE" = firefox ]; then
        local d
        d=$(dirname "$(history_db)")
        stat -c %Y "$d/places.sqlite" "$d/sessionstore.jsonlz4" "$d/sessionstore-backups/recovery.jsonlz4" 2>/dev/null | tr '\n' ' '
    else
        stat -c %Y "$PROFILE_DIR/Default/Sessions" "$PROFILE_DIR/Default/History" 2>/dev/null | tr '\n' ' '
    fi
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
    local db
    db=$(history_db)
    [ -s "$db" ] || return 0

    local members
    members=$(online_members)
    [ -n "$members" ] || return 0

    local wm=0
    [ -s "$WATERMARK_FILE" ] && wm=$(cat "$WATERMARK_FILE" 2>/dev/null || echo 0)

    # hot-copy the sqlite files so we don't fight the browser's WAL locks
    local tmpd
    tmpd=$(mktemp -d)
    local base
    base=$(basename "$db")
    cp "$db" "$tmpd/$base" 2>/dev/null
    [ -f "$db-wal" ] && cp "$db-wal" "$tmpd/$base-wal" 2>/dev/null
    [ -f "$db-shm" ] && cp "$db-shm" "$tmpd/$base-shm" 2>/dev/null

    local rows
    if [ "$ENGINE" = firefox ]; then
        # places.sqlite: visit_date is microseconds since the unix epoch
        rows=$(sqlite3 -readonly "$tmpd/$base" \
            "SELECT v.id, p.url, COALESCE(p.title, ''), datetime(v.visit_date/1000000, 'unixepoch') FROM moz_historyvisits v JOIN moz_places p ON p.id = v.place_id WHERE v.id > $wm ORDER BY v.id ASC LIMIT 500;" 2>/dev/null)
    else
        # History: visit_time is microseconds since 1601-01-01
        rows=$(sqlite3 -readonly "$tmpd/$base" \
            "SELECT visits.id, urls.url, COALESCE(urls.title, ''), datetime(visits.visit_time/1000000 - 11644473600, 'unixepoch') FROM visits JOIN urls ON urls.id = visits.url WHERE visits.id > $wm ORDER BY visits.id ASC LIMIT 500;" 2>/dev/null)
    fi
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
if [ ! -s "$(history_db)" ]; then
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
