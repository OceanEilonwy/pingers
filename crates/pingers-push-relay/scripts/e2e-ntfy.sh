#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# End-to-end test of pingers-push-relay against a local ntfy (a UnifiedPush distributor).
#
# 1. Starts ntfy on loopback with an empty configuration and an in-memory cache.
# 2. Builds and starts the relay binary with a throwaway configuration: the RFC 7748 test key
#    pair as key 1, a test server credential, UnifiedPush allowed to reach loopback, and
#    counts logging every second (so the run shows what an operator would see).
# 3. Runs the ignored end-to-end tests (tests/relay/e2e_ntfy.rs): a device seals its ntfy
#    endpoint to the relay key for one mailbox, the "server" forwards blob and bind to the
#    relay, and the content-free push is read back from ntfy's subscribe API.
#
# Usage: crates/pingers-push-relay/scripts/e2e-ntfy.sh
# Environment:
#   NTFY_BIN    ntfy binary (default: the newest under <repo>/../tools/ntfy/)
#   NTFY_PORT   default 18080
#   RELAY_PORT  default 18443
#   CARGO_BUILD_JOBS is passed through to cargo.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
ntfy_bin=${NTFY_BIN:-$(ls -d "$repo"/../tools/ntfy/ntfy_*_linux_amd64/ntfy 2>/dev/null | sort -V | tail -n 1)}
ntfy_port=${NTFY_PORT:-18080}
relay_port=${RELAY_PORT:-18443}

if [[ -z "$ntfy_bin" || ! -x "$ntfy_bin" ]]; then
  echo "e2e: no ntfy binary; download a release into $repo/../tools/ntfy/ or set NTFY_BIN" >&2
  exit 2
fi

work=$(mktemp -d)
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

wait_for() {
  local url=$1 name=$2
  for _ in $(seq 1 100); do
    if curl -fsS -o /dev/null "$url" 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  echo "e2e: $name did not come up at $url" >&2
  exit 1
}

echo "e2e: ntfy $("$ntfy_bin" --help >/dev/null 2>&1; basename "$(dirname "$ntfy_bin")") on 127.0.0.1:$ntfy_port"
: >"$work/server.yml"
"$ntfy_bin" serve --config "$work/server.yml" --listen-http "127.0.0.1:$ntfy_port" \
  --base-url "http://127.0.0.1:$ntfy_port" >"$work/ntfy.log" 2>&1 &
pids+=($!)
wait_for "http://127.0.0.1:$ntfy_port/v1/health" ntfy

echo "e2e: building pingers-push-relay"
cargo build --quiet --manifest-path "$repo/Cargo.toml" -p pingers-push-relay
relay_bin="$repo/target/debug/pingers-push-relay"

# The credential, hash key and key pair must match tests/relay/common.rs. credential_hash is
# HMAC-SHA-256(hash_key, "pingers/v1/relay-credential" || credential), as config.rs documents.
cat >"$work/relay.toml" <<EOF
listen = "127.0.0.1:$relay_port"
operator = "Pingers e2e"
logging = "counts"
counts_interval_s = 1
hash_key = "4242424242424242424242424242424242424242424242424242424242424242"

[[keys]]
key_id = 1
private_key = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"
public_key = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
activated_at = 0

[[servers]]
server_id = "e2e"
credential_hash = "ff19a6fd804c602f550f469ee2bd1f12cfa347489606c8cc963003753091c20d"
pushes_per_minute = 600

[unifiedpush]
allow_insecure_local_endpoints = true
EOF
chmod 600 "$work/relay.toml"

echo "e2e: relay on 127.0.0.1:$relay_port"
"$relay_bin" "$work/relay.toml" 2>"$work/relay.log" &
pids+=($!)
wait_for "http://127.0.0.1:$relay_port/v1/config" relay

status=0
NTFY_URL="http://127.0.0.1:$ntfy_port" RELAY_URL="http://127.0.0.1:$relay_port" \
  cargo test --manifest-path "$repo/Cargo.toml" -p pingers-push-relay --test relay \
  -- --ignored --test-threads 1 e2e_ntfy || status=$?

sleep 1.2
echo "e2e: relay output (counts only):"
sed 's/^/  /' "$work/relay.log"
echo "e2e: messages on ntfy:"
for topic in upPingersFakeOpener upPingersSealed; do
  curl -fsS "http://127.0.0.1:$ntfy_port/$topic/json?poll=1&since=all" | sed "s/^/  $topic: /" || true
done
exit "$status"
