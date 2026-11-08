#!/usr/bin/env bash
# Runs deploy.yml's VM script, exactly as the SSH action does (`bash -c
# "<script>"`), against a simulated VM: a git origin with release tags, a
# local stand-in for GitHub's release downloads, the server in tmux pane 0:0
# and a `node index.js` bridge. Linux only (it reads /proc), so CI runs it:
# see deploy-test.yml.
#
#   bash .github/deploy-test/run.sh <compiled fake_server.rs>
#
# Everything lives under a temporary HOME; nothing outside it is touched.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
FAKE_BIN=$(realpath "$1")
TRIPLE=x86_64-unknown-linux-gnu
WORK=$(mktemp -d)
export HOME="$WORK/home"
mkdir -p "$HOME"

SCRIPT=$(ruby -ryaml -e 'y = YAML.load_file(ARGV[0]); puts y["jobs"]["deploy"]["steps"].find { |s| s["name"] == "Deploy on the VM" }["with"]["script"]' "$ROOT/.github/workflows/deploy.yml")

# A git origin with three releases, and the VM's checkout at the first.
git config --global user.email deploy-test@example.com
git config --global user.name deploy-test
git config --global init.defaultBranch main
git init -q --bare "$WORK/origin.git"
git clone -q "$WORK/origin.git" "$WORK/seed"
mkdir -p "$WORK/seed/backend" "$WORK/seed/kotak-bridge"
echo 'setInterval(() => {}, 1000);' > "$WORK/seed/kotak-bridge/index.js"
for v in 200 201 202; do
  echo "$v" > "$WORK/seed/RELEASE"
  git -C "$WORK/seed" add -A
  git -C "$WORK/seed" commit -qm "release $v"
  git -C "$WORK/seed" tag "server-v0.1.$v"
done
git -C "$WORK/seed" push -q origin main --tags
git clone -q "$WORK/origin.git" "$HOME/auto-trader"
git -C "$HOME/auto-trader" reset -q --hard server-v0.1.200
BACKEND="$HOME/auto-trader/backend"

# Release downloads: 0.1.201 is good, 0.1.202 never comes up.
mkdir -p "$WORK/releases/server-v0.1.201" "$WORK/releases/server-v0.1.202"
cp "$FAKE_BIN" "$WORK/releases/server-v0.1.201/server-0.1.201-$TRIPLE"
printf '#!/bin/sh\nexit 1\n' > "$WORK/releases/server-v0.1.202/server-0.1.202-$TRIPLE"
(cd "$WORK" && exec python3 -m http.server 9000 --bind 127.0.0.1 >/dev/null 2>&1) &
export RELEASES_BASE="http://127.0.0.1:9000/releases"

version() { curl -sf http://127.0.0.1:8080/api/version 2>/dev/null | grep -o '"version":"[^"]*"' | cut -d '"' -f 4 || true; }
wait_for() {
  for _ in $(seq 1 20); do
    [ "$(version)" = "$1" ] && return 0
    sleep 0.5
  done
  echo "FAIL: $1 never came up"
  exit 1
}
check() {
  if [ "$2" = "$3" ]; then echo "ok: $1"; else echo "FAIL: $1: expected '$3', got '$2'"; exit 1; fi
}
# Prints only the script's exit code; its log goes to stderr (the CI log).
deploy() {
  local rc=0
  TAG="$1" ASSET="server-${1#server-v}-$TRIPLE" REPO_SLUG=test/test bash -c "$SCRIPT" >&2 || rc=$?
  echo "$rc"
}
server_pid() { pgrep '^server-[0-9]' | head -n1 || true; }

# The live server, 0.1.200, in tmux pane 0:0, and its bridge.
cp "$FAKE_BIN" "$BACKEND/server-0.1.200-$TRIPLE"
tmux new-session -d -s 0 -x 200 -y 50
tmux send-keys -t 0:0 "cd $BACKEND && ./server-0.1.200-$TRIPLE" C-m
(cd "$HOME/auto-trader/kotak-bridge" && exec node index.js) &
wait_for 0.1.200
check "bridge running" "$(pgrep -fx 'node index.js' | wc -l | tr -d ' ')" 1

echo "--- 1. A good release goes live"
check "deploy succeeds" "$(deploy server-v0.1.201 | tail -n1)" 0
check "new version answers" "$(version)" 0.1.201
check "source follows the release" "$(cat "$HOME/auto-trader/RELEASE")" 201
check "bridge stopped" "$(pgrep -fx 'node index.js' | wc -l | tr -d ' ')" 0
check "new and previous binaries kept" "$(ls "$BACKEND" | grep -c '^server-')" 2
check "previous binary backed up" "$(test -f "$BACKEND/.server_prev" && echo yes)" yes

echo "--- 2. The same release again does nothing"
before=$(server_pid)
check "re-deploy succeeds" "$(deploy server-v0.1.201 | tail -n1)" 0
check "server not restarted" "$(server_pid)" "$before"

echo "--- 3. A release that never comes up is rolled back"
check "deploy fails" "$(deploy server-v0.1.202 | tail -n1)" 1
check "previous version answers again" "$(version)" 0.1.201
check "source rolled back" "$(cat "$HOME/auto-trader/RELEASE")" 201

echo "All deploy checks passed."
tmux kill-server || true
