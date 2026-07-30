# Pointlock 03：YAML Authoring 格式与编译链路

> 本文是 Pointlock 设计文档系列第 3 篇（文件编号 03），骨架见 `00-architecture-spine.md`。覆盖需求产出 4（authoring 格式与示例）与产出 5（编译链路）。
>
> **事实来源声明**：原定必读输入《需求与目标》《DeviceRail 真实接口事实报告》在编排时路径未注入（字面量 `undefined`）。本文全部 DeviceRail 事实与命名以骨架 §附录 A（已逐项对照 DeviceRail 源仓库核实）为准；凡骨架词汇表未覆盖而本文必需的词，逐一标注「待骨架收编」并汇总于 §5。

---

## 1. YAML Authoring 格式规范

### 1.0 地位声明（原则 1/2 的落点）

YAML 是**作者界面**，不是执行协议。它的全部使命是在 `parse` 阶段被消化为 AST；`normalize` 之后 YAML 不复存在，runner 的入口签名只接受 `FlowIR`。因此本节的每一条语法都必须回答同一个问题：**它消解为 IR 的哪个部分**。回答只有两种：

- **一一对应（1:1）**：YAML 键与 IR 字段同构，编译只做重命名（snake_case → camelCase）与校验；
- **语法糖（sugar）**：在 `normalize`/`bind` 阶段被消解，IR 中没有对应物（或对应物形态不同）。

完整对照见 §1.10。**不存在第三种**：任何「YAML 里有但既不映射也不消解」的键都是编译错误（fail-closed，未知键拒绝）。

### 1.1 文件与顶层结构

- 一个文件一个 flow，扩展名约定 `*.flow.yaml`；subflow 以相对路径引用，编译期按 `irHash` 锁定。
- 顶层键封闭（骨架 A.7）：`flow` `provider` `params` `outputs` `steps` `handlers` `macros` `verdict_policy`。未知顶层键 → 编译错误（无 `x-` 扩展区，减面）。

```yaml
flow: <flow-id>                  # → FlowIR.flowId；[a-z][a-z0-9_-]* 
provider: devicerail             # → FlowIR.provider.name；v0.1 唯一合法值
verdict_policy: standard         # → FlowIR.verdictPolicy；standard | strict；默认 standard
params:                          # → FlowIR.params: ParamDecl[]
  <name>: { schema: <JSON Schema>, required: <bool>, default: <值>? }
outputs:                         # → FlowIR.outputs: OutputDecl[]
  <name>: { schema: <JSON Schema>, from: "${{ <expr> }}" }
macros: { ... }                  # 编译期模板，见 §1.7；IR 中蒸发
handlers: { ... }                # flow 级钩子，见 §1.8 → FlowIR.handlers
steps: [ ... ]                   # → FlowIR.body: StepIR[]
```

`params`/`outputs` 的 `schema` 是 JSON Schema Draft 2020-12（与 DeviceRail `ActionDefinition.inputSchema` 同方言），编译期做元校验（schema 本身非法 → 编译错误）。

### 1.2 Step 语法：head-key 分派

每个 step 是一个 map：`id` + 恰好一个 **head key** + 若干通用键。head key 决定 step kind，分派顺序封闭：

1. **动词键**（8 个，骨架 A.4 `CanonicalVerb`）：`tap` `set_value` `clear` `wait_for` `find` `observe` `screenshot` `invoke` → `ActionStepIR`；
2. **结构键**：`call` → `CallStepIR`；`human` → `HumanStepIR`；`if` → `IfStepIR`；`foreach` → `ForeachStepIR`；`let` → `LetStepIR`；
3. **无 head key 但有 `expect`** → `AssertStepIR`（独立断言步，`observe: "fresh"`）；
4. **flow 本地 macro 名**（`macros:` 中定义）→ 编译期展开（§1.7）。

head key 缺失、多于一个、或与封闭词汇/其他 macro 名冲突 → 编译错误。`id` 必填，格式 `[a-z][a-z0-9_]*`；**`:` 分段保留给编译器合成 id**（schema `StepId` 文法：作者只能写首段，`:` 分段属编译器——macro 展开的 hygiene 前缀与 handler 内嵌 human step 共用这一机制，见 §1.7 与 02 §3），作者 id 中不得出现 `:` 或 `.`。

通用键（骨架 A.7 step 通用清单，全部 1:1 映射 `StepBase`）：

| YAML | IR | 说明 |
|---|---|---|
| `id` | `stepId` | flow 内唯一、稳定；修复时不改 id 即保留历史身份（resume 对齐的锚） |
| `preflight` | `preflight: AssertionIR[]` | **前置/resume 世界探针**；语法同 `expect` 的断言列表 |
| `expect` | `assertions: AssertionIR[]` | **后置断言**。与 `preflight` 永不混用（骨架 R9） |
| `retry` | `retry: RetryPolicy` | 子键 `max_attempts` `backoff_ms` `retry_on`；只作用于 act 阶段 |
| `timeout_ms` | `timeoutMs` | |
| `effect` | `effect: EffectClass` | 动词键可省略（编译器推导，见 §1.3）；`invoke` 必填 |
| `idempotent` | `idempotent` | 作者声明；reconcile 不确定分支与 `action_timed_out` 自动重试的许可 |
| `checkpoint` | `checkpoint` | 默认 `true`；macro 展开体内默认 `false` |
| `on_fail` `on_unknown` `on_error` `on_resume_drift` | `handlers: HandlerBinding[]` | step 级钩子，覆盖 flow 级（§1.8） |

### 1.3 动词步骤与 selector 约定

动词是 YAML 层的最后一站：`bind` 阶段经 provider manifest 的 `VerbBinding`（声明式 `argMap`，零代码执行）翻译为原生 `actionName`，**编译后动词从 IR 消失**（骨架 R7、A.6）。DeviceRail 绑定：`tap→tapElement`、`set_value→setElementValue`、`clear→clearElement`、`wait_for→waitForElement`、`find→findElement`（协议五件套，feature `device.semanticActions.v1`）。

统一形状：凡指向元素的动词，selector 一律挂在 `element` 子键下（与断言语法同构，减少作者心智负担）：

```yaml
- id: enter_username
  set_value:
    element: { identifier: "com.example.shop:id/username_input" }
    value: ${{ params.username }}

- id: tap_login
  tap:
    element: { role: button, name: "登录" }

- id: wait_home
  wait_for:
    element: { identifier: "com.example.shop:id/home_tab" }
    state: visible            # WaitForElementCondition: present|visible|enabled|absent
```

**selector map 与 DeviceRail `ElementSelector` 同构，字段名原样透传**（camelCase，透传规则优先于 YAML snake_case 规则）：`context?: { contextKind: native|web, contextId? }`、`role?`、`name?`、`identifier?`、`text?: { value, mode: exact|contains, caseSensitive? }`、`value?`、`css?`。不同定位通道消费不同字段：`dom` 消费 `css`；`uiTree` 消费 `role`/`name`/`identifier`/`text`；`coordinate` 不消费 selector（消费静态 `coordinate` 键）。这是 fallback 链合法性检查的素材（§4.3）。

非元素动词：

```yaml
- id: capture_state
  observe: [screenshot, uiSnapshot]     # 列表形式 = ProviderSession.observe 的 wants
- id: captcha_shot
  screenshot: {}                        # observe 的单要素快捷形
- id: launch_app
  invoke:                               # 逃逸门（骨架 A.6）：driver 专有 action 一律 invoke
    action: launchApp                   # 原生 action 名，以 lockfile.device.actions 为准
    args: { packageName: "com.example.shop" }   # 实参容器定名 args（M2 收编）
  effect: mutating                      # invoke 必须显式声明 effect
  idempotent: true
```

规则钉死：

- **effect 推导**：`tap`/`set_value`/`clear` → `mutating`；`wait_for`/`find`/`observe`/`screenshot` → `readonly`；`invoke` 无推导，必须显式写 `effect`（漏写 → 编译错误）。显式声明与推导冲突 → 编译错误（不静默采信任一方）。
- **invoke 的 output 类型**：无 `outputSchema` 声明即 `unknown` 类型；下游要取字段必须先用 `expect_schema` 显式收窄（骨架 §7），否则 `check` 阶段拒绝。
- `observe`/`screenshot` 编译为 `effect: readonly` 的 `ActionStepIR`，但在 devicerail 侧的执行目标是 `device.observe` RPC 而非 `device.execute` action——`VerbBinding` 需要允许 RPC 目标（**待骨架收编**，见 §5-11）。其 output：`{ observationId, screenshot?: EvidenceRef, uiSnapshot?: EvidenceRef }`（Evidence 在 observing 阶段已本地化）。
- `invoke` 子键 `action`（骨架 A.6 已示例）与实参容器 `args`（**M2 收编**：M0–M2 实现既定为 `args`，与原生动作"arguments"语义对齐；不复用 `inputs`——`call` 的 `inputs` 保持不变，两者语义不同：`inputs` 是 call-by-value 契约、`args` 是原生动作实参。已入骨架 A.7，见 §5-4）。

### 1.4 Fallback 声明：`locate_via` / `verify_via` / `coordinate`

原则 6/7 的作者界面。**没写就没有降级**：缺省 `locate_via` = 单 attempt，通道取 provider manifest 对该平台声明的首选定位通道（devicerail/android = `uiTree`）；缺省 `verify_via` = `[uiTree]`（同理）。runner 绝不即兴降级。

```yaml
- id: tap_login
  tap:
    element: { role: button, name: "登录", css: "#login-btn" }
  locate_via: [uiTree, coordinate]      # act-chain：[dom, uiTree, coordinate] 的子序列
  coordinate: { x: 540, y: 1650 }       # 链中出现 coordinate 则必填（静态坐标）
  expect:
    - element: { identifier: "com.example.shop:id/login_form" }
      state: absent
      verify_via: [uiTree, vision]      # verify-chain：[dom, uiTree, vision] 的子序列
      visual: "登录表单已从屏幕上消失"     # 链尾含 vision 时必填（见下）
```

钉死的合法性规则（`bind` 阶段强制，违反即编译错误）：

1. `locate_via` ⊆ `[dom, uiTree, coordinate]` 且保持该全序的子序列（不许 `[coordinate, uiTree]` 这种倒序）；`vision` 出现在 `locate_via` = 编译错误（视觉不能定位，类型层封死）。
2. `verify_via` ⊆ `[dom, uiTree, vision]` 子序列；`coordinate` 出现在 `verify_via` = 编译错误（坐标不能验证）。
3. `coordinate` 通道入链 ⟺ 静态 `coordinate: { x, y }` 键同在（互为充要）。
4. `dom` 入链 ⟹ selector 含 `css`；`uiTree` 入链 ⟹ selector 含 `role`/`name`/`identifier`/`text` 至少其一。
5. `vision` 只准出现在 `verify_via` 链尾，且该断言必须携带显式 `visual` 提示词——**编译器不自动生成视觉提示**（那是隐式降级语义，违反原则 6）。`visual` 键在此复用断言词汇（**待骨架收编**，见 §5-7）。
6. 每条链逐项做能力校验：通道须在 manifest `channels` 中声明且 `role` 相容（vision 只能 `verify`）、`requiresFeature` 须在 lockfile 可用集内（如 `uiTree` → `observation.uiSnapshot.v1`）。

`locate_via` 编译为 `ActionBinding.attempts: BoundAttempt[]`（每通道一个 attempt；`acceptExecutionModes` **逐 attempt 推导，只由该 attempt 自身通道决定**（02 §5.1 规则 6）：`dom`/`uiTree` attempt 恒 `["nativeSemantic","webSemantic"]`，`coordinate` attempt 恒 `["nativeSemantic","webSemantic","coordinateFallback"]`——作者写 `coordinate` 授权的是链上显式的坐标 attempt，**不是**语义 attempt 的内部坐标降级；语义 attempt 遇 daemon 内部 `coordinateFallback` 一律触发骨架 §6.4 R-degrade 处置，无论链上是否声明了坐标兜底）。`verify_via` 1:1 进 `AssertionIR.verifyVia`；非 visual 谓词断言的 `visual` 提示词 1:1 进 `AssertionIR.visionPrompt`（IR schema 以条件子句强制「链含 vision ⟺ visionPrompt 在场」，运行期由 VisionVerifier 逐字消费）。

**降级链的运行期语义提醒**（详见骨架 §6.3，作者必须理解才能正确使用）：verify-chain 只救「看不到」（omission/缺料 → 试下一通道），不救「不喜欢答案」——任何通道完成求值得出 fail 即终局；链耗尽仍无法求值 → `unknown`，绝不猜。

### 1.5 断言语法（`expect` 与 `preflight` 共用）

断言列表的每一项编译为一条 `AssertionIR`，四种谓词一一对应：

```yaml
expect:
  - element: { identifier: "com.example.shop:id/avatar" }     # predicate: elementState
    state: visible                                            # present|visible|enabled|absent
  - element: { identifier: "com.example.shop:id/welcome" }    # predicate: elementText
    text: { value: "欢迎回来", mode: contains }                # value 必须是静态字面量（见下）
  - expr: ${{ eq(steps.probe.output.matched, true) }}         # predicate: expr（纯表达式）
  - visual: "购物车角标显示数字 1"                              # predicate: visual（只能作降级验证
    region: { x: 900, y: 40, width: 160, height: 120 }        #   或人审素材；见下方约束）
    verify_via: [vision]
```

- **谓词静态性（钉死）**：`elementState`/`elementText`/`visual` 三型谓词的全部字段（selector、`state`、`text`/`value`、`visual` 提示词、`region`）在 IR 中是**静态字面量**——schema `PredicateIR` 没有任何 Expr 槽位，`TextMatchIR.value` 是 `minLength: 1` 的静态 string。`${{ }}` 出现在这些位置 → 编译错误（错误信息给出下述改写指引）。动态比较的**唯一**谓词表面是 `expr`（对 `params/vars/iter/steps.*.output/steps.*.verdict` 的纯表达式）。推论一：「屏幕文本 = 某个运行期值」在 v0.1 没有结构化谓词表面——合法手段只有 (i) 断言值编译期可知（含 macro 实参为字面量时的展开产物），(ii) 改写为对上游 step output 的 `expr` 断言，(iii) 交 `human` judge 裁决；`visual` 只能作显式声明的降级验证或人审素材（原则 7），不得独撑 pass。推论二：「字段为空」不可表达（`minLength: 1` 拒绝空串），`value: ""` 是编译错误。这不是权宜——动作实参（`ActionBinding.args`）是 Expr、断言谓词是字面量，正是「action 吃运行期值、assertion 是编译期钉死的问题」（原则 3）在类型层的形状。

- 每项可选 `verify_via`（§1.4 规则）与 `assert_id`（缺省由编译器按内容派生稳定 id，进 `AssertionIR.assertId` 与 RunPath 的 `assertion` 帧）。
- `value: <string>` 是 `elementText` 的糖：断言元素当前值与给定串 `exact` 匹配。受谓词静态性约束：给定串必须是静态非空字面量；macro 体内写 `value: ${{ macro.<param> }}` 仅当调用点实参是编译期字面量时合法（展开即静态），实参为运行期表达式 → 编译错误。
- **`visual` 谓词约束**：`predicate: visual` 的断言，其 `verify_via` 只能是 `[vision]`，且不允许作为 step 的唯一断言支撑一个 `pass`——除非同 step 存在结构化通道断言，或该步 verdict 交人裁决（`human` judge）。理由：原则 7，视觉不做主验证；v0.1 `pointlock-vision` 缺省实现返回 `unknown`，此约束保证流程在无视觉能力时仍诚实（unknown 而非假 pass）。
- `onMissingInput` 无 YAML 表面：固定 `"unknown"`（原则 4），不可配置。
- **`preflight` 与 `expect` 的辨析**（骨架 A.7 防漂移重点）：`preflight` 是「动手前世界应该长这样」的探针，resume 时被强制重放（漂移检测）；`expect` 是「动完手世界应该变成这样」的后置断言，参与 verdict 折叠。语法同构，语义永不混用。
- 独立断言步（无 head key，仅 `expect`）→ `AssertStepIR`，`observe: "fresh"`（执行时新拍 Observation）。`observe: { fromStep }` 变体 v0.1 无 YAML 表面，仅服务离线重判工具链。

### 1.6 结构步骤

**call（Subflow，一等公民，原则 9）**

```yaml
- id: do_login
  call: ./flows/login.flow.yaml        # 相对路径；normalize 阶段独立编译并按 irHash 锁定
  inputs:                              # call-by-value；callee 只见显式 inputs
    username: ${{ params.username }}
    password: ${{ params.password }}
```

→ `CallStepIR { flowRef: { flowId, irHash }, inputs }`。caller 只能引用 callee 声明的 `outputs`（`steps.do_login.output.<name>`）。call step 的 verdict = callee 的 flow verdict。同一 callee 在联编闭包内出现版本冲突（两处引用解析出不同 `irHash`）→ 编译错误。

**human（正式节点，原则 8）**

```yaml
- id: solve_captcha
  human:
    mode: provideInput                 # confirm | judge | provideInput | repairWorld
    prompt: "请识别验证码并输入文本"
    presents: [ "${{ steps.captcha_shot.output.screenshot }}" ]
    expect_schema:                     # provideInput 的输出契约 → HumanStepIR.outputSchema
      type: object                     #（复用 expect_schema 键，待骨架收编，见 §5-5）
      properties: { text: { type: string } }
      required: [text]
    on_timeout: unknown                # 固定值，可省略；写别的值 = 编译错误
  timeout_ms: 300000
```

`judge`/`confirm` 用 `decisions: [...]` 枚举选项；人的判定即 verdict，人的输入即 output（`steps.solve_captcha.output.text`），与机器产物同账本、同折叠规则。超时固定 `unknown`，绝不默认 pass/fail。

**if / foreach / let**

```yaml
- id: handle_captcha
  if: ${{ eq(steps.probe_captcha.output.matched, true) }}
  then: [ <steps> ]
  else: [ <steps> ]                    # 可省略；未选中分支的 step 记 skipped

- id: fill_each
  foreach:
    in: ${{ params.line_items }}
    as: item                           # 体内经 ${{ iter.item }} 引用
    steps: [ <steps> ]                 # 复用顶层 steps 键作循环体

- id: compose
  let:
    full_address: ${{ concat(params.street, ", ", params.city) }}   # vars.full_address，SSA 单赋值
```

### 1.7 Macros（编译期模板）

macro 是**编译期蒸发物**（骨架概念 4）：`normalize` 阶段卫生展开后彻底消失，无运行期身份、无独立 verdict、无独立 checkpoint，只留 origin trace 进 `sourceMap`。与 subflow 的分工三问见骨架 §2.1。

```yaml
macros:
  fill_field:                          # macro 名：flow 本地，不得与封闭词汇冲突
    params: [element, value]           # 编译期参数（复用 params 键；非运行期作用域）
    steps:
      - id: clear
        clear: { element: "${{ macro.element }}" }
      - id: type
        set_value: { element: "${{ macro.element }}", value: "${{ macro.value }}" }
        # 注意：这里刻意没有 expect。value 实参通常是运行期表达式（如 params.username），
        # 而断言谓词是静态字面量（§1.5 谓词静态性）——「输入内容 = 运行期值」没有结构化谓词
        # 表面。这两步在报告中记 unverified（骨架 R4），终局验证由流程级断言承担（§2.2）。

steps:
  - id: enter_username                 # 调用 = macro 名作 head key
    fill_field:
      element: { identifier: "com.example.shop:id/username_input" }
      value: ${{ params.username }}
```

钉死规则：

1. **参数引用 `${{ macro.<param> }}`**：编译期 AST 级替换（实参可以是任意 YAML 节点——上例 `element` 传入整个 selector map），展开后 IR 中不存在 `macro.*` 作用域（**待骨架收编**，见 §5-2）。
2. **hygiene 展开**：体内 step id 加前缀 `<调用点 id>:`（上例展开为 `enter_username:clear`、`enter_username:type`）——落在 schema `StepId` 为编译器保留的 `:` 分段文法内（02 §3），与 handler 内嵌 human step 的合成 id 同一机制。**推论（钉死）**：合成 id 永不出现在 ref 路径（`RefPath` 的 `StepIdRef` 文法不含 `:` 分段），因此 **macro 体内禁止经 `steps.<体内 id>.output/verdict` 跨步引用**——`normalize` 对指向体内 id 的 `steps.*` 引用直接拒绝（这同时封死了展开后引用被外层同名 step 捕获的可能）。体内引用 flow 体内在前的 step（`steps.<外部 id>.*`）不受影响；macro 体内步之间需要数据流 → 用 subflow，不要用 macro。同理，macro 体内步的 output 对调用点之后的 step 也不可见（合成 id 无 ref 表面）。
3. **禁递归**（含互递归），展开深度与展开后节点总数有上限，超限 fail-closed。
4. 展开体内 `checkpoint` 默认 `false`（骨架 §3 `StepBase`）；作者可在体内显式打开。
5. macro 无 verdict：展开出的 steps 各自判定。需要聚合判定、运行期身份或独立测试 → 用 subflow，不要用 macro。

### 1.8 Handlers

钩子四种（骨架 A.4）：`on_fail` `on_unknown` `on_error` `on_resume_drift`；flow 级与 step 级同构，step 级覆盖 flow 级（同 hook 时 step 级先触发且遮蔽）。每个 hook 键下是一个 binding（map）或 binding 列表；处置动作用 `Disposition` 枚举值作 head key：

```yaml
handlers:                              # flow 级
  on_error:
    - error_classes: [session_degraded]        # 仅 on_error 可过滤 ErrorClass
      escalate:                                # Disposition: escalate → 升级人机节点
        mode: repairWorld
        prompt: "会话降级，请检查设备后裁决"
        decisions: [adopt, redo, abort]
        timeout_ms: 600000
      max_triggers: 1

# step 级示例
- id: launch_app
  invoke: { action: launchApp, args: { packageName: "com.example.shop" } }
  effect: mutating
  idempotent: true
  on_fail:
    repair: ./flows/dismiss-permission-dialog.flow.yaml   # Disposition: repair → 修复 subflow
    max_triggers: 2                                       # 跑完修复流后本步重入（onFail 语义）
```

- 处置 head key 五选一：`retry`（值 = RetryPolicy 子键）| `continue`（值 `{}`）| `escalate`（值 = human 子键）| `abort`（值 `{}`）| `repair`（值 = subflow 路径，同 `call` 锁定 irHash）。
- `error_classes`、`max_triggers` 与五个处置 head key 为本文引入的 YAML 键（已收编骨架 A.7「handler 子键」行，见 §5-1）；IR 侧 `HandlerBinding.errorClasses/maxTriggers/action` 骨架已有。
- handler **没有可被数据流引用的输出**（骨架 R10）：`steps.*` 无法引用 handler 内部产物；执行留 StepRecord 审计痕（RunPath 带 `hook` 帧）。
- `on_fail`/`on_unknown` 响应 verdict，`on_error` 响应 `ErrorClass`，`on_resume_drift` 响应 resume 探针失败。AssertionFailure 不是错误（只走 `on_fail`）；`evidence_unavailable` 不是错误（是 verify-chain 降级触发器，最终体现为 unknown，走 `on_unknown`）。

**默认升级姿态（R13，00 §6.9 收编 1）**：标准 authoring 模板自带 flow 级 `handlers: on_unknown → escalate`（`max_triggers: 1`），使「拿不准就问人」成为默认姿态而非 opt-in。配套 lint：flow 对 unknown 无任何处置时，编译器发 **RF3xxx 段 warning（非 error）** 提示（§4.3 F6）——warning 意味着作者可有意识地删除该 handler，编译器不强制，但沉默的 unknown 必须是显式选择。

### 1.9 表达式、引用、锚点与环境变量约定

**表达式**：定界符 `${{ ... }}`（骨架 R8）；内部语法映射 `Expr`——引用 + 白名单纯函数 `eq ne not and or concat len coalesce jsonPath regexMatch`，非图灵完备（无循环、无自定义函数、无 I/O）。**引号规则（M2 收编）**：YAML flow context（`{}`/`[]` 集合）内的 `${{ }}` 表达式必须以引号包裹——表达式含 `{`/`}`/`,`，在 flow context 中不是合法 plain scalar（YAML 1.2 文法限制，saphyr/PyYAML 均拒）；block context 中可裸写。求值时机分两处（02 §8.3）：**实参位置**（动词/`invoke`/`call` 的实参、`if`/`foreach`/`let`/`presents`）在 step 进 `ready` 时一次性求值并快照入 `resolvedInputs`，resume 不重算；**断言位置**（`preflight` 与 `expect` 的 `expr` 谓词）分别在 `probing`/`asserting` 阶段求值，不进 `resolvedInputs`。

**作用域（封闭清单，骨架 §7）**：`params.*`、`env.*`、`steps.<id>.output.*`、`steps.<id>.verdict`、`vars.*`、`iter.<as>`。可见性：只能引用同一 flow 体内、拓扑在前的 step——**self-ref 例外恰一处 YAML 表面**：本步 `expect` 的 `expr` 谓词内可写 `steps.<自身 id>.output.*`（指本步投影后输出，`asserting` 阶段求值无鸡生蛋问题，02 §4.1.1）；`preflight` 与实参位置 self-ref 非法。subflow 是硬边界；handler 无输出。

**字符串插值糖**：`${{ }}` 占满整个标量 → 表达式的类型化值原样传递；嵌入字符串（`"欢迎, ${{ params.username }}!"`）→ normalize 消解为 `concat(...)`，结果恒为 string。

**YAML 锚点与别名**：允许 `&anchor` / `*alias` 做**值复用**（在 `parse` 阶段由 saphyr 消解（yaml-rust2 系，带 span marks；R12——serde_yaml 已停维护不可用），IR 无痕迹；sourceMap 记录别名的定义点以便错误回指）；展开后节点总数计入 parse 上限（billion-laughs 防护）。**禁止 merge key `<<`**（YAML 1.1 扩展，超出「YAML 值限 JSON 数据模型」约束（00 §8 阶段 1），fail-closed 拒绝）。锚点复用值，macro 复用步骤序列，subflow 复用带契约的流程——三层复用机制，各管一层，不得越级。

**环境变量**：`env.*` 是 **runner 装配层注入的封闭只读环境**，v0.1 清单：`env.deviceId`、`env.platform`、`env.runId`（骨架仅示例 `env.deviceId`，清单**待骨架收编**，见 §5-8）。钉死：Pointlock 表达式**永不读取进程环境变量**（`process.env` 无表面）——OS 环境值要进 flow，必须在调用侧显式落到参数：`pointlock run --param username="$APP_USER"`（shell 展开发生在 Pointlock 之外）或 `--params-file params.json`。理由：隐式环境读取会把秘密悄悄写进 `resolvedInputs` 快照与 Evidence，与 v0.2 `secrets.*` 的不透明句柄设计正面冲突。

**v0.2 预留（不得挪用）**：`secrets`、`protected` 两个关键字已锁定（骨架 R6）：`secrets.*` 是不透明句柄作用域，只可整体进 protected action 实参位，禁止参与运算或出现在 Evidence；`protected: true` 显式声明 protected action。**v0.1 编译器在 `bind` 阶段对 `protection: protected` 的 action fail-closed 拒绝**；YAML 中出现 `secrets`/`protected` 键 → 编译错误「v0.2 预留」。

### 1.10 语法糖 vs 一一对应：权威对照表

**一一对应（1:1，编译只做校验与重命名）**

| YAML | IR | 备注 |
|---|---|---|
| `flow` / `provider` / `verdict_policy` | `flowId` / `provider.name` / `verdictPolicy` | |
| `params.<n>.{schema,required,default}` | `ParamDecl` | |
| `outputs.<n>.{schema,from}` | `OutputDecl` | |
| `id` `timeout_ms` `effect` `idempotent` `checkpoint` | `stepId` `timeoutMs` `effect` `idempotent` `checkpoint` | |
| `preflight` / `expect` 断言项 | `preflight` / `assertions`（`AssertionIR`） | 谓词四型 1:1 |
| `verify_via` | `verifyVia` | |
| `retry.{max_attempts,backoff_ms,retry_on}` | `RetryPolicy{maxAttempts,backoffMs,retryOn}` | |
| `call` + `inputs` | `CallStepIR{flowRef,inputs}` | 路径→`{flowId,irHash}` 在 normalize 锁定 |
| `human.{mode,prompt,presents,decisions,on_timeout}` | `HumanStepIR` | `on_timeout` 只有一个合法值 |
| `if`/`then`/`else`、`foreach.{in,as,steps}`、`let` | `IfStepIR`/`ForeachStepIR`/`LetStepIR` | |
| handler 键（§1.8） | `HandlerBinding`/`HandlerAction` | |

**语法糖（normalize/bind 消解，IR 中无同形对应物）**

| 糖 | 消解为 | 阶段 |
|---|---|---|
| 动词 head key（`tap` 等 8 个） | `ActionBinding.attempts[].actionName`（原生名）+ `args`（经 `argMap`）；`verb` 仅存报告元数据 | bind |
| `locate_via` + `coordinate` | `attempts: BoundAttempt[]` + `acceptExecutionModes` 推导 | bind |
| macro 定义与调用、`${{ macro.* }}` | 卫生展开的 step 序列 + sourceMap origin trace | normalize |
| 字符串插值 | `concat(...)` 表达式 | normalize |
| YAML 锚点/别名 | 值复制 | parse |
| `value:` 断言键 | `elementText` 谓词（exact） | normalize |
| `screenshot: {}` / `observe: [...]` 列表形 | observe 类 `ActionStepIR`（wants 展开） | normalize |
| 无 head key + `expect` | `AssertStepIR{observe:"fresh"}` | normalize |
| 缺省填充（`verdict_policy`、`checkpoint`、缺省单通道链、`effect` 推导） | 显式 IR 字段 | normalize/bind |

**只存在于 IR、没有 YAML 表面的字段**（编译器独占产出，作者与 LLM 都无法伪造）：`irVersion` `irHash` `lockfileDigest` `requiredFeatures` `effectHash` `judgeHash` `sourceMap` `subflows` `actionName` `acceptExecutionModes` `protection` `requiresFeature` `assertId`（缺省派生时）`onMissingInput`。这张负面清单是 §4.6「runner 拒绝未编译产物」论证的一半。

---

## 2. 示例一：Android 登录（devicerail provider）

三个文件。展示：subflow、macro、handler（权限弹窗）、fallback（act-chain + verify-chain）、断言、human 节点（图形验证码）、protected input 的 v0.1 现实与 v0.2 预留。selector 中的 `identifier` 值与 `launchApp` 等 driver 专有 action 名为示例值，**实际以 `pointlock lock` 产出的 lockfile 为准**（协议保证的只有五件套）。

### 2.1 主 flow：`flows/shop-login.flow.yaml`

```yaml
flow: shop_login
provider: devicerail
verdict_policy: standard

params:
  username: { schema: { type: string }, required: true }
  # v0.1 现实：密码是明文参数，会进入 resolvedInputs 快照与 Evidence。
  # 这正是 v0.2 secrets.* 不透明句柄 + protected: true 要解决的问题（§1.9）；
  # v0.1 若 lockfile 中对应 action 声明 protection: protected，编译器直接拒绝（骨架 R6）。
  password: { schema: { type: string }, required: true }

outputs:
  logged_in_as:
    schema: { type: string }
    from: ${{ params.username }}

handlers:                        # flow 级：会话降级 → 升级人机
  on_error:
    - error_classes: [session_degraded]
      escalate:
        mode: repairWorld
        prompt: "DeviceRail 会话降级。请检查设备连接后裁决。"
        decisions: [adopt, redo, abort]
        timeout_ms: 600000
      max_triggers: 1

steps:
  - id: launch_app
    invoke:
      action: launchApp                    # driver 专有 action：一律走 invoke 逃逸门（骨架 A.6-3）
      args: { packageName: "com.example.shop" }
    effect: mutating
    idempotent: true                       # 重复拉起同一 app 无额外副作用 → 授权超时自动重试
    timeout_ms: 15000
    on_fail:                               # 权限弹窗 handler：修复 subflow 跑完后本步重入
      repair: ./dismiss-permission-dialog.flow.yaml
      max_triggers: 2
    expect:
      - element: { identifier: "com.example.shop:id/login_form" }
        state: visible

  - id: do_login
    call: ./login.flow.yaml                # subflow：一等公民，按 irHash 锁定
    inputs:
      username: ${{ params.username }}
      password: ${{ params.password }}
    on_fail:                               # 登录整体失败也可能是权限弹窗压场：先修复再重入
      repair: ./dismiss-permission-dialog.flow.yaml
      max_triggers: 1

  - id: verify_logged_in                   # 独立断言步（AssertStepIR，observe: fresh）
    preflight:                             # resume 探针：中断后恢复时强制重放（骨架 §6.7-C）
      - element: { identifier: "com.example.shop:id/home_tab" }
        state: present
    expect:
      - element: { identifier: "com.example.shop:id/avatar" }
        state: visible
        verify_via: [uiTree, vision]       # 显式降级验证；vision 在链尾 + 显式提示词
        visual: "首页右上角显示用户头像，处于已登录状态"
      - element: { identifier: "com.example.shop:id/welcome_text" }
        text: { value: "欢迎回来", mode: contains }   # 谓词静态性（§1.5）：断言静态欢迎文案；
                                                    # 「文案含当前用户名」无结构化谓词表面，
                                                    # 需要时交本步 on_unknown 的人工 judge
    on_unknown:                            # 结构化与视觉都无法确认 → 交人裁决，绝不猜
      escalate:
        mode: judge
        prompt: "无法自动确认登录态，请人工判定。"
        decisions: [pass, fail]
        timeout_ms: 300000
      max_triggers: 1
```

### 2.2 登录 subflow：`flows/login.flow.yaml`

```yaml
flow: login
provider: devicerail
verdict_policy: standard

params:
  username: { schema: { type: string }, required: true }
  password: { schema: { type: string }, required: true }

outputs:
  home_ready:
    schema: { type: boolean }
    from: ${{ eq(steps.wait_home.output.matched, true) }}

macros:
  fill_field:                              # macro：清空再输入；编译期蒸发
    params: [element, value]
    steps:
      - id: clear
        clear: { element: "${{ macro.element }}" }
      - id: type
        set_value: { element: "${{ macro.element }}", value: "${{ macro.value }}" }
        # 无就地文本断言（§1.5 谓词静态性：value 实参是运行期表达式）；
        # 两步 unverified，输入是否生效由 wait_home 与 caller 的 verify_logged_in 终局验证

steps:
  - id: enter_username                     # 展开为 enter_username:clear / enter_username:type
    preflight:
      - element: { identifier: "com.example.shop:id/login_form" }
        state: visible
    fill_field:
      element: { identifier: "com.example.shop:id/username_input" }
      value: ${{ params.username }}

  - id: enter_password
    fill_field:
      element: { identifier: "com.example.shop:id/password_input" }
      value: ${{ params.password }}
    # v0.2 预告（当前编译器拒绝，此处仅注释）：
    #   set_value: { element: ..., value: "${{ secrets.password }}" }
    #   protected: true
    # 届时走 DeviceRail feature action.protected.v1；观测按 protectedAction 合法缺料
    #（uiSnapshotOmission/screenshotOmission），断言相应输出 unknown 而非泄密。

  - id: probe_captcha                      # readonly 探测：waitForElementResult.matched 驱动分支
    wait_for:
      element: { identifier: "com.example.shop:id/captcha_image" }
      state: present
    timeout_ms: 3000

  - id: handle_captcha
    if: ${{ eq(steps.probe_captcha.output.matched, true) }}
    then:
      - id: captcha_shot
        screenshot: {}
      - id: solve_captcha                  # 人机协作是正式节点（原则 8）
        human:
          mode: provideInput
          prompt: "请识别图中验证码文本"
          presents: [ "${{ steps.captcha_shot.output.screenshot }}" ]
          expect_schema:
            type: object
            properties: { text: { type: string } }
            required: [text]
        timeout_ms: 300000                 # 超时 → verdict unknown（固定，不可配置）
      - id: enter_captcha
        set_value:
          element: { identifier: "com.example.shop:id/captcha_input" }
          value: ${{ steps.solve_captcha.output.text }}

  - id: tap_login
    tap:
      element: { role: button, name: "登录" }
    locate_via: [uiTree, coordinate]       # act-chain 显式降级；uiTree 语义定位失败(final)才用坐标
    coordinate: { x: 540, y: 1650 }        # coordinate 入链 ⟹ 静态坐标必填
    retry:
      max_attempts: 3
      backoff_ms: { initial: 500, factor: 2, max: 4000 }
      retry_on: [action_failed_retryable, target_stale]
    expect:
      - element: { identifier: "com.example.shop:id/login_form" }
        state: absent

  - id: wait_home
    wait_for:
      element: { identifier: "com.example.shop:id/home_tab" }
      state: visible
    timeout_ms: 10000
    expect:
      - expr: ${{ eq(steps.wait_home.output.matched, true) }}
```

### 2.3 修复 subflow：`flows/dismiss-permission-dialog.flow.yaml`

```yaml
flow: dismiss_permission_dialog            # repair subflow：无数据输出，只把世界修回锚点
provider: devicerail
verdict_policy: standard
params: {}
outputs: {}

steps:
  - id: probe_dialog
    wait_for:
      element: { role: button, name: "允许" }   # Android 系统权限弹窗
      state: visible
    timeout_ms: 2000

  - id: allow_if_present
    if: ${{ eq(steps.probe_dialog.output.matched, true) }}
    then:
      - id: tap_allow
        tap: { element: { role: button, name: "允许" } }
        expect:
          - element: { role: button, name: "允许" }
            state: absent
```

### 2.4 讲解要点

| 要点 | 位置 | 语义 |
|---|---|---|
| subflow | `do_login` → `login.flow.yaml` | 独立编译、`irHash` 锁定、call-by-value、call step verdict = callee flow verdict |
| macro | `fill_field` | 编译期蒸发；hygiene 前缀 `enter_username:clear`（`:` 分段 = 编译器合成段，无 ref 表面）；体内无跨步引用；动态输入不做就地文本断言（§1.5 谓词静态性），终局验证在流程级断言 |
| handler | flow 级 `on_error`（session_degraded → escalate）；step 级 `on_fail`（repair 权限弹窗） | repair 跑完后宿主 step 重入；`max_triggers` 防循环；handler 无数据输出 |
| act-chain fallback | `tap_login.locate_via` | `[uiTree, coordinate]` + 静态坐标；白名单逐 attempt 只看自身通道（§1.4）——语义 attempt 被 daemon 内部降级（`coordinateFallback`）一律 R-degrade：强制全量验证，否则 unknown，声明了坐标 attempt 也不豁免 |
| verify-chain fallback | `verify_logged_in` | `[uiTree, vision]` + 显式 `visual` 提示；vision pass 记 `degraded`（strict 策略折叠为 unknown） |
| human 节点 | `solve_captcha`（provideInput）、两处 escalate（judge/repairWorld） | durable：`humanRequested` 落账本，进程可退出；超时固定 unknown |
| protected input | `enter_password` 注释块 | v0.1 明文参数 + bind 阶段拒绝 protected action；v0.2 `secrets.*`/`protected: true` 预留 |
| preflight vs expect | `enter_username.preflight` / 各步 `expect` | 前置探针（resume 重放） vs 后置断言（进 verdict），永不混用 |

## 3. 示例二：Web 表单（playwright provider，说明性）

**前提声明**：v0.1 唯一随附 provider 是 `devicerail`（骨架 §2 概念 6），`FlowIR.provider.name` 的类型在 v0.1 钉为 `"devicerail"`。本例演示 provider 抽象的可移植性——playwright provider 是 v0.2 候选；其包名归属（Rust crate 还是 stdio JSON-RPC sidecar 包——Rust 核心下直连 Playwright 需 Node sidecar，00 §4 SPI 进程边界注记）列为 v0.2 收编议题，本轮不定名（R12），下文仅以 `provider: playwright` 指代。其 manifest（通道：`dom` role=both、`uiTree` role=both（accessibility tree）、`vision` role=verify；动词绑定到 Playwright 原生操作）为说明性构造。放宽 `provider.name` 为 string 的时机见 §5-10。YAML 层不受影响：作者只写通用动词，provider 差异被 manifest 吸收。

```yaml
flow: web_shipping_form
provider: playwright                       # 说明性：v0.1 编译器只接受 devicerail
verdict_policy: strict                     # 演示：degraded pass 折叠为 unknown

params:
  base_url: { schema: { type: string }, required: true }
  street:   { schema: { type: string }, required: true }
  city:     { schema: { type: string }, required: true }
  receiver: { schema: { type: string }, required: true }

outputs:
  submitted:
    schema: { type: boolean }
    from: ${{ eq(steps.verify_submitted.verdict, "pass") }}

macros:
  clear_field:                             # 「清空地址栏」macro；编译期蒸发
    params: [element]
    steps:
      - id: wipe
        clear: { element: "${{ macro.element }}" }
        # 「字段已空」不可断言：TextMatchIR.value 是 minLength 1 的静态串（§1.5 谓词静态性推论二），
        # elementState 四值也不含 empty。本步 unverified；后续 fill_address 覆盖写入即语义兜底

steps:
  - id: goto_form                          # 导航不在 8 动词内 → invoke 逃逸门
    invoke:
      action: goto                         # playwright manifest 声明的原生 action（说明性）
      args: { url: '${{ concat(params.base_url, "/checkout/address") }}' }
    effect: mutating
    idempotent: true
    timeout_ms: 20000
    expect:
      - element: { css: "form#shipping" }
        state: visible
        verify_via: [dom]

  - id: compose_address
    let:
      full_address: ${{ concat(params.street, ", ", params.city) }}

  - id: clear_address                      # macro 调用 → clear_address:wipe
    clear_field:
      element: &addr_field                 # YAML 锚点：值复用（parse 期消解，IR 无痕）
        css: "input#address"
        role: textbox
        name: "收货地址"

  - id: fill_address
    set_value:
      element: *addr_field                 # 别名引用同一 selector
      value: ${{ vars.full_address }}      # 动作实参是 Expr，运行期值合法（对比：谓词不行）
    locate_via: [dom, uiTree]              # act-chain：CSS 优先，语义树兜底（无坐标降级）
    # 无就地文本断言（§1.5 谓词静态性）；提交结果由 verify_submitted 终局验证

  - id: fill_receiver
    set_value:
      element: { css: "input#receiver", role: textbox, name: "收货人" }
      value: ${{ params.receiver }}
    locate_via: [dom, uiTree]

  - id: submit
    tap:
      element: { css: "button[type='submit']", role: button, name: "提交订单" }
    locate_via: [dom, uiTree]
    retry:
      max_attempts: 2
      backoff_ms: 1000
      retry_on: [action_failed_retryable, target_stale]

  - id: verify_submitted                   # 显式三级降级验证链：dom → uiTree → vision
    expect:
      - element: { css: ".banner-success", role: status, name: "订单确认" }
        text: { value: "提交成功", mode: contains }
        verify_via: [dom, uiTree, vision]
        visual: "表单上方出现绿色的『提交成功』确认横幅"
    on_unknown:
      escalate:
        mode: judge
        prompt: "三条验证通道均无法确认提交结果，请人工判定。"
        presents: []
        decisions: [pass, fail]
        timeout_ms: 300000
      max_triggers: 1
```

**降级链逐通道走读**（`verify_submitted`，运行期语义 = 骨架 §6.3）：

1. **dom**：对 fresh Observation 的 DOM 求值 `.banner-success` 文本 contains「提交成功」。求值完成且成立 → pass（首选通道，无降级标记）；求值完成且不成立 → **fail 终局**，不再看 uiTree/vision（降级链救「看不到」，不救「不喜欢答案」）。
2. **uiTree**：仅当 dom 通道**无法完成求值**（如观测缺料）才轮到；用 `role: status, name: "订单确认"` 在 accessibility tree 求值。完成即终局（pass 则标 `degradedVerify`；本 flow `verdict_policy: strict` → degraded pass 折叠为 **unknown**——作者选 strict 就是在声明「二级通道的 pass 我不当真」）。
3. **vision**：仅当结构化通道全部缺料才轮到；以显式 `visual` 提示词交 `pointlock-vision` 的 VisionVerifier。它只能产出 pass(degraded)/unknown 语境下的证词，永远无法推翻结构化通道（后者若完成求值早已终局）。v0.1 缺省实现返回 unknown。
4. 链耗尽仍无法求值 → **unknown**，触发 `on_unknown` → 人工 judge。全链没有任何一处会「猜」。

## 4. 编译链路：NL → YAML draft → Typed IR → Runner

### 4.0 总览

```
  自然语言意图
      │  参与者: 人 + LLM（authoring 助手 = @pointlock/nl-drafter，信任边界外）
      │  约束物: authoring JSON Schema + CapabilityLockfile 词汇提示
      │  意图缺口 → 结构化问询(elicitation, §4.1.1) → 答案织回重起草
      ▼
  YAML draft (*.flow.yaml)          ←──── 编译诊断(JSON, 带 YAML span) 反馈循环
      │  人：评审 draft（YAML 是界面，diff 可读）
      ▼
  pointlock compile（五阶段: parse → normalize → check → bind → seal）
      │  消费: CapabilityLockfile（pointlock lock 产出，进版本库）+ ProviderManifest
      │  产出: FlowIR (*.ir.json, 含 irHash/effectHash/judgeHash/sourceMap/lockfileDigest)
      │        + binding report（人读：每步绑定到什么、哪些降级被授权）
      │  人：评审 binding report 后放行首跑
      ▼
  pointlock run <flow>.ir.json       runner 只吃 FlowIR；载入即验 irHash
      │  openSession → attestation 复核 lockfileDigest（capability_drift 则拒跑）
      ▼
  执行（RunLog / checkpoint / resume —— 04 号文档管辖）
```

LLM 出现且仅出现在**阶段 0**。编译器内部零 LLM、零代码执行（verb 映射是声明式 `argMap`，骨架 R7）、全确定性：同输入（YAML + lockfile + manifest）必得同 `irHash`。

### 4.1 阶段 0：NL → YAML draft（LLM 的位置与约束）

| 项 | 内容 |
|---|---|
| 输入 | 自然语言意图；authoring JSON Schema（`pointlock compile --emit-authoring-schema` 从封闭关键字表生成）；lockfile 词汇提示（可用 action 名 + inputSchema、可用 feature、平台、各通道可用性——即 LLM 被告知「这台设备究竟会什么」）；可选的 uiSnapshot 素材（任何 DeviceRail 客户端经 `device.observe` + `ui.snapshot.get` 捕获）用于落 selector |
| 输出 | `*.flow.yaml` 草稿；意图存在缺口时先产结构化问询（elicitation，§4.1.1），不擅自补全 |
| 约束机制 | ① **schema 约束**：draft 必须过 authoring JSON Schema（封闭关键字、封闭枚举值、结构形状），LLM 无法发明关键字；② **capability 约束**：lockfile 提示把词汇收窄到实际能力，且即便 LLM 越界，`bind` 阶段照样拒绝——提示是优化，编译器才是执法；③ **反馈循环**：`pointlock compile --format json` 的结构化诊断（§4.4，带 YAML 行列与修复提示）直接回喂 LLM 迭代，直到零 error；④ **表面隔离**：LLM 只产 YAML，永远接触不到 IR——§1.10 负面清单里的字段（irHash、actionName、acceptExecutionModes…）没有 YAML 表面，想伪造也没有语法位置 |
| 人的确认点 | 见 §4.5 |

编译器对 draft 来源完全不知情也不关心：人手写与 LLM 生成走同一道门。这是「LLM 可用但不可信」的结构化表达。

### 4.1.1 编译期问询（elicitation，R13；里程碑 M2）

NL 起草器实体为 `@pointlock/nl-drafter`（TS package，信任边界外，00 §1.2/A.1；选 TS 以用 LLM 生态）。它在阶段 0 除产 YAML 草稿外，遇下列**四类情形必须发结构化提问**，不得擅自补全（00 §6.9 收编 2）：

1. **必填 param 缺失**：意图未给出某个 `required: true` 参数的取值来源；
2. **目标选择器歧义**：意图描述在 uiSnapshot 素材中匹配多个候选元素，selector 无法唯一落定；
3. **fallback 链授权**：`coordinate` 进 `locate_via` 或 `vision` 进 `verify_via` 需作者显式点头——降级授权是作者决策（原则 6/7，§1.4），起草器不得代签；
4. **secret 处理策略**：意图涉及密码/令牌类输入时，问明 v0.1 明文参数现实与 v0.2 `secrets.*` 预留（§1.9）之间的取舍。

问题为 JSON 结构：`question`（陈述缺口）/ `options`（候选答案）/ 目标 YAML path（指向草稿中待定位置）。答案织回后**重起草**，循环至 `pointlock compile` 通过——与 §4.1 约束机制 ③ 的诊断反馈循环同构且互补：诊断循环消编译错误，elicitation 循环消意图缺口。

边界重申（既有原则，不变）：LLM 永远只产 YAML 草稿，编译器是唯一执法者。elicitation 提高的是草稿一次通过率与作者意图保真度，不新增任何信任面。

### 4.2 阶段 1–5：逐阶段输入/输出契约

（职责总表见骨架 §8；本节补齐输入输出与本文引入的语法的归属。）

| # | 阶段 | 输入 | 输出 | 本文语法的归属与拒绝条件 |
|---|---|---|---|---|
| 1 | `parse` | YAML 字节流 | AST + 逐节点源 span | saphyr（yaml-rust2 系，带 span marks，YAML 行号诊断能力不降；serde_yaml 已停维护不可用；YAML 值仍限 JSON 数据模型，R12）；锚点/别名消解（别名定义点入 span 记录）；`<<` merge key 拒绝；byte/depth/展开节点数上限；重复键拒绝 |
| 2 | `normalize` | AST（多文件：主 flow + 依赖闭包） | 规范化的候选 IR 树（未绑定）+ origin trace | head-key 分派（未知键/多 head key 拒绝）；macro 卫生展开（`:` 前缀合成 id；递归/超深拒绝；体内 `steps.*` 引用指向体内 id → 拒绝）；字符串插值 → `concat`；`value:` 糖 → `elementText`；谓词静态性（`${{ }}` 出现在非 `expr` 谓词字段 → 拒绝，§1.5）；缺省填充；subflow 独立编译并按 `irHash` 锁定（版本冲突拒绝）；展开后 step id 唯一性与格式校验；`secrets`/`protected` 出现 → 拒绝（v0.2 预留） |
| 3 | `check` | 候选 IR 树 | 类型化 IR 树 + 数据依赖图 | 引用消解（未定义 step/output/var/iter → 拒绝；前向引用 → 拒绝；跨 subflow 边界引用 → 拒绝）；表达式类型检查（PureFn 元数与实参类型；`if.cond`/`expr` 谓词必须 boolean；invoke 的 unknown output 未经 `expect_schema` 收窄即取字段 → 拒绝）；`vars` SSA 单赋值；依赖图成环拒绝；handler 环检测；`orphaned` 前置检查（被下游引用的 step 被删 → 悬空引用错） |
| 4 | `bind` | 类型化 IR 树 + `ProviderManifest` + `CapabilityLockfile` | 完全绑定的 IR 树 | **宪法执行处**：动词 → `actionName`（声明式 `argMap`）；实参对 `inputSchema` 静态形状校验；act/verify 双链逐项能力与素材校验（§1.4 六条规则）；protected action 拒绝；feature 归集 → `requiredFeatures`；`acceptExecutionModes` 推导；跨 `documentEpoch` 的元素引用注入 revalidate（`findElement` 重定位）step |
| 5 | `seal` | 绑定完成的 IR 树 | `FlowIR`（`*.ir.json`）+ binding report | 规范化序列化；`irHash` + 逐步 `effectHash`/`judgeHash`；`sourceMap`（IR path → YAML span + macro origin trace）；嵌入 `lockfileDigest`。**seal 无拒绝条件**——一切拒绝在前四阶段完成，seal 只对合法树盖章 |

编译不需设备在线（消费 lockfile 快照）；`pointlock lock` 单独对真实 daemon 执行 `system.hello` + `device.capabilities` 固化事实，lockfile 进版本库（骨架 §4.1）。

### 4.3 静态检查清单（工程可对表逐项实现）

按任务五大类展开，标注所属阶段与错误码段（编码规则见 §4.4）：

**A. 词汇表校验（parse/normalize，RF1xxx/RF2xxx）**
- A1 顶层键 ∈ 封闭清单（A.7）；A2 step 通用键 ∈ 封闭清单；A3 head key ∈ 动词 ∪ 结构 ∪ 本地 macro 名；A4 枚举值 ∈ 封闭集（`state`、`mode`、`retry_on` 的 ErrorClass、`verdict_policy`、`verify_via`/`locate_via` 通道名、human `mode`/`decisions` 形状、`on_timeout` 恒 `unknown`）；A5 macro 名不得遮蔽封闭词汇；A6 `secrets`/`protected` 预留拒绝；A7 `<<` 拒绝。

**B. id 唯一性与稳定性（normalize，RF2xxx）**
- B1 展开后 stepId flow 内唯一；B2 id 格式 `[a-z][a-z0-9_]*`，`:`/`.` 不得出现（`:` 分段保留给编译器合成 id）；B3 `assert_id` 唯一（缺省派生保证）；B4 macro 展开前缀不产生碰撞（hygiene 保证，碰撞即内部错误）；B5 macro 体内 `steps.*` 引用不得指向体内 step id（合成 id 无 ref 表面，§1.7 规则 2）。

**C. 表达式与数据流（check，RF3xxx）**
- C1 作用域封闭：引用前缀 ∈ {params, env, steps, vars, iter}；C2 拓扑序：只引用同 flow 体内在前的 step（self-ref 特例见 02 §4.1.1：本步 `outputs` 与本步 `assertions` 的 `expr` 谓词内 `steps.<自身 id>.output.*` 合法——分别指原始输出与投影后输出；`preflight` 与实参位置 self-ref 非法）；C3 subflow 硬边界；C4 handler 无输出（引用 handler 内部 step 的 output → 错误）；C5 PureFn 白名单与元数/类型；C6 谓词位置必须 boolean；C7 invoke unknown output 先收窄再取字段；C8 SSA：`vars` 单赋值；C9 数据依赖图无环（同时产出骨架 §6.7 effectDirty 下游失效计算所需的图）；C10 handler `maxTriggers ≥ 1` 且 repair 引用的 subflow 编译闭包无环。

**D. capability binding（bind，RF4xxx）**
- D1 `provider.name` 与 manifest 一致（v0.1 恒 devicerail）；D2 lockfile 存在且 `protocolSelected` 落在 manifest 支持窗（major=1, 1.5）；D3 每个 `actionName` ∈ `lockfile.device.actions`（无 lockfile 退 `manifest.knownActions` ∩ guaranteed feature 覆盖域）；D4 实参 ∩ `inputSchema` 静态形状（能静态判定的部分；运行期仍二次校验）；D5 feature 检查：动词/通道的 `requiresFeature` ∈ 可用集（如五件套 → `device.semanticActions.v1`，uiTree 断言 → `observation.uiSnapshot.v1`，verdict 回写 → `verdict.record.v1`）；D6 `protection: protected` → 拒绝（v0.1）；D7 `requiredFeatures` 归集完备（运行期进 `FeatureOffer.required`）。

**E. fallback 链合法性（bind，RF4xxx）**
- E1 `locate_via` 是 `[dom, uiTree, coordinate]` 的有序子序列、`verify_via` 是 `[dom, uiTree, vision]` 的有序子序列；E2 vision ∉ act-chain（原则 7 类型层封死）、coordinate ∉ verify-chain；E3 vision 仅链尾且 `visual` 提示词必在；E4 coordinate 入链 ⟺ 静态 `coordinate` 键；E5 dom 入链 ⟹ `css` 在场、uiTree 入链 ⟹ 结构化字段在场；E6 链上每通道经 manifest `channels` 声明且 role 相容、feature/platform 满足；E7 `visual` 谓词的断言不得独撑 pass（§1.5 约束）；E8 未声明链却依赖降级的形状（如 selector 只有 `css` 而缺省通道是 uiTree）→ 错误并给修复提示。

以及：F1 params/outputs 的 JSON Schema 元校验（RF2）；F2 `outputs.from` 引用消解（RF3）；F3 subflow `inputs` 对 callee `params` 契约的形状校验（RF3）；F4 跨 `documentEpoch` 元素引用 → 注入 revalidate 并发 warning（RF4，非 error）；F5 谓词静态性（RF2）：`elementState`/`elementText`/`visual` 谓词字段（selector/`state`/`text`/`value`/`visual`/`region`）不接受 `${{ }}`，且 `value`/`text.value` 非空——诊断附改写指引（改用 `expr` 谓词或编译期字面量，§1.5）；F6 默认升级姿态 lint（check，RF3，**warning 非 error**，R13）：flow 对 unknown 无任何处置时提示（§1.8）。

### 4.4 编译错误报告格式（可定位到 YAML 行）

诊断类型（`pointlock-compiler` 导出，经 `--format json` 以 JSON 面世；下述 TS 记法为规范记法，真相源为 Rust DTO——R12，00 §3 引言块；类型名**待骨架收编**，见 §5-9）：

```ts
export interface CompileDiagnostic {
  code: string;                    // "RF" + 阶段位 + 序号：RF1xxx=parse, RF2xxx=normalize,
                                   // RF3xxx=check, RF4xxx=bind（seal 不产诊断）
  stage: "parse" | "normalize" | "check" | "bind";
  severity: "error" | "warning";
  message: string;                 // 一句话，陈述违反的规则
  span: { file: string; startLine: number; startCol: number;
          endLine: number; endCol: number };          // 1-based，YAML 源位置
  irPath?: string;                 // 已成形部分的 IR 路径（RunPath 规范串风格）
  macroTrace?: { macro: string; span: CompileDiagnostic["span"] }[];
                                   // 宏展开链：错误在展开体内时逐层回指调用点（源自 origin trace）
  candidates?: string[];           // bind 阶段：最近似的可用能力（编辑距离），喂 LLM 修复循环
  hint?: string;                   // 指向本文/骨架条款的修复建议
}
```

人读格式（`--format pretty`，默认）：

```
error RF4104 (bind): act-chain 声明了通道 coordinate，但缺少静态坐标
  --> flows/login.flow.yaml:47:5
   |
47 |     locate_via: [uiTree, coordinate]
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   = 宏展开链: fill_field ← flows/login.flow.yaml:12:7 (enter_password)
   = 提示: coordinate 通道必须与 `coordinate: { x, y }` 同时出现（03 §1.4 规则 3，原则 6）

error RF4021 (bind): action "swipeElement" 不在 lockfile 能力集内
  --> flows/login.flow.yaml:63:5
   = 候选: tapElement, setElementValue, waitForElement
   = 提示: 运行 `pointlock lock` 重新固化设备能力，或改用 invoke 前先确认 driver 支持
```

机器格式（`--format json`）：`{ ok: boolean, irHash?: string, diagnostics: CompileDiagnostic[] }` ——这是阶段 0 LLM 反馈循环的输入。硬规则：**每条诊断必须有 span**（parse 失败也至少给到出错行）；错误在宏展开体内时 `macroTrace` 必须完整回溯到作者可见的调用点——作者永远被指向自己写下的那一行，而不是编译器展开出来的中间产物。`sourceMap` 在 seal 后接棒承担同一职责（运行期 `pointlock locate` 把 RunPath 译回 YAML 行，骨架 §9）。

### 4.5 人的确认点（穷尽清单）

| # | 时机 | 人确认什么 | 机制 |
|---|---|---|---|
| 1 | draft 评审 | YAML 语义 = 本人意图（YAML 是界面，为人的评审而设计——原则 1 的存在理由之一） | 代码评审 / diff；LLM 产物与手写产物同标准 |
| 2 | lockfile 变更 | 设备能力快照的变化（新 action、feature 消长） | `pointlock lock` 产物进版本库，走 VCS 评审 |
| 3 | binding report 评审 | 每步绑到哪个原生 action、哪些降级被授权（act/verify 链、acceptExecutionModes）、requiredFeatures 清单 | `pointlock compile` 随 IR 产出人读报告；团队约定首跑前必读 |
| 4 | 运行期 human step | 流程内声明的正式人机节点（confirm/judge/provideInput/repairWorld） | `pointlock-human-cli`；durable，超时 unknown |

1–3 在编译面（离线），4 在运行面。没有第五处：runner 不会在未声明的位置临时征求人的意见——需要人就写 `human` step 或 `escalate` handler（原则 8）。

R13 补注（清单口径不变）：阶段 0 的编译期问询（§4.1.1）发生在 draft 起草循环内，归入确认点 1（draft 评审的前置对话）；监督模式（`pointlock run --supervise <mutating|all>`，00 §6.9，M2）是**调用时显式声明的 run 级策略**——门控问答（`purpose: "supervision"`）由该声明触发，不是 runner 即兴征询，与「未声明位置不征求人」不冲突。

### 4.6 为什么 runner 拒绝执行任何未经编译的东西

这不是政策偏好，是五重结构性机制，每一重独立成立：

1. **类型面**：`pointlock-runner` 的入口签名只接受 `FlowIR`，不接受字符串或 YAML AST（骨架 §1.2 硬规则）；runner crate 依赖清单里没有 YAML parser——想执行 YAML 在链接期就不可能，而非运行期被拦。
2. **完整性面**：载入 `*.ir.json` 时重算规范化哈希并与 `irHash` 比对，不符即拒。手改 IR、截断、拼接一律现形；`irVersion` 不符同样拒（无静默迁移）。
3. **能力面**：`FlowIR.lockfileDigest` + `requiredFeatures` 在 `openSession` 时经 attestation 复核（`system.hello` 的 `FeatureOffer.required` 语义保证 required 不满足则握手失败）；未经 `bind` 的东西没有合法的 lockfileDigest，`capability_drift` 拒跑。绕过编译 = 绕过原则 5 的全部检查（能力缺失、未声明降级、protected 拒绝），runner 没有能力也没有义务在运行期补做这些编译期检查。
4. **执行语义面**：resume/修复对齐（骨架 §6.7）依赖逐步 `effectHash`/`judgeHash`，verdict 折叠依赖编译期钉死的 `verifyVia` 与 `acceptExecutionModes`，WAL/reconcile 依赖确定性的 `resolvedInputs` 求值——这些字段只有 `seal` 能正确产出。喂给 runner 一份没有双哈希的「YAML 直译物」，等于让 checkpoint、resume、离线重判全体失效。
5. **信任面**：阶段 0 允许 LLM 起草，整个体系的安全论证就建立在「起草者不可信，编译器是唯一执法者」上。若 runner 留有任何执行未编译输入的旁门，LLM（或注入到 NL 意图里的攻击者）的产物就能绕过封闭词汇、能力绑定与降级授权直达设备。旁门必须不存在，而不是默认关闭。

一句话：**YAML 是界面，IR 是契约，runner 只认契约**。编译器是从「人机共写的意图」到「可审计的执行物」之间唯一的、确定性的、零执行的翻译官。

---

## 5. 待骨架收编词汇清单（提请骨架评审）

本文按骨架命名风格引入、但 A.7/A.3 尚未收录的词汇（正文出现处均已标注）：

1. handler YAML 子键：`error_classes`、`max_triggers`；处置 head key `retry` `continue` `escalate` `abort` `repair`（= `Disposition` 枚举值转 YAML 键）。**已收编**：骨架 A.7「handler 子键」行。
2. macro 参数的编译期引用作用域 `${{ macro.<param> }}`（normalize 消解，不进 IR 运行期作用域）。
3. macro 调用形式 = 用户定义 head key（机制收编：head-key 分派顺序 动词 → 结构 → macro）。
4. `invoke` 子键 `action`（A.6 已示例，建议正式入 A.7）与实参容器 `args`（原提案复用 `inputs`，M2 收编改定为 `args`，见 §1.3）。**已收编**：骨架 A.7「动词键」行已补 invoke 子键 `action`、`args`。
5. `expect_schema` 复用为 `human`（provideInput）的输出契约表面（→ `HumanStepIR.outputSchema`）。
6. `observe` 动词的列表值形式（值 `screenshot` / `uiSnapshot`）。
7. `visual` 键复用为非 visual 谓词断言的 vision 链尾提示词（§1.4 规则 5）。
8. `env.*` 封闭键清单：`env.deviceId` `env.platform` `env.runId`（骨架仅示例 deviceId）。
9. 类型名 `CompileDiagnostic`（`pointlock-compiler` 导出；TS 记法为规范记法，R12）。
10. `FlowIR.provider.name` 由字面量 `"devicerail"` 放宽为 string 的时机（v0.2，随第二个 provider 落地；§3 示例的前提）。
11. `observe`/`screenshot` 动词在 devicerail 的绑定目标是 RPC `device.observe` 而非 `device.execute` action——`VerbBinding` 需允许声明 RPC 目标（或 provider-kit 为 observe 类动词开特化绑定形态）。

## 6. 已确认关闭项

- **`wait_for` 超时未命中语义（原开放条目：需对照 DeviceRail 实际行为确认；2026-07-17 已实证关闭）**：DeviceRail 源码 `crates/ios-webdriver/src/appium_driver.rs` 的 `wait_for_element` 在等待窗口耗尽时返回 `Ok(WaitOutcome::NotMatched)`——**成功终态而非错误**；协议 schema `protocol/schema/v1/wait-for-element-result.schema.json` 的 `required` 仅 `[matched, condition]`，`element` 可选。故 `waitForElement` 未命中 = outcome `succeeded` + `matched: false`（观测数据而非失败）。本文 §2.2 `probe_captcha`、§2.3 `probe_dialog` 以 `steps.<id>.output.matched` 驱动分支的写法即以此为据；定性与 04 篇 §6.2 一致。

---

*本文与骨架 Canonical Vocabulary 的任何冲突以骨架为准；发现冲突请提架构评审而非就地改写。*
