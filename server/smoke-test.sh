#!/usr/bin/env bash
# Smoke test for clipcloak-server: boots the release binary against a local model and
# asserts the HTTP contract the extension depends on — health, bearer-token auth
# enforcement (secure-by-default), and actual redaction. Run in CI on every push
# and locally with:
#
#   ORT_DYLIB_PATH=/path/to/libonnxruntime.so PII_MODELS_DIR=./semplifica \
#     bash server/smoke-test.sh
#
# Requires the release binary at server/target/release/clipcloak-server.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/server/target/release/clipcloak-server"
PORT="${PII_PORT:-8731}"
TOKEN="smoke-secret-token"
ORIGIN="chrome-extension://ihjamhkkcgbifajnbikldcjfamggnbaj"
LOG="$(mktemp)"

: "${PII_MODELS_DIR:=$ROOT/semplifica}"
export PII_MODELS_DIR PII_PORT="$PORT" PII_TOKEN="$TOKEN"

[ -x "$BIN" ] || { echo "missing binary: $BIN (run cargo build --release)"; exit 1; }

"$BIN" > "$LOG" 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true; rm -f "$LOG"' EXIT

# Model load is cold here (~30s). Wait for the listen line, bail if it dies.
for _ in $(seq 1 180); do
  if grep -q "Listening on" "$LOG" 2>/dev/null; then break; fi
  if ! kill -0 "$SRV" 2>/dev/null; then echo "server exited early:"; cat "$LOG"; exit 1; fi
  sleep 1
done
grep -q "Listening on" "$LOG" || { echo "server never bound:"; cat "$LOG"; exit 1; }

base="http://127.0.0.1:$PORT"
fail() { echo "SMOKE FAIL: $1"; echo "--- server log ---"; cat "$LOG"; exit 1; }

# 1. health is open
code=$(curl -s -o /dev/null -w "%{http_code}" "$base/health")
[ "$code" = "200" ] || fail "health expected 200, got $code"

# 2. no token -> 401 (secure by default)
code=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$base/classify" \
  -H "Origin: $ORIGIN" -H "Content-Type: application/json" -d '{"text":"x"}')
[ "$code" = "401" ] || fail "missing-token expected 401, got $code"

# 3. wrong token -> 401
code=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$base/classify" \
  -H "Origin: $ORIGIN" -H "Authorization: Bearer wrong" \
  -H "Content-Type: application/json" -d '{"text":"x"}')
[ "$code" = "401" ] || fail "wrong-token expected 401, got $code"

# 4. correct token -> 200 with email + phone redacted
resp=$(curl -s -X POST "$base/classify" \
  -H "Origin: $ORIGIN" -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"text":"Email me at jane.doe@example.com or call 083 555 1234."}')
echo "$resp" | grep -q '\[EMAIL\]'     || fail "expected [EMAIL] in: $resp"
echo "$resp" | grep -q '\[PHONE_NUM\]' || fail "expected [PHONE_NUM] in: $resp"

echo "SMOKE OK"
