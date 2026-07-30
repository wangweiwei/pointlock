#!/usr/bin/env bash
# M0 acceptance demo (08 §6.1): compile determinism, the 3-step pass,
# and the ledger's event-order discipline — scriptable end to end.
#
# Prereq: cargo build -p pointlock-cli   (or POINTLOCK_BIN=...)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${POINTLOCK_BIN:-$ROOT/target/debug/pointlock}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pointlock-m0-hello.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
[ -x "$BIN" ] || { echo "pointlock binary not found at $BIN — run: cargo build -p pointlock-cli"; exit 64; }

FLOW="$(dirname "$0")/m0-hello.flow.yaml"

echo "━━━ (1) compile determinism: same input, byte-identical irHash"
"$BIN" lock --provider fake --out "$WORK/fake.lock.json"
"$BIN" compile "$FLOW" --provider fake --lockfile "$WORK/fake.lock.json" --out "$WORK/a.ir.json"
"$BIN" compile "$FLOW" --provider fake --lockfile "$WORK/fake.lock.json" --out "$WORK/b.ir.json"
HASH_A=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["root"]["irHash"] if "root" in json.load(open(sys.argv[1])) else json.load(open(sys.argv[1]))["irHash"])' "$WORK/a.ir.json")
HASH_B=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["root"]["irHash"] if "root" in json.load(open(sys.argv[1])) else json.load(open(sys.argv[1]))["irHash"])' "$WORK/b.ir.json")
[ "$HASH_A" = "$HASH_B" ] || { echo "irHash not deterministic: $HASH_A vs $HASH_B"; exit 1; }
echo "irHash stable: $HASH_A"

echo
echo "━━━ (2) the 3-step run: judged, flow verdict pass"
"$BIN" run "$WORK/a.ir.json" --provider fake --store "$WORK/store" --run-id m0-hello

echo
echo "━━━ (3) ledger inspection: spine §6.1/§6.2 event order"
"$BIN" inspect --store "$WORK/store" --run m0-hello --rebuild-checkpoint

echo
echo "acceptance items proven elsewhere: fail/unverified variants and the"
echo "kill -9 crash windows live in crates/pointlock-cli/tests/e2e_m0.rs and"
echo "crates/pointlock-runner/tests; the codegen gate in behavioral_equivalence.rs."
