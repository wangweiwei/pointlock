#!/usr/bin/env bash
# M2 acceptance demo (08 §6.3): the human-gate lifecycle, scriptable end
# to end — run to the gate, the process EXITS (detached mode), the
# pending request survives on the ledger, the human answers through the
# cli channel on resume, the run finishes pass with the ledger
# continuous (one runId, request/response paired).
#
# Prereq: cargo build -p pointlock-cli   (or POINTLOCK_BIN=...)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${POINTLOCK_BIN:-$ROOT/target/debug/pointlock}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/pointlock-m2-checkout.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
[ -x "$BIN" ] || { echo "pointlock binary not found at $BIN — run: cargo build -p pointlock-cli"; exit 64; }

FLOW="$(dirname "$0")/m2-checkout.flow.yaml"

echo "━━━ (0) lock + compile"
"$BIN" lock --provider fake --out "$WORK/fake.lock.json"
"$BIN" compile "$FLOW" --provider fake --lockfile "$WORK/fake.lock.json" --out "$WORK/checkout.ir.json"

echo
echo "━━━ (1) run to the gate: foreach adds 3 items, then awaitingHuman"
echo "    (the runner process exits — 'kill the runner' is the detached"
echo "    mode's normal life; exit code 3 = suspended on a human)"
set +e
"$BIN" run "$WORK/checkout.ir.json" --provider fake --store "$WORK/store" --run-id m2-checkout
CODE=$?
set -e
[ "$CODE" = 3 ] || { echo "expected exit 3 (awaiting human), got $CODE"; exit 1; }

echo
echo "━━━ (2) next day: the pending request is on the ledger"
"$BIN" inspect --store "$WORK/store" --run m2-checkout

echo
echo "━━━ (3) respond via the cli channel and resume to pass"
printf 'ship\n' | "$BIN" resume "$WORK/checkout.ir.json" --provider fake \
  --store "$WORK/store" --run m2-checkout --interactive

echo
echo "━━━ (4) the report: who judged what, when (actor = cli:os:<user>@<host>)"
"$BIN" report --store "$WORK/store" --run m2-checkout

echo
echo "acceptance items proven elsewhere: escalate/maxTriggers, onResumeDrift"
echo "repair, macro origin trace, supervision ordering and the NL/repair"
echo "loops live in crates/pointlock-cli/tests/e2e_m2.rs, e2e_serve.rs and"
echo "crates/pointlock-runner/tests/handlers.rs (+ packages/nl-drafter tests)."
