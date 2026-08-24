#!/bin/bash
# ---------------------------------------------------------------------------
# Server self-update script.
#
# Triggered detached (via `setsid`) by POST /api/update_server, or run by hand.
#
# PATH CONTRACT — the server binary MUST be started from ~/auto-trader/backend,
# because at runtime it resolves these paths RELATIVE to its working directory:
#   - frontend static files : ../frontend/dist   (see server/src/main.rs)
#   - websocket bridge dir   : ../kotak-bridge    (see kotak_client/src/websocket.rs)
# Do NOT start the binary from any other directory or those will break.
#
# Design:
#   - Binaries are built by CI (.github/workflows/release-server.yml) and
#     downloaded here, NOT compiled on this VM. This is a small VM that also
#     runs the live 50ms trading-tick loop; a `cargo build --release` here
#     would compete with it for CPU/memory and risks OOM-killing the whole
#     box mid-build. Keep the build off this box.
#   - The frontend is NOT built here. Production frontend traffic is served
#     by Vercel (see README's split-deployment guide), not by this server's
#     ../frontend/dist static fallback, so building it on every update was
#     pure wasted time/CPU for an artifact nothing in production reads.
#   - Everything slow/fragile (release lookup, download) runs BEFORE the
#     running server is touched, so live-order-monitoring downtime is
#     limited to the final stop -> swap -> restart -> health-check window.
#   - The previously-running binary is backed up before being replaced. If
#     the new binary fails its post-restart health check, we roll back to it
#     automatically rather than leaving the live account unattended.
#   - `set -e` is intentionally NOT used: a single non-fatal step must never
#     abort the whole update. Steps whose failure would leave us running
#     unknown/broken code (git sync, binary download) are checked explicitly
#     and are fatal.
#   - All output is logged to /tmp/update.log for post-mortem debugging.
# ---------------------------------------------------------------------------
set -uo pipefail

LOG="/tmp/update.log"
exec >"$LOG" 2>&1
echo "[$(date '+%F %T')] ===== Update started ====="

REPO="$HOME/auto-trader"
BACKEND="$REPO/backend"
TMUX_PANE="0:0"
HEALTH_URL="http://127.0.0.1:8080/api/health"
BACKUP_BIN="$BACKEND/.server_prev"

# ---------------------------------------------------------------------------
# 1. Sync source to origin/main (hard reset — resilient to a dirty tree, e.g.
#    the deleted session.json left behind by the update handler).
#    A failed sync means we don't reliably know what code we're about to run
#    next update, so — unlike the rest of this script — these are fatal.
# ---------------------------------------------------------------------------
cd "$REPO" || { echo "FATAL: cannot cd to $REPO"; exit 1; }
if ! git fetch origin --tags --prune; then
    echo "FATAL: git fetch failed"
    exit 1
fi
if ! git reset --hard origin/main; then
    echo "FATAL: git reset --hard failed"
    exit 1
fi
echo "Source now at: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"

# ---------------------------------------------------------------------------
# 2. Fetch latest release binary info.
#    VERSION_TAG  = the GitHub tag,   e.g. server-v0.1.48
#    ASSET_NAME   = the file on disk, e.g. server-0.1.48-x86_64-unknown-linux-gnu
#    DOWNLOAD_URL = full download URL for the linux-gnu asset
# ---------------------------------------------------------------------------
echo "Fetching latest release info from GitHub..."
LATEST_JSON=$(curl -s https://api.github.com/repos/MrImmortal09/auto-trader/releases/latest)
VERSION_TAG=$(echo "$LATEST_JSON" | grep -o '"tag_name": *"[^"]*"' | head -n 1 | cut -d '"' -f 4)
DOWNLOAD_URL=$(echo "$LATEST_JSON" | grep -o '"browser_download_url": *"[^"]*"' | grep 'x86_64-unknown-linux-gnu' | head -n 1 | cut -d '"' -f 4)
# Extract just the filename from the download URL (e.g. server-0.1.48-x86_64-unknown-linux-gnu)
ASSET_NAME=$(basename "$DOWNLOAD_URL")

if [ -z "$DOWNLOAD_URL" ] || [ -z "$VERSION_TAG" ] || [ -z "$ASSET_NAME" ]; then
    echo "FATAL: could not resolve latest release download URL / tag / asset name."
    echo "  VERSION_TAG=$VERSION_TAG"
    echo "  DOWNLOAD_URL=$DOWNLOAD_URL"
    echo "  ASSET_NAME=$ASSET_NAME"
    exit 1
fi

echo "Latest: $VERSION_TAG  →  asset: $ASSET_NAME"

TMP_BIN="/tmp/$ASSET_NAME"
echo "Downloading from $DOWNLOAD_URL ..."
if ! curl -fsSL "$DOWNLOAD_URL" -o "$TMP_BIN"; then
    echo "FATAL: binary download failed. Old server left untouched."
    exit 1
fi
chmod +x "$TMP_BIN"

# Final resting place of the binary (named by asset, inside backend/).
TARGET_BIN="$BACKEND/$ASSET_NAME"

# ---------------------------------------------------------------------------
# 3. Back up whatever binary is currently live, so a failed health check
#    below can restart the account on known-good code instead of nothing.
# ---------------------------------------------------------------------------
CURRENT_BIN=$(ls -t "$BACKEND"/server-*-x86_64-unknown-linux-gnu 2>/dev/null | head -n 1 || true)
if [ -n "$CURRENT_BIN" ]; then
    echo "Backing up current binary ($CURRENT_BIN) -> $BACKUP_BIN"
    cp "$CURRENT_BIN" "$BACKUP_BIN"
fi

# ---------------------------------------------------------------------------
# 4. Stop the running server + any orphaned websocket bridge processes.
#    The server runs in the foreground of tmux pane 0:0 as ./server-X.Y.Z...
#    This is where real downtime starts.
# ---------------------------------------------------------------------------
echo "Stopping existing server..."
tmux send-keys -t "$TMUX_PANE" C-c 2>/dev/null || true
sleep 2
# Kill any leftover server binary (matches ./server-0.x.y... or ./server-vX...)
pkill -f './server-' 2>/dev/null || true
# Kill orphaned Node websocket bridge(s); the new server respawns them on connect.
pkill -f 'node index.js' 2>/dev/null || true
sleep 1

# ---------------------------------------------------------------------------
# 5. Install the new binary and restart.
# ---------------------------------------------------------------------------
echo "Installing new binary at $TARGET_BIN ..."
mkdir -p "$BACKEND"
mv "$TMP_BIN" "$TARGET_BIN"
chmod +x "$TARGET_BIN"

echo "Starting new server in tmux..."
tmux send-keys -t "$TMUX_PANE" "cd $BACKEND && ./$ASSET_NAME" C-m

# ---------------------------------------------------------------------------
# 6. Health-check the new server (unauthenticated GET /api/health). If it
#    never comes up, roll back to the backed-up binary rather than leaving
#    the live account with no server attending to open positions.
# ---------------------------------------------------------------------------
echo "Waiting for new server to become healthy..."
HEALTHY=false
for i in $(seq 1 15); do
    sleep 2
    if curl -sf "$HEALTH_URL" >/dev/null 2>&1; then
        HEALTHY=true
        break
    fi
    echo "  ...not up yet (attempt $i/15)"
done

if [ "$HEALTHY" = true ]; then
    echo "New server is healthy ($VERSION_TAG / $ASSET_NAME)."
    # Clean up older release binaries — keep only the new one and the backup
    # of what was previously running, so disk doesn't grow unbounded.
    for f in "$BACKEND"/server-*-x86_64-unknown-linux-gnu; do
        [ "$f" = "$TARGET_BIN" ] && continue
        [ "$f" = "$CURRENT_BIN" ] && continue
        rm -f "$f"
    done
    echo "[$(date '+%F %T')] ===== Update complete ($VERSION_TAG / $ASSET_NAME) ====="
else
    echo "FATAL: new server ($ASSET_NAME) failed health check after 30s."
    if [ -n "$CURRENT_BIN" ] && [ -f "$BACKUP_BIN" ]; then
        echo "Rolling back to previous binary: $CURRENT_BIN"
        tmux send-keys -t "$TMUX_PANE" C-c 2>/dev/null || true
        sleep 2
        pkill -f './server-' 2>/dev/null || true
        cp "$BACKUP_BIN" "$CURRENT_BIN"
        chmod +x "$CURRENT_BIN"
        tmux send-keys -t "$TMUX_PANE" "cd $BACKEND && ./$(basename "$CURRENT_BIN")" C-m
        echo "Rollback restart issued for $(basename "$CURRENT_BIN"). Verify manually — this needs human eyes on a live account."
    else
        echo "No previous binary available to roll back to. Manual intervention required NOW — the account may be unattended."
    fi
    echo "[$(date '+%F %T')] ===== Update FAILED, rollback attempted ====="
    exit 1
fi
