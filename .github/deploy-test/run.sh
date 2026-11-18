#!/usr/bin/env bash
# Runs deploy.yml's VM script, exactly as the SSH action does (`bash -c
# "<script>"`), against a simulated VM: a git origin with release tags, a
# local stand-in for GitHub's release downloads (binaries and frontend
# builds), the server in tmux pane 0:0, a `node index.js` bridge and a live
# frontend/dist. Linux only (it reads /proc), so CI runs it: see
# deploy-test.yml.
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

# A git origin with the releases, and the VM's checkout at the first.
git config --global user.email deploy-test@example.com
git config --global user.name deploy-test
git config --global init.defaultBranch main
git init -q --bare "$WORK/origin.git"
git clone -q "$WORK/origin.git" "$WORK/seed" 2>/dev/null
mkdir -p "$WORK/seed/backend" "$WORK/seed/kotak-bridge" "$WORK/seed/frontend"
touch "$WORK/seed/backend/.keep"
echo 'setInterval(() => {}, 1000);' > "$WORK/seed/kotak-bridge/index.js"
cp "$ROOT/frontend/.gitignore" "$WORK/seed/frontend/.gitignore"
RELEASES=(200 201 202 203 204)
for v in "${RELEASES[@]}"; do
  echo "$v" > "$WORK/seed/RELEASE"
  git -C "$WORK/seed" add -A
  git -C "$WORK/seed" commit -qm "release $v"
  git -C "$WORK/seed" tag "server-v0.1.$v"
done
git -C "$WORK/seed" push -q origin main --tags
git clone -q "$WORK/origin.git" "$HOME/auto-trader"
git -C "$HOME/auto-trader" reset -q --hard server-v0.1.200
BACKEND="$HOME/auto-trader/backend"
FRONT="$HOME/auto-trader/frontend"

# Release downloads, all the stand-in server; 0.1.202 is made to crash below.
# Each has a frontend build whose index.html is its version, except 0.1.203,
# like a release from before the frontend was built.
for v in "${RELEASES[@]}"; do
  mkdir -p "$WORK/releases/server-v0.1.$v"
  cp "$FAKE_BIN" "$WORK/releases/server-v0.1.$v/server-0.1.$v-$TRIPLE"
  [ "$v" = 203 ] && continue
  mkdir -p "$WORK/frontend-$v/assets"
  echo "$v" > "$WORK/frontend-$v/index.html"
  echo "app $v" > "$WORK/frontend-$v/assets/app-$v.js"
  tar -czf "$WORK/releases/server-v0.1.$v/frontend-0.1.$v.tar.gz" -C "$WORK/frontend-$v" .
done
# The live frontend, 0.1.200's.
mkdir -p "$FRONT/dist"
echo 200 > "$FRONT/dist/index.html"
(cd "$WORK" && exec python3 -m http.server 9000 --bind 127.0.0.1 >/dev/null 2>&1) &
export RELEASES_BASE="http://127.0.0.1:9000/releases"
export HEALTH_TIMEOUT=60
export REPO_SLUG=test/test

asset() { echo "server-${1#server-v}-$TRIPLE"; }
digest() { echo "sha256:$(sha256sum "$WORK/releases/$1/$(asset "$1")" | cut -d ' ' -f1)"; }
# The release's frontend build, and its digest; empty when it has none.
frontend_asset() {
  local name="frontend-${1#server-v}.tar.gz"
  if [ -f "$WORK/releases/$1/$name" ]; then echo "$name"; fi
}
frontend_digest() {
  local name
  name=$(frontend_asset "$1")
  if [ -n "$name" ]; then echo "sha256:$(sha256sum "$WORK/releases/$1/$name" | cut -d ' ' -f1)"; fi
}
# The version whose frontend build is live, and the one kept before it.
frontend() { cat "$FRONT/dist/index.html" 2>/dev/null || echo none; }
previous_frontend() { cat "$FRONT/.dist_prev/index.html" 2>/dev/null || echo none; }
version() { curl -sf --max-time 3 http://127.0.0.1:8080/api/version 2>/dev/null | grep -o '"version":"[^"]*"' | cut -d '"' -f 4 || true; }
server_pid() { pgrep '^server-[0-9]' | head -n1 || true; }
bridges() { pgrep -fx 'node index.js' | wc -l | tr -d ' '; }
wait_for() {
  for _ in $(seq 1 "${2:-40}"); do
    [ "$(version)" = "$1" ] && return 0
    sleep 0.5
  done
  echo "FAIL: $1 never came up (now '$(version)')"
  exit 1
}
check() {
  if [ "$2" = "$3" ]; then echo "ok: $1"; else echo "FAIL: $1: expected '$3', got '$2'"; exit 1; fi
}
# Runs the deploy as the SSH action does; prints only its exit code (its log
# goes to stderr, the CI log). An empty digest argument means GitHub's.
deploy() {
  local tag="$1" dig="${2:-$(digest "$1")}" fdig="${3:-$(frontend_digest "$1")}" rc=0
  TAG="$tag" ASSET="$(asset "$tag")" DIGEST="$dig" \
    FRONTEND_ASSET="$(frontend_asset "$tag")" FRONTEND_DIGEST="$fdig" \
    bash -c "$SCRIPT" >&2 || rc=$?
  echo "$rc"
}

# The live server, 0.1.200, in tmux pane 0:0, and its bridge.
cp "$FAKE_BIN" "$BACKEND/$(asset server-v0.1.200)"
tmux new-session -d -s 0 -x 200 -y 50
tmux send-keys -t 0:0 "cd $BACKEND && ./$(asset server-v0.1.200)" C-m
(cd "$HOME/auto-trader/kotak-bridge" && exec node index.js) &
wait_for 0.1.200
check "bridge running" "$(bridges)" 1

echo "--- 1. A download that doesn't match GitHub's checksum is refused before anything stops"
before=$(server_pid)
check "deploy fails" "$(deploy server-v0.1.201 sha256:0000000000000000000000000000000000000000000000000000000000000000 | tail -n1)" 1
check "old version still answers" "$(version)" 0.1.200
check "server not restarted" "$(server_pid)" "$before"
check "source not moved" "$(cat "$HOME/auto-trader/RELEASE")" 200
check "frontend not moved" "$(frontend)" 200

echo "--- 1b. A frontend build that doesn't match GitHub's checksum is refused before anything stops"
check "deploy fails" "$(deploy server-v0.1.201 "" sha256:0000000000000000000000000000000000000000000000000000000000000000 | tail -n1)" 1
check "old version still answers" "$(version)" 0.1.200
check "server not restarted" "$(server_pid)" "$before"
check "source not moved" "$(cat "$HOME/auto-trader/RELEASE")" 200
check "frontend not moved" "$(frontend)" 200
check "nothing left staged" "$(test -e "$FRONT/.dist_new" && echo yes || echo no)" no

echo "--- 2. A good release goes live, with its frontend build"
check "deploy succeeds" "$(deploy server-v0.1.201 | tail -n1)" 0
check "new version answers" "$(version)" 0.1.201
check "source follows the release" "$(cat "$HOME/auto-trader/RELEASE")" 201
check "frontend follows the release" "$(frontend)" 201
check "frontend assets unpacked" "$(cat "$FRONT/dist/assets/app-201.js")" "app 201"
check "previous frontend kept" "$(previous_frontend)" 200
check "bridge stopped" "$(bridges)" 0
check "new and previous binaries kept" "$(ls "$BACKEND" | grep -c '^server-')" 2
check "previous binary backed up" "$(test -f "$BACKEND/.server_prev" && echo yes)" yes
check "frontend/ clean for git" "$(git -C "$HOME/auto-trader" status --porcelain -- frontend)" ""

echo "--- 3. The same release again does nothing"
before=$(server_pid)
check "re-deploy succeeds" "$(deploy server-v0.1.201 | tail -n1)" 0
check "server not restarted" "$(server_pid)" "$before"
check "frontend still the release's" "$(frontend)" 201
check "previous frontend still kept" "$(previous_frontend)" 200

echo "--- 3b. The same release again puts its frontend build back, without a restart"
echo broken > "$FRONT/dist/index.html"
check "re-deploy succeeds" "$(deploy server-v0.1.201 | tail -n1)" 0
check "server not restarted" "$(server_pid)" "$before"
check "frontend repaired" "$(frontend)" 201

echo "--- 4. A release that never comes up is rolled back, quickly, frontend too"
touch "$BACKEND/$(asset server-v0.1.202).crash"
started=$SECONDS
check "deploy fails" "$(deploy server-v0.1.202 | tail -n1)" 1
check "previous version answers again" "$(version)" 0.1.201
check "source rolled back" "$(cat "$HOME/auto-trader/RELEASE")" 201
check "frontend rolled back" "$(frontend)" 201
if [ $((SECONDS - started)) -ge "$HEALTH_TIMEOUT" ]; then echo "FAIL: a crashing release took the whole timeout"; exit 1; fi
echo "ok: rolled back in $((SECONDS - started)) s"

echo "--- 5. With the tmux session gone (a reboot), it is made again; a release without a frontend build leaves it"
tmux kill-server
sleep 1
check "deploy succeeds" "$(deploy server-v0.1.203 | tail -n1)" 0
check "new version answers" "$(version)" 0.1.203
check "frontend left as it was" "$(frontend)" 201

echo "--- 6. A dropped SSH connection doesn't stop the deploy halfway"
setsid bash -c "TAG=server-v0.1.204 ASSET=$(asset server-v0.1.204) DIGEST=$(digest server-v0.1.204) FRONTEND_ASSET=$(frontend_asset server-v0.1.204) FRONTEND_DIGEST=$(frontend_digest server-v0.1.204) bash -c \"\$0\"" "$SCRIPT" >/dev/null 2>&1 &
outer=$!
sleep 2
kill -- "-$outer" 2>/dev/null || true
wait_for 0.1.204 120
check "source follows the release" "$(cat "$HOME/auto-trader/RELEASE")" 204
check "frontend follows the release" "$(frontend)" 204

echo "All deploy checks passed."
tmux kill-server || true
