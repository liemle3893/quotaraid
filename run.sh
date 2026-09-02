#!/usr/bin/env bash
# Bring up the hub and this machine's agent. Token lives in .env (gitignored).
set -eu
cd "$(dirname "$0")"
[ -f .env ] || { echo "no .env — make one with QUOTARAID_TOKEN=..."; exit 1; }
set -a; . ./.env; set +a
B=./target/release/quotaraid
[ -x "$B" ] || cargo build --release
"$B" hub --listen 0.0.0.0:7777 &
HUB=$!
trap 'kill $HUB 2>/dev/null' EXIT
until curl -sf http://127.0.0.1:7777/panel >/dev/null 2>&1; do :; done
"$B" agent --hub ws://127.0.0.1:7777/ingest
