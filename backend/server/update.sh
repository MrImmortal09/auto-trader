#!/bin/bash
set -euo pipefail

echo "Starting server update process..."

# 1. Fetch latest release info
echo "Fetching latest release info from GitHub..."
LATEST_JSON=$(curl -s https://api.github.com/repos/MrImmortal09/auto-trader/releases/latest)
DOWNLOAD_URL=$(echo "$LATEST_JSON" | grep -o '"browser_download_url": *"[^"]*"' | grep 'x86_64-unknown-linux-gnu' | head -n 1 | cut -d '"' -f 4)

if [ -z "$DOWNLOAD_URL" ]; then
    echo "Error: Could not find the download URL for the latest release."
    exit 1
fi

echo "Downloading binary from $DOWNLOAD_URL ..."
TMP_BIN="/tmp/server-latest"
curl -sL "$DOWNLOAD_URL" -o "$TMP_BIN"
chmod +x "$TMP_BIN"

TARGET_DIR="$HOME/auto-trader/backend/server"
TARGET_BIN="$TARGET_DIR/server-bin"

# 2. Stop existing server
echo "Stopping existing server..."
pkill -f "server-bin" || true
tmux send-keys -t 0:0 C-c
sleep 2

# build the frontend
echo "Building frontend..."
FRONTEND="$HOME/auto-trader/frontend"
if cd "$FRONTEND"; then
    git pull || echo "Warning: git pull failed"
    pnpm install || echo "Warning: npm install failed"
    pnpm run build || echo "Warning: npm run build failed"
else
    echo "Warning: could not cd to $FRONTEND"
fi

# 3. Replace binary
echo "Replacing binary..."
mkdir -p "$TARGET_DIR"
mv "$TMP_BIN" "$TARGET_BIN"

# 4. Restart server in tmux pane 0:0
echo "Starting new server in tmux..."
tmux send-keys -t 0:0 "cd $TARGET_DIR && ./server-bin" C-m

echo "Update complete!"
