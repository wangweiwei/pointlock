#!/usr/bin/env bash
# M1 acceptance demo (08 §6.2): capability closure (lock → bind →
# attestation), the real-daemon login run, and single-call failure
# location — scriptable end to end against the DeviceRail mock daemon.
#
# Prereqs: cargo build -p pointlock-cli
#          the sibling DeviceRail repo built:
#            cargo build --bin devicerail-daemon   (in ../device-rail)
#          or DEVICERAIL_DAEMON=/path/to/devicerail-daemon
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${POINTLOCK_BIN:-$ROOT/target/debug/pointlock}"
DAEMON="${DEVICERAIL_DAEMON:-$ROOT/../device-rail/target/debug/devicerail-daemon}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pointlock-m1-login.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
[ -x "$BIN" ] || { echo "pointlock binary not found at $BIN — run: cargo build -p pointlock-cli"; exit 64; }
[ -x "$DAEMON" ] || { echo "devicerail-daemon not found at $DAEMON — build the sibling repo or set DEVICERAIL_DAEMON"; exit 64; }

FLOW="$(dirname "$0")/m1-login.flow.yaml"
# The DeviceRail control plane has no asset byte channel (documented
# provider gap): omit screenshots so evidence localization stays green.
DAEMON_ARGS=(--daemon-cmd "$DAEMON" --daemon-env DEVICERAIL_SCREENSHOT_POLICY=omit)

echo "━━━ (1) capability closure: lock freezes the live world"
"$BIN" lock --provider devicerail --out "$WORK/real.lock.json" "${DAEMON_ARGS[@]}"

echo
echo "━━━ (1b) compile binds against the frozen capabilities"
"$BIN" compile "$FLOW" --provider devicerail --lockfile "$WORK/real.lock.json" --out "$WORK/login.ir.json"

echo
echo "━━━ (1c) attestation is fail-closed: a tampered lockfile refuses to run"
python3 - "$WORK/real.lock.json" "$WORK/tampered.lock.json" << 'PY'
import json, sys
lockfile = json.load(open(sys.argv[1]))
lockfile["hello"]["featuresEnabled"] = []
json.dump(lockfile, open(sys.argv[2], "w"))
PY
if "$BIN" run "$WORK/login.ir.json" --provider devicerail --store "$WORK/store" \
    --run-id tampered --lockfile "$WORK/tampered.lock.json" "${DAEMON_ARGS[@]}" 2>"$WORK/tampered.err"; then
  echo "a tampered lockfile must refuse to run"; exit 1
fi
echo "refused (as pinned): $(tail -1 "$WORK/tampered.err")"

echo
echo "━━━ (2) the login run against the real daemon: flow verdict pass"
"$BIN" run "$WORK/login.ir.json" --provider devicerail --store "$WORK/store" \
  --run-id m1-login --lockfile "$WORK/real.lock.json" "${DAEMON_ARGS[@]}"

echo
echo "━━━ (3) one-call failure location: the adjudicable dossier"
"$BIN" locate --store "$WORK/store" --run m1-login --step type_username --format json \
  | python3 -c 'import json,sys;d=json.load(sys.stdin);print(json.dumps({k:d[k] for k in ("stepId","runPath","verdict","state")},indent=2,ensure_ascii=False))'

echo
echo "acceptance items proven elsewhere: the dual-hash two acts (judgeDirty"
echo "offline re-judge / effectDirty rollback re-run) live in"
echo "crates/pointlock-cli/tests/e2e_m1.rs and crates/pointlock-runner/tests."
