#!/usr/bin/env bash
# End-to-end smoke test: runs the monitor against the local docker cluster and
# checks its HTTP endpoints. Expects the cluster to be up (`make up`) and the
# binary to be built (`cargo build`).
set -euo pipefail

BIN=${BIN:-target/debug/weaviate-monitor}
PORT=${PORT:-3999}
WEAVIATE_URL=${WEAVIATE_URL:-http://localhost:8080}
WEAVIATE_API_KEY=${WEAVIATE_API_KEY:-readonly-key}

pids=()
cleanup() {
  for pid in "${pids[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT

failures=0
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; failures=$((failures + 1)); }

# start <port> <weaviate url>: runs the monitor in the background and waits for it.
start() {
  BIND_ADDR="127.0.0.1:$1" WEAVIATE_URL="$2" WEAVIATE_API_KEY="$WEAVIATE_API_KEY" \
    RUST_LOG=warn "$BIN" &
  pids+=($!)
  if ! curl -sf --retry 30 --retry-connrefused --retry-delay 1 -o /dev/null "http://127.0.0.1:$1/healthz"; then
    echo "monitor on port $1 did not start" >&2
    exit 1
  fi
}

# expect <name> <url> <http code> <json status>
expect() {
  local body code
  body=$(curl -s -w '\n%{http_code}' "$2")
  code=${body##*$'\n'}
  body=${body%$'\n'*}
  if [ "$code" = "$3" ] && grep -q "\"status\":\"$4\"" <<<"$body"; then
    pass "$1 -> $code $4"
  else
    fail "$1 -> expected $3 $4, got $code: $body"
  fi
}

echo "Smoke test against $WEAVIATE_URL"

start "$PORT" "$WEAVIATE_URL"
expect "GET /healthz" "http://127.0.0.1:$PORT/healthz" 200 ok
expect "GET /api/liveness (cluster up)" "http://127.0.0.1:$PORT/api/liveness" 200 up

# A second instance pointed at a port with nothing listening must report down.
dead_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
start $((PORT + 1)) "http://127.0.0.1:$dead_port"
expect "GET /api/liveness (weaviate unreachable)" "http://127.0.0.1:$((PORT + 1))/api/liveness" 503 down

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "All checks passed"
