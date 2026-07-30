#!/usr/bin/env bash
# M3a acceptance demo (08 §6.4): the 15-minute v0.1 closing narrative,
# single provider (the fake registration under the v0.1 provider name
# `devicerail`) end to end.
#
# The script prepares every artifact and run the narrative needs, starts
# `pointlock inspect --serve --ui`, and walks the operator through the
# eight acceptance items chapter by chapter — the UI-side observations
# (graph overlay, timeline filters, inbox, repair closure) happen in the
# browser; the terminal side of each chapter runs here.
#
# Prereqs: cargo build -p pointlock-cli   (or POINTLOCK_BIN=...)
#          pnpm --dir packages/ui build  (or POINTLOCK_UI_DIST=...)
#          python3 on PATH (the canned vision endpoint of chapter 5)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The 10k-event chapter wants the release binary: checkpoint
# materialization per append is O(view) (I1 discipline — cadence
# registered for v0.2 evaluation), so the generation run is minutes on
# release and much worse on debug.
if [ -z "${POINTLOCK_BIN:-}" ] && [ -x "$ROOT/target/release/pointlock" ]; then
  BIN="$ROOT/target/release/pointlock"
else
  BIN="${POINTLOCK_BIN:-$ROOT/target/debug/pointlock}"
fi
UI_DIST="${POINTLOCK_UI_DIST:-$ROOT/packages/ui/dist}"
PORT="${POINTLOCK_DEMO_PORT:-43110}"
VISION_PORT="${POINTLOCK_DEMO_VISION_PORT:-43199}"
TIMELINE_N="${POINTLOCK_DEMO_TIMELINE_N:-2000}"
WORK="${POINTLOCK_DEMO_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/pointlock-m3a-demo.XXXXXX")}"

banner() { printf '\n\033[1;36m━━━ %s\033[0m\n' "$*"; }
note()   { printf '\033[0;33m%s\033[0m\n' "$*"; }
pause()  { printf '\n\033[1m[enter 继续]\033[0m '; read -r; }

[ -x "$BIN" ] || { echo "pointlock binary not found at $BIN — run: cargo build -p pointlock-cli"; exit 64; }
[ -f "$UI_DIST/index.html" ] || { echo "UI dist not found at $UI_DIST — run: pnpm --dir packages/ui build"; exit 64; }

mkdir -p "$WORK/flows" "$WORK/artifacts"
STORE="$WORK/store"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT

banner "准备：lock + 五个 flow 编译（工作目录 ${WORK}）"

"$BIN" lock --provider fake --out "$WORK/fake.lock.json" >/dev/null

# Chapter 1: a multi-step "login" flow that passes — the live-follow run.
cat > "$WORK/flows/login-demo.flow.yaml" <<'YAML'
flow: login_demo
provider: devicerail
params:
  username: { schema: { type: string }, required: true }
steps:
  - id: open_login
    invoke: { action: tapElement, args: { element: login_button } }
    effect: mutating
    idempotent: true
  - id: type_username
    invoke:
      action: setElementValue
      args: { element: username_field, value: "${{ params.username }}" }
    effect: mutating
    expect_schema:
      type: object
      properties:
        value: { type: string }
  - id: submit
    invoke: { action: tapElement, args: { element: submit_button } }
    effect: mutating
  - id: read_back
    invoke: { action: findElement, args: { element: home_banner } }
    effect: readonly
    expect:
      - assert_id: username_typed
        expr: ${{ eq(steps.type_username.output.value, params.username) }}
YAML

# Chapter 2: the repair-closure flow — the expected literal is wrong on
# purpose, so read_back fails and the operator repairs it from the UI.
cat > "$WORK/flows/repair-demo.flow.yaml" <<'YAML'
flow: repair_demo
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: set_ssid
    invoke:
      action: setElementValue
      args: { element: ssid_field, value: "${{ params.ssid }}" }
    effect: mutating
    expect_schema:
      type: object
      properties:
        value: { type: string }
  - id: read_back
    invoke: { action: findElement, args: { element: ssid_field } }
    effect: readonly
    expect:
      - assert_id: ssid_was_typed
        expr: ${{ eq(steps.set_ssid.output.value, "WrongName") }}
YAML

# Chapter 3: the human-gate flow — a machine `fail` escalates to a human
# judge (M2 escalate handler), which parks the run in the inbox.
cat > "$WORK/flows/human-demo.flow.yaml" <<'YAML'
flow: human_demo
provider: devicerail
handlers:
  on_fail:
    - escalate:
        mode: judge
        prompt: the machine judged checkout fail; please rule
        decisions: [pass, fail, unknown]
        timeout_ms: 3600000
        on_timeout: unknown
      max_triggers: 1
steps:
  - id: checkout
    invoke: { action: tapElement, args: { element: checkout_button } }
    effect: mutating
    expect:
      - assert_id: never_true
        expr: ${{ eq(1, 2) }}
YAML

# Chapter 4: composition — a foreach (aggregated tally) plus a subflow
# call whose inner step fails (collapsed node shows the aggregate red).
cat > "$WORK/flows/compose-callee.flow.yaml" <<'YAML'
flow: probe_inner
provider: devicerail
steps:
  - id: inner_probe
    invoke: { action: findElement, args: { element: missing_widget } }
    effect: readonly
    expect:
      - assert_id: inner_never_true
        expr: ${{ eq(1, 2) }}
YAML
cat > "$WORK/flows/compose-demo.flow.yaml" <<'YAML'
flow: compose_demo
provider: devicerail
params:
  fields: { schema: { type: array }, required: true }
steps:
  - id: fill_each
    foreach:
      in: ${{ params.fields }}
      as: field
      steps:
        - id: fill_one
          invoke:
            action: setElementValue
            args: { element: bulk_field, value: "${{ iter.field }}" }
          effect: mutating
  - id: probe_session
    call: ./compose-callee.flow.yaml
    inputs: {}
YAML

# Chapter 6: the pagination flow — a wide foreach generating a 10k+
# event ledger (bounded DTOs + the 50/page hard cap must stay smooth).
cat > "$WORK/flows/timeline-demo.flow.yaml" <<'YAML'
flow: timeline_demo
provider: devicerail
params:
  items: { schema: { type: array }, required: true }
steps:
  - id: churn
    foreach:
      in: ${{ params.items }}
      as: item
      steps:
        - id: churn_one
          invoke:
            action: setElementValue
            args: { element: churn_field, value: "${{ iter.item }}" }
          effect: mutating
YAML

compile() {
  "$BIN" compile "$WORK/flows/$1" --lockfile "$WORK/fake.lock.json" \
    --out "$WORK/artifacts/$2" >/dev/null
}
compile login-demo.flow.yaml    login-demo.flow.ir.json
compile repair-demo.flow.yaml   repair-demo.flow.ir.json
compile human-demo.flow.yaml    human-demo.flow.ir.json
compile compose-demo.flow.yaml  compose-demo.flow.ir.json
compile timeline-demo.flow.yaml timeline-demo.flow.ir.json
cp "$ROOT/examples/vision-demo.flow.yaml" "$WORK/flows/"
compile vision-demo.flow.yaml   vision-demo.flow.ir.json
note "5+1 个 flow 编译完成 → $WORK/artifacts"

# An empty store so the host has something to open before the first run.
"$BIN" run "$WORK/artifacts/login-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param username=warmup --run-id warmup-0 >/dev/null || true

# The 10k+ event ledger of chapter 6 is generated up front so the
# narrative never stalls on it ($TIMELINE_N iterations ≈ 5 events each;
# ~2-3 minutes on a release binary — see the binary note above).
note "生成 10k+ 事件 run（timeline-1，约 2-3 分钟，验收 6 用）..."
ITEMS="$(python3 -c "import json; print(json.dumps([f'v{i}' for i in range($TIMELINE_N)]))")"
"$BIN" run "$WORK/artifacts/timeline-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param "items=$ITEMS" --run-id timeline-1 >/dev/null

banner "启动投影 host（08 §1 裁决：Rust CLI 即 host，@pointlock/ui 纯 SPA）"
"$BIN" inspect --serve --store "$STORE" --artifacts "$WORK/artifacts" \
  --ui "$UI_DIST" --port "$PORT" > "$WORK/serve.out" 2>&1 &
PIDS+=($!)
sleep 1
URL="$(grep -o "http://127.0.0.1:$PORT/?token=[a-f0-9]*" "$WORK/serve.out" | head -1)"
[ -n "$URL" ] || { echo "serve did not start:"; cat "$WORK/serve.out"; exit 70; }
echo "浏览器打开: $URL"
note "验收 1（前半）：flow 列表应已列出全部已编译 flow 与 warmup run。"
pause

banner "验收 1：CLI 发起 login run，UI 实时跟进"
note "回到浏览器停在 run 列表/详情——回车后 CLI 启动 login_demo，"
note "观察：frontier 蓝色动效推进 → 全绿；timeline 五类过滤各自正确；"
note "SSE 断开后（可临时断网验证）收件箱/revision 轮询无缝接替。"
pause
"$BIN" run "$WORK/artifacts/login-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param username=alice --run-id login-1
pause

banner "验收 2 + 7：失败 → 红节点 → 案卷 → YAML 深链 → UI 修复闭环（R13 负向验收）"
set +e
"$BIN" run "$WORK/artifacts/repair-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param ssid=HomeWifi --run-id repair-1
note "exit=$?（fail=1 符合预期）"
set -e
note "在 UI 中走完整修复闭环，全程不碰终端："
note "  红节点 read_back → 检查器案卷（attempts/observations/断言 reason/evidence 画廊）"
note "  → 「在 YAML 中打开」应落在 $WORK/flows/repair-demo.flow.yaml 的断言行"
note "  → 把 \"WrongName\" 改成 \"HomeWifi\" 保存"
note "  → 「重编译」→「对齐预演」：set_ssid reusable、read_back judgeDirty（蓝，离线重判不重派）"
note "  → 「从 checkpoint 继续」→ 图转绿。"
note "验收 7（R13 修复提议的 UI 呈现）就在同一面板上核对："
note "  · YAML 改动（人改或 nl-drafter 起草的 patch 同一入口——编辑器里的源文件）"
note "  · 重编译结果与 alignmentReport 预演即『提议呈现』：哪些历史保留(reusable)、"
note "    哪些离线重判(judgeDirty)、哪些重跑(effectDirty/new)、哪些需确认"
note "  · 负向断言：页面上不存在批准提交控件（webUi 不收响应，06 §4.2）——"
note "    批准 = 人在编辑器保存 + 点「从 checkpoint 继续」这一 CLI 等价动作，"
note "    UI 实时跟进 resume 与图状态推进。"
pause

banner "验收 3：human 闸门 → 收件箱 → human-cli 回应 → 续跑"
set +e
"$BIN" run "$WORK/artifacts/human-demo.flow.ir.json" --store "$STORE" \
  --provider fake --run-id human-1
note "exit=$?（3=挂起等待 human，符合预期）"
set -e
note "UI 收件箱应出现红点：prompt/presents/倒计时/回应指引齐全。"
note "回车后按指引经 CLI 通道回应（提示时输入 pass）——"
note "store 写入 humanResponded 后收件箱实时消项，run 续跑。"
pause
"$BIN" resume "$WORK/artifacts/human-demo.flow.ir.json" --store "$STORE" \
  --run human-1 --provider fake --interactive
pause

banner "验收 4：subflow 折叠聚合 verdict + foreach 聚合计数"
set +e
"$BIN" run "$WORK/artifacts/compose-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param 'fields=["a","b","c"]' --run-id compose-1
note "exit=$?（1=子流内部失败上浮，符合预期）"
set -e
note "UI：foreach 节点聚合计数 3 pass；call 折叠节点聚合红，"
note "展开后内部失败步 inner_probe 可直达检查器。"
pause

banner "验收 5：vision 链尾——可用产出真实 pass/fail，不可用降级 unknown（原则 7）"
python3 - "$VISION_PORT" <<'PY' &
import http.server, json, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('content-length', 0)))
        body = json.dumps({"id": "msg", "type": "message", "role": "assistant",
                           "content": [{"type": "text",
                                        "text": "PASS: the field visibly shows the requested name"}],
                           "stop_reason": "end_turn"}).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PY
PIDS+=($!)
sleep 0.5
note "· vision 可用（canned 判定端点代真实 API——fake 截图非真 PNG；"
note "  对真实 provider 只需 ANTHROPIC_API_KEY，同一 flag 直连 Anthropic）："
ANTHROPIC_API_KEY=demo-key ANTHROPIC_BASE_URL="http://127.0.0.1:$VISION_PORT" \
  "$BIN" run "$WORK/artifacts/vision-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param ssid=HomeWifi --vision anthropic --run-id vision-pass-1
note "· vision 不可用（不带 --vision）→ visual 断言 unknown，flow verdict unknown（exit 2）："
set +e
"$BIN" run "$WORK/artifacts/vision-demo.flow.ir.json" --store "$STORE" \
  --provider fake --param ssid=HomeWifi --run-id vision-off-1
note "exit=$?（2=unknown 符合预期）"
set -e
note "UI 对比两个 run 的 field_visibly_filled 断言 reason。"
pause

banner "验收 6：10k+ 事件 run 的 timeline 分页（50/页硬上限 + 有界 DTO）"
# No pipe: inspect prints thousands of step lines and would die on EPIPE
# under pipefail if head closed the pipe early.
"$BIN" inspect --store "$STORE" --run timeline-1 > "$WORK/timeline-inspect.txt" 2>&1
head -3 "$WORK/timeline-inspect.txt"
note "UI 打开 timeline-1（准备阶段已生成）的 timeline，翻页应保持流畅。"
pause

banner "验收 8：投影协议边界（R14）——fixtures + 适配层单测代验"
note "UI 与 store 之间只经投影协议；该项由测试链代验，不在浏览器里演："
note "  cargo test -p pointlock-store          # golden fixtures（协议侧）"
note "  pnpm --dir packages/projection-types test   # 字面量 satisfies 检查"
note "  pnpm --dir packages/ui test                 # 适配层单测（不 import store 内部类型）"

banner "叙事完毕"
echo "store:    $STORE"
echo "UI:       $URL"
echo "工作目录: ${WORK}（脚本退出即停 host；目录保留供复查）"
