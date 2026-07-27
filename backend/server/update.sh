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
# Notes:
#   - `set -e` is intentionally NOT used: a single non-fatal step (e.g. a warn)
#     must never abort the whole update and leave the server stopped.
#   - All output is logged to /tmp/update.log for post-mortem debugging.
# ---------------------------------------------------------------------------
set -uo pipefail

LOG="/tmp/update.log"
exec >"$LOG" 2>&1
echo "[$(date '+%F %T')] ===== Update started ====="

REPO="$HOME/auto-trader"
BACKEND="$REPO/backend"
FRONTEND="$REPO/frontend"
TMUX_PANE="0:0"

# ---------------------------------------------------------------------------
# 1. Sync source to origin/main (hard reset — resilient to a dirty tree, e.g.
#    the deleted session.json left behind by the update handler).
# ---------------------------------------------------------------------------
cd "$REPO" || { echo "FATAL: cannot cd to $REPO"; exit 1; }
git fetch origin --tags --prune || echo "WARN: git fetch failed"
git reset --hard origin/main    || echo "WARN: git reset --hard failed"
echo "Source now at: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"

# ---------------------------------------------------------------------------
# 2. Fetch latest release binary
# ---------------------------------------------------------------------------
echo "Fetching latest release info from GitHub..."
LATEST_JSON=$(curl -s https://api.github.com/repos/MrImmortal09/auto-trader/releases/latest)
DOWNLOAD_URL=$(echo "$LATEST_JSON" | grep -o '"browser_download_url": *"[^"]*"' | grep 'x86_64-unknown-linux-gnu' | head -n 1 | cut -d '"' -f 4)
VERSION_TAG=$(echo "$LATEST_JSON" | grep -o '"tag_name": *"[^"]*"' | head -n 1 | cut -d '"' -f 4)

if [ -z "$DOWNLOAD_URL" ] || [ -z "$VERSION_TAG" ]; then
    echo "FATAL: could not resolve latest release download URL / tag."
    exit 1
fi

echo "Downloading $VERSION_TAG binary from $DOWNLOAD_URL ..."
TMP_BIN="/tmp/$VERSION_TAG"
if ! curl -fsSL "$DOWNLOAD_URL" -o "$TMP_BIN"; then
    echo "FATAL: binary download failed."
    exit 1
fi
chmod +x "$TMP_BIN"

# Binary lives in and is launched from backend/ (see PATH CONTRACT above).
TARGET_BIN="$BACKEND/$VERSION_TAG"

# ---------------------------------------------------------------------------
# 3. Stop the running server + any orphaned websocket bridge processes.
#    The server runs in the foreground of tmux pane 0:0 as `./server-vX.Y.Z`.
# ---------------------------------------------------------------------------
echo "Stopping existing server..."
tmux send-keys -t "$TMUX_PANE" C-c 2>/dev/null || true
sleep 2
# Belt-and-suspenders: the process shows up as `./server-v...` (relative path).
pkill -f './server-v' 2>/dev/null || true
# Kill orphaned Node websocket bridge(s); the new server respawns them on connect.
pkill -f 'node index.js' 2>/dev/null || true
sleep 1

# ---------------------------------------------------------------------------
# 4. Build the frontend (already synced by the hard reset above).
# ---------------------------------------------------------------------------
echo "Building frontend..."
if cd "$FRONTEND"; then
    pnpm install   || echo "WARN: pnpm install failed"
    pnpm run build || echo "WARN: pnpm run build failed"
else
    echo "WARN: could not cd to $FRONTEND"
fi

# ---------------------------------------------------------------------------
# 5. Install the new binary.
# ---------------------------------------------------------------------------
echo "Installing new binary at $TARGET_BIN ..."
mkdir -p "$BACKEND"
mv "$TMP_BIN" "$TARGET_BIN"
chmod +x "$TARGET_BIN"

# ---------------------------------------------------------------------------
# 6. Restart the server in tmux pane 0:0 — MUST run from backend/ (PATH CONTRACT).
# ---------------------------------------------------------------------------
echo "Starting new server in tmux..."
tmux send-keys -t "$TMUX_PANE" "cd $BACKEND && ./$VERSION_TAG" C-m

echo "[$(date '+%F %T')] ===== Update complete ($VERSION_TAG) ====="
