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
# 2. Fetch latest release binary.
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
    echo "FATAL: binary download failed."
    exit 1
fi
chmod +x "$TMP_BIN"

# Final resting place of the binary (named by asset, inside backend/).
TARGET_BIN="$BACKEND/$ASSET_NAME"

# ---------------------------------------------------------------------------
# 3. Stop the running server + any orphaned websocket bridge processes.
#    The server runs in the foreground of tmux pane 0:0 as ./server-X.Y.Z...
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
# 4. Build the frontend (already synced by the hard reset above).
#    Run with CI=true + --ignore-scripts to suppress interactive pnpm prompts
#    (pnpm update banners block stdin and cause the script to hang).
# ---------------------------------------------------------------------------
echo "Building frontend..."
if cd "$FRONTEND"; then
    CI=true pnpm install --ignore-scripts 2>&1 || echo "WARN: pnpm install failed"
    pnpm run build 2>&1                        || echo "WARN: pnpm run build failed"
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
#    Use the ASSET_NAME (e.g. server-0.1.48-x86_64-unknown-linux-gnu), not the tag.
# ---------------------------------------------------------------------------
echo "Starting new server in tmux..."
tmux send-keys -t "$TMUX_PANE" "cd $BACKEND && ./$ASSET_NAME" C-m

echo "[$(date '+%F %T')] ===== Update complete ($VERSION_TAG / $ASSET_NAME) ====="
