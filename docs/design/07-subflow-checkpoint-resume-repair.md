# Pointlock 执行语义：Subflow、失败定位、Checkpoint、Resume 与局部修复

> 本文是 Pointlock 设计文档系列第 7 篇，骨架见 [00-architecture-spine.md](./00-architecture-spine.md)。骨架是唯一上位依据；本文与骨架冲突时以骨架为准。
>
> 覆盖需求产出 10（执行语义核心）：(a) Subflow 调用机制；(b) 失败定位与判案卷宗；(c) Checkpoint 模型与存储格式；(d) Resume 语义与副作用安全；(e) 局部修复与对齐；(f) 与 LangGraph / Prefect 的对比。全部 DeviceRail wire 层名称与骨架附录 A.8 的已核实清单逐字一致。

---

## 0. 本篇的三条不变式（先立后论）

后文一切规则都是这三条不变式的推论，工程实现时可直接把它们写成断言：

- **I1（真相唯一）**：RunLog（append-only、单调 `seq`）是一次 Run 的唯一真相；`CheckpointView` 是它在安全点的确定性物化视图，**任何时刻可由 RunLog 重建**，二者不一致时以 RunLog 为准。
- **I2（绝不盲目重放）**：一个 `effect: mutating` 且未声明 `idempotent: true` 的 step，其任何已进入四分终态（`succeeded | failed | cancelled | timedOut`）的 attempt，**绝不会被 runner 自动重新 dispatch**。重复执行只有四个合法来源，且每一个都是显式意志：(i) 作者修改该步（`effectDirty`）后经显式确认的 resume（§5.4）；(ii) **位置失效重放**——本身未被修改（双哈希未变）、只因排在 resume 点之后而将被重新执行的步：凡其旧记录存在**生效 attempt**（`succeeded`/`timedOut` 终局）者，必须逐一进入 §5.4 的 `requiresConfirmation`、经 `--allow-mutating-reexec` 点名并受 §5.4 第 3 条的 preflight 探针守护后方可重放，**绝不因「作者没改这一步」而免检**；(iii) handler 的显式 `retry` 处置（骨架 §6.5）；(iv) 人在 `repairWorld` 节点的 `redo` 裁决（骨架 §6.7-B）。
- **I3（世界无假设）**：Pointlock 从不假设设备在暂停/崩溃/修复期间没被动过。resume 的世界状态假设必须以 `preflight` 探针（`AssertionIR[]`，纯谓词）显式表达并在续跑前求值；没有探针的 resume 在报告中标 `unprobed`，而不是假装校验过。

---

## 1. Subflow 调用机制

Subflow 是一等公民（原则 9）：被当作一个 step 调用的 Flow，有独立编译、独立 `irHash`、运行期调用帧。本节把骨架 §2 概念 3 与 `CallStepIR` 落成可开工的运行期契约。

### 1.1 调用契约：call-by-value 双向闸门

```ts
export interface CallStepIR extends StepBase {
  kind: "call";
  flowRef: { flowId: string; irHash: Hash };  // 按内容哈希锁定 callee 版本
  inputs: Record<string, Expr>;               // call-by-value
}
```

**参数传入**：call step 进入 `ready` 时，`inputs` 的每个 `Expr` 在 **caller 作用域**（`params.* / env.* / steps.<id>.output.* / vars.* / iter.<as>`）求值一次，快照进本步 `resolvedInputs`（骨架 §6.6 的 resolvedInputs 快照化规则对 call step 同样成立）。快照值随后按 callee `FlowIR.params` 的 `ParamDecl.schema` 逐项校验：

- 编译期 `check` 阶段已做静态类型核对（callee 的 params 契约随 `flowRef.irHash` 一起被锁定，编译器能拿到）；
- 运行期仍做二次 schema 校验——表达式求出的动态值可能超出静态类型能表达的约束，失败归类 `bind_arguments_invalid`，不重试，step fail（这是编译器或表达式的 bug 信号，同骨架 §5）。

**输出返回**：callee 的 `FlowIR.outputs`（`OutputDecl { name, schema, from: Expr }`）在 callee 全部 body 执行完毕后，于 **callee 作用域**内求值、按 `schema` 校验、整体快照为 call step 的 `output`。caller 此后经 `steps.<callStepId>.output.<name>` 引用。输出同样是值拷贝——callee 帧销毁后不存在任何跨帧引用。

**verdict 聚合**：call step 自身不折叠 assertion——它的 verdict 恒等于 callee 的 flow verdict（骨架 §6.3：`call step → callee 的 flow verdict`）。callee 内部 `degraded` 的 pass 折叠出的 flow verdict 若带 `degraded=true`，该标记原样出现在 call step verdict 上，并参与 caller 的 flow verdict 折叠（`verdictPolicy: strict` 时按骨架规则折叠为 unknown）。

### 1.2 作用域隔离（硬边界）

骨架 §7 的可见性规则在 subflow 边界上的完整展开：

| 名字空间 | 是否穿透 subflow 边界 | 说明 |
|---|---|---|
| `params.*` | 否 | callee 只见自己的 params（= call step `inputs` 的求值快照） |
| `steps.<id>.*` | 否 | callee 看不见 caller 的任何 step；caller 只见 `steps.<callStepId>.output.*` 与 `steps.<callStepId>.verdict` |
| `vars.*` / `iter.<as>` | 否 | 帧内私有；跨帧传值只能走 `inputs` / `outputs` |
| `env.*` | **是（只读）** | run 级环境（如 `env.deviceId`）。同一 Run 绑定恰好一台设备、一条活跃 DeviceRail Session（骨架 §2 概念 1），env 是这一事实的投影，逐帧复制没有意义 |
| handlers | 否 | callee 的 step/flow 级 handlers 管 callee 内部转移；caller 的 handlers 只见 call step 的聚合 verdict 与错误。**例外**：run 级错误（`transport_lost`）不逐帧冒泡，直接触发 run 级 `runSuspended`（骨架 §5），恢复走 §4 的 resume 通道 |

没有动态作用域、没有全局可变量、handler 无输出（骨架 R10）——这三条合起来保证：**一个帧的完整执行语义由 `(calleeIrHash, inputsSnapshot, env)` 决定**。这是 §3 checkpoint 能以「帧栈 + 快照」闭包运行状态、§5 对齐能按哈希判定复用的前提。

### 1.3 版本锁定、嵌套深度与递归

- **锁定**：`flowRef` 按 `irHash` 锁定 callee；caller 的 `irHash` 覆盖 `subflows` 引用表（骨架 `FlowIR.subflows`，引用不内联），形成联编闭包——**改 callee 必然改 caller 的 irHash**，修复对齐（§5）因此对深层修改天然可见。
- **递归结构性不可能**：`irHash` 是内容哈希，flow A 引用 flow B 的 irHash、B 再引用 A 需要哈希不动点，构造不出来。编译器无需运行期递归检查。
- **嵌套深度**：编译期 `normalize` 阶段解析全部 subflow 引用形成静态链接闭包（DAG），`check` 阶段计算最长调用链。v0.1 上限 **`maxCallDepth = 8`**（含根帧；编译常量，超限拒产 IR）。理由：调用图完全静态，深度在编译期可判定，把限制放运行期是浪费；8 层对「登录 / 导航 / 表单 / 校验」类设备流程绰绰有余，而 `CheckpointView.frames` 的体积与人读 RunPath 的可理解性都受益于硬上限。
- **`foreach × call`**：foreach body 里调 subflow 时，每次迭代是独立的帧实例，RunPath 以 `iteration` 帧区分（§2.1），checkpoint / 对齐按迭代逐一处理。

### 1.4 Subflow 局部失败与局部重试

失败处理有明确的**四层阶梯**，逐层耗尽才升级，绝不隔层跳跃：

1. **callee step 级 attempt 内重试**（`StepBase.retry: RetryPolicy`）：同一 attempt 重发，新 `callId`、新 `actionIntent`，只对 `action_failed_retryable`、`target_stale`（重试前强制重 observe）及 `idempotent: true` 步的 `action_timed_out` 生效（骨架 §6.5 第 1 挂载点）。
2. **callee 内 handler**：step 级覆盖 flow 级；`onFail/onUnknown/onError` 产出处置（`retry | continue | escalate | abort | repair`），`maxTriggers` 防循环。这一层完全发生在 callee 帧内，caller 无感知。
3. **callee flow verdict 传导**：callee 内某 step 终局 fail 且 handler 未能挽回 → callee flow verdict fold 为 fail → call step `judged{fail}` → **caller 的 `on_fail`** 在 call step 上触发。caller 的 handler 看到的是聚合结果，看不到 callee 内部哪条 assertion 挂了——要定位得走 `failedStepPath`（§2），它会精确穿透到 callee 内部。
4. **run 级**：caller 也无 handler 或处置为 `abort` → run 按骨架 §6.2 进入 aborted / blocked 路径。

**call step 上的 `{kind: "retry"}` 处置语义（钉死）**：重新调用 callee **从头开始**，形成新的 attempt 帧（RunPath `#2`），旧帧的全部 StepRecord 保留在 RunLog（审计不可少，R10 精神）。三条配套规则：

- 世界效果**不撤销**：前一次调用中已 `succeeded` 的 mutating 动作留在世界里，Pointlock 没有补偿事务（也不假装有）。守护手段是 callee 首步（及关键步）的 `preflight` 探针——重入时探针会撞见残留状态，走 callee 自己的 `onResumeDrift`/`repair` 路径，这正是骨架 §6.7-C「修复锚点 subflow（如 ensureLoggedIn）」的用武之地。
- 与崩溃恢复严格区分：**resume 落回帧内精确位置（§4.6），绝不重启 callee**；handler retry 是作者显式声明的「整体重来」策略，二者不共享语义。
- 局部重试不跨边界：callee 失败不会触发 caller 内其他 step 的 retry；caller 的 retry 预算与 callee 内部预算独立计数（骨架 §6.5）。

若「整体重来」代价不可接受，作者应改用 `{kind: "repair", flowRef}`：跑修复 subflow（无数据输出）把世界修回锚点，然后按 hook 语义重探或重入（骨架 `HandlerAction` 定义）。

### 1.5 完整示例

`login.flow.yaml`（callee，独立编译、独立测试）：

```yaml
flow: login
provider: devicerail
params:
  account: { type: string, required: true }
  password: { type: string, required: true }
outputs:
  sessionUser: ${{ steps.readWelcome.output.text }}
steps:
  - id: focusAccount
    tap:
      element: { role: "textField", identifier: "account" }
    effect: mutating
    idempotent: true
  - id: enterAccount
    set_value:
      element: { role: "textField", identifier: "account" }
      value: ${{ params.account }}
    effect: mutating
    idempotent: true            # set 语义天然幂等：重放收敛到同一世界
    expect:
      - element: { identifier: "account" }
        value: ${{ params.account }}
  - id: enterPassword
    set_value:
      element: { role: "textField", identifier: "password" }
      value: ${{ params.password }}
    effect: mutating
    idempotent: true
  - id: submitLogin
    tap:
      element: { role: "button", name: "登录" }
    effect: mutating            # 未声明 idempotent：二次 tap 可能二次提交
    retry: { max_attempts: 2, backoff_ms: 800, retry_on: [target_stale] }
    expect:
      - element: { identifier: "welcomeBanner" }
        state: visible
  - id: readWelcome
    find:
      element: { identifier: "welcomeBanner" }
    effect: readonly
```

`checkout.flow.yaml`（caller）片段：

```yaml
steps:
  - id: purchase
    call: login                      # normalize 阶段解析为 flowRef{flowId:"login", irHash:"sha256:9c2e…"}
    inputs:
      account: ${{ params.user }}
      password: ${{ params.pass }}
    preflight:
      - element: { identifier: "loginPage" }
        state: visible               # 兼作 resume 漂移探针
    on_fail:
      retry: { max_attempts: 1, backoff_ms: 0, retry_on: [] }   # 整体重调一次
  - id: eachItem
    foreach:
      in: ${{ steps.loadCart.output.items }}
      as: item
      steps:
        - id: addToCart
          tap:
            element: { role: "button", name: '${{ concat("加入-", iter.item) }}' }
          effect: mutating
          expect:
            - element: { text: { value: "${{ iter.item }}", mode: "contains" } }
              state: present
```

编译后：`checkout` 的 IR 中 `purchase` 是 `CallStepIR`，`subflows` 表登记 `login@sha256:9c2e…`；动词 `tap/set_value/find` 消失，`BoundAttempt.actionName` 为原生 `tapElement / setElementValue / findElement`（骨架 A.6）。运行期若 `submitLogin` 的 `expect` 失败且 callee 内无 handler：`login` flow verdict = fail → `purchase` judged fail → caller `on_fail` 重调一次 `login`（attempt 帧 `#2`），重调时 `focusAccount` 起的幂等步安全重放，`submitLogin` 的 preflight 世界由 `expect`/探针把关。

> YAML 表面语法的最终权威在 YAML 层设计文档；本例只为固定运行期语义，关键字全部取自骨架 A.7 封闭清单。

---

## 2. 失败定位：failedStepPath 与判案卷宗

### 2.1 RunPath：结构化为准，规范串供人读

骨架 §9 已锁定结构：

```ts
export type RunPath = PathFrame[];
export type PathFrame =
  | { kind: "flow";      flowId: string; irHash: Hash }
  | { kind: "step";      stepId: StepId }
  | { kind: "call";      stepId?: StepId; calleeFlowId: string; calleeIrHash: Hash }
    // stepId 缺席当且仅当该 call 帧是 handler repair 启动的修复流（hook 帧直下，无宿主 call step）
  | { kind: "iteration"; index: number; key?: string }
  | { kind: "hook";      hook: string; trigger: number }
  | { kind: "attempt";   n: number }
  | { kind: "phase";     phase: "preflight" | "act" | "observe" | "assert" }
  | { kind: "assertion"; assertId: string };
```

**结构化数组是唯一权威表示**——RunLog、`CheckpointView`、`StepRecord.runPath`、`pointlock locate` 的入参与出参全部使用 JSON 形式的 `PathFrame[]`。规范串只是它的确定性人读渲染，本篇在骨架示例（`checkout@a1f3/purchase/call→login@9c2e/enterPassword#2!tokenVisible`）基础上补全全部帧类的渲染规则：

| PathFrame | 渲染 | 说明 |
|---|---|---|
| `flow` | `<flowId>@<irHash 前 8 hex>` | 路径首段；骨架硬规则 (1)：flow 一律带 irHash |
| `step` | `/<stepId>` | |
| `call` | `/<stepId>/call→<calleeFlowId>@<hash8>` | 一个帧渲染成两个视觉段：先 call step 的 id，再箭头段 |
| `iteration` | `[<index>]` 或 `[<index>:<key>]` | **无 `/`，直接缀在前面的 foreach step 段上**；v0.1 按 index 定位，`key` 预留给 keyed foreach |
| `hook` | `/hook:<hookName>:<trigger>` | handler 审计帧（骨架 R10）；其下再挂 handler 动作产生的段 |
| `attempt` | `#<n>` | 缀在 step 段上，n 从 1 起 |
| `phase` | `:<preflight\|act\|observe\|assert>` | 缀在 attempt 之后 |
| `assertion` | `!<assertId>` | 路径末段 |

示例集（同一 `checkout` run）：

```
checkout@a1f3c9d2/loadCart#1:act
    根 flow 下 loadCart 步第 1 次 attempt 的 act 阶段

checkout@a1f3c9d2/eachItem[2]/addToCart#3!itemInCart
    foreach 第 3 项（index=2）里 addToCart 第 3 次 attempt 的 itemInCart 断言

checkout@a1f3c9d2/purchase/call→login@9c2e77b0/enterPassword#2!tokenVisible
    subflow 内部失败：穿透 call 边界直达 callee 的断言

checkout@a1f3c9d2/purchase#2/call→login@9c2e77b0/focusAccount#1:preflight
    caller on_fail 重调 callee 后（call step attempt #2）首步探针

checkout@a1f3c9d2/pay/hook:onFail:1/call→repairCart@55d0ab12/clearStale#1:act
    pay 步 onFail handler 第 1 次触发，repair subflow 内部的动作
```

三条骨架硬规则原样生效：flow 段必带 irHash；宏展开不出现在路径里（sourceMap 负责译回 YAML 行号与宏调用链）；`pointlock locate <path>` 交付「可判案卷」而非坐标。

**`failedStepPath` 的定义**：run 终局为 fail / unknown / aborted 时，报告中的 `failedStepPath` = 造成该终局的**最深判定点**的 RunPath——通常精确到 `assertion` 帧（哪条断言否决）或 `phase` 帧（act 阶段哪个 attempt 以何种 ErrorClass 终止）。折叠链上每一层（callee flow verdict → call step verdict → caller flow verdict）在报告中都可展开，但 `failedStepPath` 指根因那一帧。

### 2.2 失败时保存的完整上下文

失败定位的产品承诺是：**看卷宗不用复跑**。step 终局为 fail / unknown 时，以下内容已经全部在 RunLog / store 里（多数在正常路径就落了盘，失败不需要额外抢救动作——这是事件溯源的红利）：

| 类别 | 载体 | 内容 |
|---|---|---|
| 输入值 | `StepRecord.resolvedInputs` | ready 时求值的实参快照（含 call step 的 inputs 快照）；resume 不重算，故卷宗里的值就是当时用的值 |
| 动作史 | `StepRecord.attempts`（`AttemptRecord[]`） | 每次 attempt 的 `callId`、四分终态 `outcome`、`ErrorInfo{code,message,retryable,details}` 原文、映射后的 `errorClass`、`execution.mode`（含 `coordinateFallback` 的 `fallbackReason: semanticInteractionUnavailable \| platformLimitation`）、起止时间 |
| 观测 | `StepRecord.observations`（`ObservationRecord[]`） | before/after Observation 与显式 observe 产物；**omission 类型化原因**（`uiSnapshotOmission: driverUnsupported\|policy\|protectedAction`、`screenshotOmission: policy\|protectedAction`）如实入卷——「为什么是 unknown」必须可追溯到「哪路观测缺料」 |
| 证据 | `StepRecord.evidence`（`EvidenceRef[]`） | 本地内容寻址（`AssetRef` + sha256 + localPath）；`observing` 阶段已本地化（骨架 §6.6：`ui.snapshot.get` 仅本 Session 活跃期可读，session 删除是整段式，不能指望远端长存）。**2026-07-18 收编评审条目③**：观测类之外，结算/verdict/human 类证据以**判定清单**入账——`verdictRecorded` 增 `localized[]`/`localizationGaps[]`（unverified 步以 `stepExited` 同名字段代载），fold 与卷宗以同一去重规则 (sha256, assetId) 首现胜合并；本地化失败是类型化缺口（卷宗 `evidenceGaps` 缺口画廊 + report 每步计数），绝不静默；清单有界（超上限条目转类型化缺口）；离线重判清单恒空 |
| 断言明细 | `StepRecord.assertionOutcomes` | 每条 assertion 的 `result: pass\|fail\|unknown`、实际完成求值的 `channel`、人读 `reason`（fail 写谓词与实测值之差；unknown 写缺料链——依次哪个通道为何不可用） |
| 判定 | `StepRecord.verdict` | `{ status, degraded, supersedes? }`；`degraded` 为 true 时 Evidence 中有降级事实记录（骨架 §6.4） |
| provider 状态摘要 | `stepExited` / `runSuspended` 事件 payload 的 `providerStateSummary` 字段（**已收编**，2026-07-18 收编评审；字段形状见下，两处 Option 化偏离见收编注记） | 失败时点的会话侧写 |
| 悬挂意图 | `CheckpointView.frontier.pendingIntent` | 崩溃型失败时的 `{ callId, argsSnapshot }`，resume 凭它 reconcile |

`providerStateSummary`（本篇引入；**2026-07-18 收编评审已收编**进 RunLog 事件 payload 规范——骨架 §6.1 M1 注记同步修订）：

```ts
interface ProviderStateSummary {
  sessionLineage: string[];                        // 历代 DeviceRail sessionId（尽力值：checkpoint 已知谱系 + 采集时点活跃 session）
  eventCursor?: { sessionId: string; lastSequence: number };  // 收编偏离①：Option 化——采集即时点 currentCursor() RPC，失败时缺席（绝不用 bind 时旧值冒充，原则 4）
  attestation: { lockfileDigest: Hash; attestedAt: string };
  health: { ok: boolean; degraded?: string };      // ProviderSession.health() 时点值
  deviceId: string;
  platform?: string;                               // 收编偏离②：Option 化——装配层未提供时缺席（SPI attestation 不携带，runner 无从诚实取值）
}
```

它在两个时机写入：**每个退出时生效 verdict 为 fail/unknown 的 `stepExited`**（内涵式判据，经统一采集 helper 覆盖 consult 路径、settle_error、human 步结算、call 步/assert 步 consult 退出——不是枚举三个位点；abort 后置的 `stepExited(aborted)` **裁定不写**：中止终局不作语义主张，其 fail/unknown verdict 已独立在账），以及**每次 `runSuspended`**（含 resume 前置阻塞路径——凡写入时 session 尚活即采集）。`health()` 与 `currentCursor()` 同为活 RPC，共用一个采集期限；`health()` 调用失败（如 `transport_lost` 场景）时记 `{ ok: false, degraded: "<errorClass>" }`，`currentCursor()` 失败时 `eventCursor` 缺席——任何采集失败都不阻塞挂起路径。摘要是**纯法证记录**：resume reconcile 只读 `checkpoint.binding.eventCursor`（§4.4），fold 不从摘要收割 binding，摘要在 v0.1 绝不成为控制输入。**跨段注（Wave B 评审裁定）**：前段已录 fail/unknown verdict 的步在新段再入并退出时，其 `stepExited` 携带**resume 代**的采集（谱系/cursor 自述其代际，不冒充失败时点）；失败时点的侧写在前段配对的 `runSuspended` 上——两代记录并存，各自如实。

### 2.3 `pointlock locate`：卷宗的检索面

```
pointlock locate <runId> <path>          # path 接受规范串或 JSON PathFrame[]
```

返回（JSON，供人与工具消费）：

1. **IR 节点**：该 RunPath 对应的 `StepIR` 全文（含 `effectHash/judgeHash`、binding attempts、assertions、`acceptExecutionModes`）；
2. **源定位**：经 `sourceMap` 译回的 YAML 文件、span、宏展开链（origin trace）；
3. **运行记录**：该步全部 `AttemptRecord` / `ObservationRecord` / `EvidenceRef`（含 localPath，可直接打开截图/uiSnapshot）/ `assertionOutcomes` / verdict 链（含 `supersedes` 历史）；
4. **环境**：所在帧的 `inputsSnapshot` 与 `vars` 快照、`providerStateSummary`。

**输出形状钉死（R14，骨架 §10.1/§10.2）**：`locate` 的 JSON 输出形状**即**投影协议（Projection Protocol）五族 DTO 之一的 `StepDossierView`——上列四项内容就是该 DTO 的字段面。查询层归属 `pointlock-store` 的 projection 模块（骨架 §10.2），CLI 与一切渲染器（含 08 篇 step 检查器——「step 检查器 = locate 的图形化」既有裁决借此类型化）共享同一查询层、同一形状，不存在第二条读取路径。

`locate` 是纯读操作，只查 store，不触碰设备。其 `--format json` 输出的卷宗同时是 LLM 修复提议循环的输入（R13，骨架 §6.9 收编 3；流程见 §5.1）。

---

## 3. Checkpoint 模型

### 3.1 粒度（骨架 §6.6 的操作化）

**粒度 = step 边界 + act 前 WAL 意图点**，展开为四个物化时机：

| 时机 | 触发事件 | 物化内容 |
|---|---|---|
| step 完成 | `stepExited`（`checkpoint: true` 的步，默认 true；macro 展开体内默认 false） | completed 追加该步 `StepRecord`；frontier 前移；~~`eventCursor` 以 `ProviderSession.currentCursor()` 刷新水位~~（**2026-07-18 收编评审注**：v0.1 缓期——cursor 保持代粒度（bind/resume 时各刷一次），逐步刷新登记为性能细化项；早扫描起点是保守安全的） |
| act 意图 | `actionIntent`（先 fsync 再 dispatch，骨架 §6.2） | frontier 记 `pendingIntent{ callId, argsSnapshot }`——崩溃窗口的钥匙 |
| 帧转移 | `callFramePushed` / `callFramePopped` | `frames` 栈更新（含入参快照 / 输出快照） |
| 挂起点 | `humanRequested` / `runSuspended` | `humanPending` / run 状态；写 `providerStateSummary`（**收编评审注**：摘要只随配对的 `runSuspended` 落账——每条 AwaitHuman 路径都会追加一条——`humanRequested` payload 不变）。`humanRequested` 含 `purpose: "supervision"` 的监督请求（R13，骨架 §6.1/§6.9） |

为什么 step 边界就够、不需要 phase 级 checkpoint：一个 action step 内唯一不可重做的动作是 mutating act；`observe` 只读、`assert` 纯计算（Evidence 已在 `observing` 阶段本地化，`asserting` 无 I/O，骨架 §6.2），崩在 `observing/asserting` 中段时从本步 act 之后整段重做 observe+assert，结果必然一致——而 act 本身由 `actionIntent` WAL + reconcile 护住。这就是「step 边界 + WAL 点」二元粒度的完备性论证。

**监督门控点（R13，骨架 §6.9；里程碑 M2）**：run 以 `--supervise <mutating|all>` 启动时，runner 状态机在命中策略的 step **进入 `acting` 之前**插入监督问答（此时 `resolvedInputs` 快照已可呈现）。WAL 顺序钉死：`humanRequested(purpose="supervision", presents 含 runPath / actionName / resolvedInputs 摘要)` 先 fsync → 通知 → `humanResponded(decision)` → `decision = proceed` 才写 `actionIntent` → dispatch。监督问答发生在 `actionIntent` 之前，因此不改变上述二元粒度与 `pendingIntent` 语义——**崩溃发生在问答中间时，`actionIntent` 必然尚未落盘**，重启后 supervision 请求仍 pending，惰性结算语义与 human step 同款（§4.6）；decision 封闭枚举 `proceed | abort | suspend`（v0.1 刻意无 `skip`），监督问答不产生 verdict、默认无超时。`supervisePolicy` 属 run 级策略，记入本段起始事件（`run` 段为 `runStarted`、resume 段为 `runResumed`）payload，不影响 `irHash`、不进任何哈希域；不跨段隐式继承（骨架 §6.9）——每段以启动命令旗标为准，resume 传入即覆盖、未传即本段无监督，payload 显式记 `null`。

### 3.2 内容：`CheckpointView` 逐字段

类型定义以骨架 §6.6 为准，此处给出运行期语义与本篇对 `CallFrame` 的细化：

```ts
export interface CheckpointView {
  runId: string;
  irHash: Hash;                     // 本 run 执行的 FlowIR —— 修复对齐的旧边
  lockfileDigest: Hash;             // attestation 依据；resume 时 capability_drift 检查
  paramsSnapshot: unknown;          // run 输入快照
  binding: {
    deviceId: string;
    sessionLineage: string[];       // 历代 DeviceRail sessionId（断代重开追加）
    eventCursor: { sessionId: string; lastSequence: number };
                                    // DeviceRail 事件水位：事件信封 sequence 字段的最大已消费值；
                                    // resume 后用 events.list { afterSequence } 增量核对，
                                    // 订阅路径对应 events.stream.open（events.stream.v1）的 epoch+cursor 续传语义。
                                    // 跨 session 不假装连续：sessionId 换代则 lastSequence 归零重记
  };
  completed: StepRecord[];          // 已完成步的全记录（§2.2 的卷宗单元）
  frames: CallFrame[];              // 调用栈；frames[0] 为根 flow 帧（本篇细化，见下）
  frontier: {
    runPath: RunPath;
    state: StepState;
    pendingIntent?: { callId: string; argsSnapshot: unknown };
  };
  humanPending?: { runPath: RunPath; requestId: string;
                   purpose: "step" | "supervision";   // R13（骨架 §6.6）；supervision 场景 prompt 为自动生成的门控描述
                   prompt: string };
}
```

`CallFrame` 细化（骨架 A.3 已登记类型名，字段为本篇落定；变量快照住在这里）：

```ts
export interface CallFrame {
  runPath: RunPath;                 // 根帧 = [flow 帧]；其余 = 到 call 帧为止的路径
  flowId: string;
  irHash: Hash;                     // callee 的 irHash（根帧 = CheckpointView.irHash）
  inputsSnapshot: unknown;          // call-by-value 入参快照（根帧引用 paramsSnapshot）
  vars: Record<string, unknown>;    // 本帧已执行 let 步累积的 vars.*（SSA，只增不改）
  iterStack: { as: string; index: number; item: unknown; total: number }[];
                                    // 帧内活跃 foreach 嵌套；item 为 resolvedInputs 中的快照值
  nextIndex: number[];              // 帧内结构游标：到下一个待执行 step 的体内路径（含 if 分支选择）
}
```

任务书要求的四项内容逐一对号：**变量快照** = `frames[*].vars + iterStack + inputsSnapshot`；**已完成 step 的 verdict 与输出** = `completed[*].verdict / .output`；**provider 游标** = `binding.eventCursor`（即 DeviceRail 事件信封的 `sequence` 水位）；**irHash** = 顶层 `irHash` + 各帧 `irHash` + 各 `StepRecord.effectHash/judgeHash`。

### 3.3 存储：v0.1 = 单 SQLite 库 + 内容寻址证据区

`pointlock-store`（rusqlite，`journal_mode=WAL`；R12 由 better-sqlite3 改为 rusqlite，WAL + `synchronous=FULL` 的持久性语义逐条不变，本节全部规则原样成立）。目录布局：

```
<project>/.pointlock/
  store/
    pointlock.db                # 全部 run 的 RunLog + checkpoint 物化
    evidence/
      sha256/
        ab/abcdef01…89.bin     # 内容寻址，两级扇出；扩展名按 mediaType 附加
  pointlock.lock.json           # CapabilityLockfile（进版本库）
  dist/
    checkout.flowir.json       # pointlock compile 产物（FlowIR，进版本库或制品库均可）
```

DDL（完整，可直接建库）：

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = FULL;        -- actionIntent 的 fsync 语义依赖 FULL

CREATE TABLE run (
  run_id           TEXT PRIMARY KEY,
  flow_id          TEXT NOT NULL,
  ir_hash          TEXT NOT NULL,
  lockfile_digest  TEXT NOT NULL,
  params_snapshot  TEXT NOT NULL,            -- JSON
  binding          TEXT NOT NULL,            -- BindingState 规范 JSON（M1 收编：
                                             --  事件不携带 binding 而 fold 需输入）
  status           TEXT NOT NULL CHECK (status IN
                     ('running','suspended','awaitingHuman','finished')),
                     -- awaitingHuman 兼用于 human step 等待与监督门控等待
                     -- （purpose 判别在 humanPending.purpose，非 run 级；R13）
  created_at_ms    INTEGER NOT NULL
);

CREATE TABLE run_log (                        -- append-only；I1 的真相表
  run_id   TEXT    NOT NULL REFERENCES run(run_id),
  seq      INTEGER NOT NULL,                  -- run 内单调，事务内分配
  type     TEXT    NOT NULL,                  -- 骨架 §6.1 的 17 种封闭事件名
  at_ms    INTEGER NOT NULL,
  run_path TEXT    NOT NULL,                  -- PathFrame[] 的规范 JSON
  payload  TEXT    NOT NULL,                  -- JSON（actionIntent: {callId, argsSnapshot} 等）
  PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;

CREATE TABLE checkpoint (                     -- 物化视图；随时可 DROP 重建
  run_id  TEXT PRIMARY KEY REFERENCES run(run_id),
  log_seq INTEGER NOT NULL,                   -- 物化到 run_log 的哪个 seq
  view    TEXT NOT NULL                       -- CheckpointView 的规范 JSON
);

CREATE TABLE evidence (                       -- 跨 run 去重的内容寻址索引
  sha256     TEXT PRIMARY KEY,
  media_type TEXT NOT NULL,
  byte_size  INTEGER NOT NULL,
  local_path TEXT NOT NULL
);

CREATE TABLE evidence_ref (                   -- run/step → evidence 多对多
  run_id   TEXT NOT NULL,
  seq      INTEGER NOT NULL,                  -- 产生该证据的 run_log 事件
  asset_id TEXT NOT NULL,                     -- DeviceRail AssetRef.id 原文
  sha256   TEXT NOT NULL REFERENCES evidence(sha256),
  PRIMARY KEY (run_id, seq, asset_id)
);
```

**持久性与一致性规则（钉死）**：

1. `actionIntent` 的追加是独立事务，commit 返回后才允许 `provider.execute` dispatch（WAL + `synchronous=FULL` 保证落盘先于副作用——这就是骨架「先 fsync 再 dispatch」的实现）。
2. `checkpoint.view` 的更新与对应 `run_log` 追加**同一事务**提交：读到的 checkpoint 永远与某个确切 `log_seq` 一致，不存在半新半旧视图。
3. Evidence 先写字节文件、fsync、再插 `evidence` 行、再追加引用它的 `run_log` 事件（file-before-row-before-log）。崩溃最多留下孤儿文件（可 GC），绝不出现「日志引用了不存在的证据」。
4. `run_log` 无 UPDATE/DELETE 路径；重判产生新 `verdictRecorded` 事件并在 payload 里带 `supersedes`，旧事件不动（骨架 §2 概念 12）。

**为什么 SQLite 而不是纯文件目录**：checkpoint 需要「意图 + 视图 + 日志」的原子多写（规则 1/2），文件方案要自己发明 journal；单文件库天然支持 `pointlock inspect` 在 run 进行中并发只读（WAL 模式读不阻塞写）；全部 run 共库使跨 run 的 evidence 去重（`sha256` 主键）零成本。纯文件保留给它擅长的部分——证据字节本身。**重建通道**：`pointlock inspect <runId> --rebuild-checkpoint` 从 `run_log` 全量重放折叠出 `CheckpointView` 并比对现存物化行，不一致即 store 层 bug（I1 的运行时自检）。（M1 收编）fold 的原料自足：`StepRecord` 的 `effectHash/judgeHash/resolvedInputs` 折叠自 `stepEntered` payload、`state/output` 折叠自 `stepExited` payload（骨架 §6.1 M1 注记），`CheckpointView.binding` 折叠自 run 表 `binding` 列——重放折叠不需回读 IR；跨 IR resume 的子域比对仍需旧 IR（`--old-ir`，M1 保留）。

**projection 只读查询模块（R14，骨架 §10.2）**：`pointlock-store` 新增 projection 模块，承载投影协议五族 DTO 的 Rust 定义与只读查询层（store 是读侧权威，不新增 crate；§2.3 的 locate 查询层同归属，schemars 由此生成 `@pointlock/projection-types`）。该模块只读本节各表，不新增任何写路径——上述持久性与一致性规则（写侧语义）不受影响。

---

## 4. Resume 语义

### 4.1 正确性条件（骨架 §6.7 的展开）

resume 合法 ⟺ 三个条件同时成立：

- **(A) 记录对齐**：frontier 之前每个 completed step 的记录在（可能已修复的）新 IR 下仍被认可——按 §5 的五类对齐产出 `alignmentReport`，随 `runResumed` 事件入 RunLog；
- **(B) 意图核对**：frontier 上的 `pendingIntent` 已经 `provider.reconcile(callId)` 判明下落（§4.4）；
- **(C) 世界探针**：世界通过 resume 探针（§4.2）。

任一条件不满足都不是「失败」，而是各自的显式路径：(A) 不满足 → resume 点回退或拒绝（§5）；(B) 不确定 → `onResumeDrift`/人裁决；(C) 不通过 → `drifted` → `onResumeDrift`。resume 从不静默放行。

**(A)×(B) 的顺序与交互（钉死）**：**对齐先行**，reconcile 随后但**无条件执行**——frontier 有 `pendingIntent` 就必须 reconcile 判明世界事实，即使该步在新 IR 下已 `effectDirty` 也不豁免（IR 改了，世界不会因此变回去）。二者的交叉语义：

- frontier 步对齐为 `reusable`（或重判成功的 `judgeDirty`）且 `fate: completed` → 采认落盘终态 `outcome`（M1 收编：四分 `ActionOutcome`）：`succeeded` 从 `observing → asserting` 续跑（§4.4 主路径）；`failed/cancelled/timedOut` 补写 `actionSettled` 后走与实时执行相同的 settled 处置（错误分类、重试判定）；
- frontier 步为 `effectDirty`（含 inputs 变化的整帧失效、`orderInvalidated` 降级）且 `fate: completed` → 旧 `result` **一律不采认**——它是旧 binding 产出的 `ActionResult`，与新 IR 的该步已不是同一个动作；该 result 与 reconcile 事实只作为 Evidence 留档入 RunLog。此时看旧 attempt 的 outcome：
  - outcome ∈ {`succeeded`, `timedOut`} 而该步**无 verdict**（崩在 act 之后、判定之前）→ 效果可能已在世界里，重新执行修改后的步 = 潜在双重效果。该步**视同 `priorVerdict: "unknown"` 进入 `requiresConfirmation`**（`cause: "frontierUnknown"`，§5.4），走同一 `--allow-mutating-reexec` + preflight 守护路径——「无 verdict」不构成绕过确认通道的理由；
  - outcome ∈ {`failed`, `cancelled`} → 旧动作未生效，按普通 `effectDirty` 重执行，无需确认。
- `fate: neverDispatched / logUnavailable` → 不受对齐分类影响，按 §4.4 决策表处置（`logUnavailable` 的非幂等分支同样归入 `requiresConfirmation` 或人裁决）。

附加门槛（先于 A/B/C）：重开 `ProviderSession` 时 attestation 重放——`openSession` 内 `system.hello`（IR 的 `requiredFeatures` 全量进 `FeatureOffer.required`）+ `device.capabilities`，与 `lockfileDigest` 比对，不一致 → `capability_drift`，拒跑（骨架 §4.1）。暂停期间 daemon 升级换掉了能力面，是比世界漂移更根本的失效。

### 4.2 世界状态假设：用 preflight 表达，用求值校验

「resume 假设世界处于什么状态」不允许存在于作者脑子里或文档注释里——它必须是 `StepBase.preflight`（`AssertionIR[]`，纯谓词，同 `expect` 一样的谓词语言：`elementState / elementText / expr / visual`，沿 `verifyVia` 求值）。语义（骨架 §6.7-C 的操作化）：

1. resume 的首个待执行 step 进入 `probing`，**强制**求值其 `preflight`；该步无声明则跳过并在报告标 `unprobed`（诚实优先于安慰）。**载体钉死**：`preflight` 的唯一载体是 `StepBase.preflight`——`FlowIR`（骨架 §3 / 02 的封闭字段表）没有 flow 级探针字段，A.7 顶层 YAML 键封闭清单也没有对应键，所谓「flow 级默认探针」在类型体系中无载体，语义统一收敛为**步级声明，无声明即 `unprobed`**（骨架 §6.7-C 的旧措辞按此勘正）。
2. 探针求值走 verify-chain 同款规则（骨架 §6.3）：完成求值且成立 → 放行进 `acting`；完成求值且不成立 → `drifted`；链耗尽无法求值 → 同样按 `drifted` 处理（无法确认世界 ≠ 世界正常，原则 4）。
3. `drifted` → `onResumeDrift` handler。典型配置 `{kind:"repair", flowRef}`：跑恢复锚点 subflow（如 ensureLoggedIn），跑完**重探**；超 `maxTriggers` → `awaitingHuman`，人以 `mode: "repairWorld"` 修世界后裁决。
4. 探针是 readonly 的（AssertionIR 无副作用位），重探任意次安全。

写作指引（给 flow 作者的契约）：`preflight` 描述「本步开跑前世界必须长什么样」，它同时服务两个场景——正常执行的前置校验、resume 的漂移检测。这就是骨架 R9 把它与后置断言 `expect` 强行分名的原因：**`preflight` 问过去（世界就位了吗），`expect` 问未来（动作生效了吗），永不混用**。

### 4.3 幂等性标注与「绝不重放」规则

作者在 step 上有两个声明位（骨架 `ActionStepIR`）：

- `effect: mutating | readonly`（action step 必填；`pure` 保留给 let/表达式类）——**能力声明**：这步会不会改世界。
- `idempotent?: boolean`（默认视为 false）——**重放许可**：改了世界的步，重放一次会不会把世界带到不同的地方。`set_value` 类收敛动作可标 true；`tap` 提交类绝不该标。

两个标注只在两处被消费，且语义封闭：

| 消费点 | readonly | mutating + idempotent | mutating（非幂等） |
|---|---|---|---|
| `action_timed_out` 自动重试（骨架 §5） | 允许 | 允许 | 禁止——先 reconcile/observe 确认，确认不了走 unknown 路径 |
| resume 的 `logUnavailable` 分支（§4.4） | 重放 | 重放 | **绝不自动重放**：`onResumeDrift` → 默认 escalate 人裁决 |

据此可给出 I2 的证明义务：completed 记录经对齐采认后**原样复用，不重执行**；重执行只发生在 (i) `effectDirty` 失效后作者显式确认的 resume（§5.4），(ii) 位置失效重放经 §5.4 `requiresConfirmation` 逐一确认后的 resume（`reusable` 记录本身不重跑，但排在 resume 点之后的记录会被作废重执行——作废重执行同样要过门槛），(iii) handler `retry`（作者显式策略），(iv) 人的 `redo` 裁决——四者全是显式意志，无一来自 runner 的自作主张。**非幂等已完成 step 绝不被 runner 自动重放**在实现上就是：对齐层没有任何「重跑 reusable 记录」的代码路径；resume 点之后的重执行必须先通过 §5.4 的收集规则筛一遍，命中即 fail-closed 等待 `--allow-mutating-reexec`。

### 4.4 悬挂意图核对：reconcile 决策表

崩溃可能发生在 `actionIntent` 落盘之后、四分终态入账之前。resume 凭 `pendingIntent.callId` 调 `ProviderSession.reconcile(callId)`，在事件日志中匹配 `actionStarted / actionCompleted` payload 的 `call.id === callId`（骨架 §4.2）。

**查询目标钉死（`neverDispatched` 判别的全部前提）**：reconcile 必须查**签发该 intent 的那个 session** 的事件日志——即 `checkpoint.binding.eventCursor.sessionId`。`neverDispatched`（「日志无踪迹，可安全重放」）与 `logUnavailable` 的判别完全建立在「读到了原 session 的完整事件段」之上；§4.5 规定 resume 一律开**新** session，若按新 session 查询（`session.current` 指向的是新 session 的空日志），旧 `callId` 必然无踪迹，会得出假阴性的 `neverDispatched`，进而自动重放一个效果可能已发生的非幂等动作。落地路径分两种：

- **同进程内核对**（abort / RPC 信封超时后，签发 session 仍活跃）：`session.current` 确认当前会话即签发会话，`events.list { afterSequence: eventCursor.lastSequence }` 扫描；
- **跨 session resume（常态）**：**绝不查新 session**——以 `eventCursor.sessionId` 为目标读取旧 session 日志：经 `sessions.list` 确认其存在，用 `events.list { afterSequence }` 对旧 session 补扫（04 §9.8.3 的既定用法），或 `session.export` 整段导出（协商到 `session.export.page.v1` 时带 `{ limit, afterSequence }` 分页）。

**判别规则**：旧 session 日志不可达或已删除（daemon 重启、`events.clear`、session 整段删除——session 事件日志只能整段删除）→ **一律返回 `logUnavailable`**；只有确认读到了原 session 自 `lastSequence` 起的完整事件段、且其中无 `call.id === callId` 踪迹，才允许返回 `neverDispatched`。决策表（骨架 §6.7-B 展开）：

| `ReconcileResult.fate` | 含义 | 处置 |
|---|---|---|
| `completed` | 终态已在 daemon 落盘（DeviceRail 终态有 durable shield，`cancelled/timedOut` 也是有记录的终态） | 采认落盘终态（`outcome: ActionOutcome`，M1 收编）：`succeeded` 从 `observing → asserting` 正常续跑；非成功终态补写 `actionSettled` 后走实时 settled 同路径处置。**不重放**。**前提**：该步经 (A) 对齐为 `reusable`/重判成功的 `judgeDirty`；若为 `effectDirty`，outcome 不采认、仅留档，按 §4.1 交叉语义处置 |
| `neverDispatched` | **签发 session** 的完整事件段确认无踪迹（查询目标见上；查不到日志不得归入此类） | 安全重放：新 attempt、新 `callId`、新 `actionIntent` |
| `startedNoTerminal` | 有 `actionStarted` 无 `actionCompleted`——理论不应出现（终态落盘有 shield） | 按不确定分支处理（同 `logUnavailable`），并在报告中标注协议异常 |
| `logUnavailable` | 旧 session 已结束 / 日志不可达（session 删除是整段式） | `idempotent: true` 或 `effect: "readonly"` → 重放；否则触发 `onResumeDrift`，默认 escalate：人以 `HumanStepIR{ mode: "repairWorld" }` 对着 `presents` 里的世界观测裁决 `adopt / redo / abort` |

注意 `logUnavailable` 在跨 session resume 中是**常态而非病态**：崩溃后旧 DeviceRail session 往往已随 daemon 一起消亡。这正是「Evidence 必须在 `observing` 阶段本地化」的第二重理由——人裁决 `adopt/redo` 时呈上的证据来自本地库，不依赖死去的 session。

### 4.5 Session 断代

resume 一律开新 DeviceRail session（`session.start`），绝不尝试续用旧连接：旧 `sessionId` 追加进 `sessionLineage`；`eventCursor` 换代归零、不假装跨 session 连续——**换代发生在 §4.4 的 reconcile 完成之后**，旧 `eventCursor{ sessionId, lastSequence }` 正是 reconcile 定位签发 session 与扫描起点的凭据，归零不抹除 checkpoint 中的旧值。

> **已收编（2026-07-18 收编评审；Wave C 实现修订同步）——换代的账本载体**：`runResumed` payload 增可选 `eventCursor: { sessionId, lastSequence }`——新代 session 的重置水位（reconcile 决断之后、`runResumed` 落账之前经 `currentCursor()` 取值；sessionId 即谱系扩展项；RPC 失败即缺席）。fold 规则：`runResumed` 携该字段 → `binding.sessionLineage` 追加 + `binding.eventCursor` 重置；缺席（旧账本）→ 零变动（如实单代呈现）。**签发凭据机制（实现反哺定形，取代本注初稿的 fold 盖章 + PendingIntent.issuingCursor 设计）**：持久 `PendingIntent` 与 CheckpointView **形不扩**（存量 store 的 I1 对照零风险）；harvest 按账本顺序为每个 `actionIntent` 计算**逐 intent 签发态**——`FromBinding`（intent 前无任何 resume：凭据 = **bind 时 run 行 binding cursor**，绝非折叠视图里被重置的当前值）/ `Known(cursor)`（前面最近一次携 cursor 的 resume：该段自证其代，不受更早未知段拖累）/ `Unknown`（前面有不携 cursor 的 resume：签发代不可知）。`Unknown` 或 harvest 无此 intent 记录 → **不发 RPC**，直接 logUnavailable 不确定分支——绝不用捏造凭据 reconcile（未来具备跨代扫描的 provider 同样禁止以兜底凭据裁定 neverDispatched）。SPI `reconcile(callId, issuing)` 显式携带凭据（骨架 §4.2 同步修订）；一致性套件含通用探针：外代凭据 + 本代已派发 callId → 绝不许 neverDispatched。devicerail 守卫：签发 ≠ 当前且旧日志不可达 → logUnavailable（修复既存缺陷：此前扫新 session 日志，可产假 neverDispatched）；fake 的 journal 为世界级（跨代检索天然可用），neverDispatched 在其上依然可靠。旧 session 的证据已本地化，无需跨 session 读取（骨架 §6.7）。`UiNodeRef { observationId, context: { contextKind, contextId, documentEpoch }, stableNodeId }` 绑定 `documentEpoch`，resume 后一律视为过期——目标形态：编译器对跨 epoch 引用注入 revalidate（`findElement` 重定位）step，runner 不需要运行期特判。**（2026-07-28 勘注，以实现为准）**v0.1 实现为更强的 fail-closed：动态元素目标（UiNodeRef 链引，04 §9.5）整体是编译期 typed 拒绝（`RF2026` 谓词静态性 / normalize 动态目标拒绝），过期引用根本到不了 runner——revalidate 注入随动态目标一起属后续里程碑，本句的「已注入」在 v0.1 不成立。旧 run 因 `transport_lost` 挂起的场景由此完整闭环：`runSuspended` → 修复环境 → `pointlock resume <runId>` → 新 session + attestation → A/B/C 三关 → `runResumed{alignmentReport}` → 续跑。

### 4.6 Resume 落回嵌套现场

`CheckpointView.frames + frontier` 足以把执行精确送回任意深度的现场，**不重启任何帧**：

1. 自根帧起逐帧重建作用域（`inputsSnapshot / vars / iterStack` 直接采用快照——表达式不重算，杜绝重判后下游漂移，骨架 §6.6）；
2. 按各帧 `nextIndex` 走到 frontier step；沿途 completed steps 直接以 StepRecord 入账（经 §5 对齐）；
3. foreach 现场：`iterStack` 里的 `index/item` 快照恢复 `iter.<as>`，从中断迭代继续，已完成迭代不重跑；
4. `humanPending` 现场：不需要设备动作，重新经 `pointlock-human-cli` 呈现 `humanRequested{requestId, presents}`，等 `humanResponded`（骨架 §6.8——human step 的 durable 语义本来就是为跨进程等待设计的）。`purpose: "supervision"` 的监督请求同款处置（R13，骨架 §6.9）：崩溃发生在监督问答中间时，重启后该请求仍 pending、惰性结算，`humanResponded(decision)` 且 `decision = proceed` 后才写 `actionIntent` 进入 dispatch（顺序见 §3.1 监督门控点注记）——监督请求复用同一 store 单写者仲裁、同一通知通道、同一收件箱，不新建第二套管道。监督策略逐段生效（骨架 §6.9）：`runResumed` 显式记录本段 `supervisePolicy`（resume 未传 `--supervise` 即本段无监督、记 `null`）；既有 pending 的监督请求不受本段策略影响，照常等待回应（`abort` / `suspend` 的落账细则见骨架 §6.9）。

---

## 5. 局部修复流程

场景：run 在第 N 步失败（或作者对已 pass 的某步反悔），修改 YAML 里的一个 step 或整个 subflow，期望**不重跑设备上已经做对的部分**，从修复点继续。

### 5.1 流程总览

```
①失败定位            ②修复             ③重编译                ④对齐               ⑤续跑
pointlock locate  →  改 YAML/callee →  pointlock compile  →  pointlock resume     →  从 resume 点
<runId> <path>      （不改 stepId）    产出新 FlowIR         <runId> --ir <新IR>     继续执行
                                       （新 irHash；每步       按 stepId 匹配旧
                                        effectHash/judgeHash    StepRecord，产出
                                        重算）                  alignmentReport
```

**LLM 修复提议循环（R13，骨架 §6.9 收编 3；CLI 形态 M2）**：①–②可由 NL 起草器（`@pointlock/nl-drafter`）代跑——失败 → `pointlock locate --format json` 取判案卷宗（§2.3）→ 起草器提议 YAML patch（diff 形态）→ **人审批门**：呈现 diff 与 align-preview 的 `alignmentReport` 预览（§5.2 的对齐产物在此被预先消费：哪些历史保留、哪些重跑、哪些需确认）→ 批准 → resume。LLM 只产 YAML 草稿，③④⑤的执法者仍是编译器与对齐器；UI 审批表单属 M3a，且遵守 06 §4.2 既有裁决——v0.1 webUi 不收响应，批准动作经 human-cli / CLI 等价通道完成。

关键纪律：**修复时不改 `stepId`**。stepId 是作者提供、flow 内唯一、稳定的身份（骨架 §3）——对齐按 stepId 匹配，改 id 等于宣告「这是一个新步」（`new`）并把旧记录判成 `orphaned`。改语义不改身份，是局部修复的作者侧契约。

subflow 修复的传导是自动的：改 `login.flow.yaml` → `login` 新 irHash → caller 的 `subflows` 引用更新 → `purchase`（call step）的 `effectHash` 变（骨架双哈希规则：call 的 effectHash 覆盖 `flowRef.irHash + inputs`）→ 对齐在 `purchase` 处发现 `effectDirty`——但见 §5.2，**call step 的失效在「inputs 未变、变更完全来自 callee irHash、帧未完成」时可以下钻**，不必整体重调；inputs 有变则整帧失效，禁止下钻。

### 5.2 对齐规则（骨架 §6.7-A 的执行细则）

新 IR 正序遍历，按 `stepId` 匹配旧 `StepRecord`，逐步分类（封闭枚举 `AlignmentClass`）：

| 情形 | 分类 | 处置 |
|---|---|---|
| id 同，effectHash 同，judgeHash 同 | `reusable` | 直接采认：verdict、output、evidence 全复用 |
| id 同，effectHash 同，judgeHash 变 | `judgeDirty` | **离线重判**（§5.3）：新断言对存档 Observation/Evidence 重新求值，新 Verdict `supersedes` 旧的，设备零 I/O |
| id 同，effectHash 变 | `effectDirty` | 记录不采认；该步及其数据依赖下游全部失效 |
| 新 IR 有、旧记录无 | `new` | resume 点不得晚于它 |
| 旧记录有、新 IR 无 | `orphaned` | 归档不采认（若其 output 被下游引用，编译期 `check` 已报悬空引用错，到不了运行期） |

**resume 点** = 遍历序上第一个「非 `reusable` 且非重判成功的 `judgeDirty`」的位置。v0.1 是顺序执行模型，所以任务书的「改了第 N 步则 N 之前的结果可复用、N 起重新执行」精确成立：resume 点之前全部采认，resume 点起全部重新执行（包括其间与修改无数据依赖的步——顺序模型下它们本来就排在后面）。`check` 阶段建好的数据依赖图在这里的用途是**解释与审查**：`alignmentReport` 里每个失效步都标明失效原因链（直接 effectDirty，还是传染自哪个上游），供人审查 resume 决定是否合理。

**位置失效不豁免副作用门槛（I2 (ii) 的落点）**：「resume 点起全部重新执行」中可能包含这样的步——自身双哈希未变、逐步分类本是 `reusable`、已有 pass 终局，只因排在 resume 点之后而被作废重执行（例：修了第 3 步，第 7 步「提交订单」未被修改却要二次执行）。它们不落入「作者修改的步」的范畴，却同样构成非幂等 mutating 动作对世界的二次执行。因此对齐必须把 **resume 点起每个满足 §5.4 副作用判据的已完成步**（有 StepRecord、`effect: mutating`、未声明 `idempotent: true`、记录中存在生效 attempt，**无论其 effectHash/judgeHash 是否变化**）收进 `requiresConfirmation`（`cause: "positionalReplay"`），与被修改的步走同一条 `--allow-mutating-reexec` + preflight 守护通道（§5.4）。三个「合法来源」之外的第四来源必须显式确认，正是 I2 修订后的第 (ii) 款。

**顺序一致性校验（采认的前提）**：按 `stepId` 匹配对**重排**不设防——交换两个已完成、互无数据依赖的步，id 与双哈希均不变，逐步分类全是 `reusable`，但世界是按旧顺序产生的，新 IR 声称的执行序从未真实发生过（对顺序敏感的效果——两次 tap 的先后、`assert` 步 `observe: "fresh"` 的拍摄时点——这等于把另一份执行历史冒认成本 IR 的历史）。因此**对齐按 id 匹配，但采认以顺序一致为前提**：把全部匹配上的 completed 记录按新 IR 遍历序排列，校验其旧 RunLog 执行序（`seq`）严格递增；自第一处逆序起（含逆序点），其后所有 completed 记录一律不采认，降级按 `effectDirty` 处置——resume 点不得晚于逆序点，其中满足 §5.4 副作用判据者进 `requiresConfirmation`（`cause: "orderInvalidated"`），由作者确认顺序无关后逐一放行。`alignmentReport` 对应 entry 的 `reason` 写明逆序对（哪两个 stepId 的相对顺序颠倒）。同一规则在 call 帧下钻与 foreach 迭代内部递归适用。

**嵌套结构的对齐细则**：

- **call step 下钻（先辨因，后下钻）**：call 的 `effectHash` 覆盖 `flowRef.irHash + inputs` 两个域（骨架双哈希规则），`effectDirty` 时必须先对比新旧 `CallStepIR.inputs` 的规范形，区分成因，三种情形处置互斥：
  - **变更完全来自 callee irHash、`inputs` 表达式域逐项不变**，且该 call step 在旧 run 中**尚未完成**（崩在 callee 内部）→ 允许下钻：对齐带着新旧 callee IR **递归进帧**，callee 内部按同规则逐步分类，`frames` 中该帧的 `irHash` 更新为新 callee irHash，resume 点可以落在 callee 内部——这正是「修 subflow 从修复点继续」的主路径。下钻的合法性依赖一条必须写明的推论：**活跃帧的 `inputsSnapshot` 永不因新 IR 更新**（resolvedInputs 快照化，§4.6 / 骨架 §6.6）——inputs 表达式未变时，旧快照恰好就是新 IR 会求出的同一组值，callee 内步按哈希对齐才真的成立。
  - **`inputs` 表达式域有任何变化**（无论 callee irHash 变没变）→ **该 call 帧整体失效（fail-closed），禁止下钻**。原因有二，都是静默失效：(a) callee 内部各步的 `effectHash` 覆盖的是实参**表达式**（callee 视角的 `params.*` 引用）而非求值结果，caller 侧换了实参来源（如 `username` 换参数源）时 callee 内步的双哈希纹丝不动，按哈希对齐会把「用旧实参跑出的记录」误判 `reusable`；(b) 依上一条推论，帧的 `inputsSnapshot` 不会按新 IR 重求值，续跑的剩余步骤会继续用旧值。唯一诚实的语义：旧帧全部记录归档不采认，重调 callee（新帧、按新 IR 重新求值 `inputs` 并快照），resume 点不得晚于该 call step；旧帧内若存在 `succeeded`/`timedOut` 的非幂等 mutating attempt，该 call step 进 `requiresConfirmation`（§5.4 生效 attempt 判据——重调即对已生效动作的二次执行）。
  - call step 在旧 run 中**已完成**（有聚合 verdict）→ 无论成因，不下钻：整个 call 记录失效，重调 callee（旧帧记录归档保留），并按 §5.4 门槛处理（call step 视同 mutating 当且仅当其 callee 传递闭包内含 `effect: mutating` 步，见 §5.4）。
- **foreach**：迭代按 `iteration.index` 逐一对齐（v0.1 位置制；`key` 预留）。foreach 头部（`in` 表达式、`as`）变 → 该 foreach 步整体 `effectDirty`；仅 body 内某步变 → 每个已完成迭代内部按步分类——body 步 `judgeDirty` 时每个迭代的该步各自离线重判，全部重判成功则整个 foreach 仍可采认。
- **if**：`cond` 变 → 该 if 步 `effectDirty`（分支选择可能不同，旧记录含当时的分支与 skipped 记录）；仅命中分支 body 内某步变 → 按步分类；仅未命中分支变 → 命中分支记录不受影响，`reusable`。
- **hook 帧下的记录**（handler 审计痕）不参与对齐复用——handler 无输出、不产生可被下游引用的数据（骨架 R10），旧 hook 记录一律归档。

`AlignmentReport`（骨架 A.3 已登记类型名，字段为本篇落定）：

```ts
export interface AlignmentReport {
  oldIrHash: Hash;
  newIrHash: Hash;
  entries: {
    runPath: RunPath;
    stepId: StepId;
    class: AlignmentClass;              // reusable | judgeDirty | effectDirty | new | orphaned
    reason: string;                     // 人读：哪个哈希变了 / 传染自哪个上游（数据依赖链）
    rejudge?: { ok: boolean; newVerdict?: "pass"|"fail"|"unknown"; supersedes: string };
  }[];
  resumePoint: RunPath;
  requiresConfirmation: {               // §5.4：需要显式旗标才放行的项（覆盖被修改步 + 位置/顺序失效重放 + frontier 悬案）
    runPath: RunPath; stepId: StepId;
    cause: "mutatingReexec" | "positionalReplay" | "orderInvalidated" | "frontierUnknown";
                                        // 封闭枚举（M1 收编，骨架 A.4）；首值原记 "effectDirty"，
                                        // 定名 mutatingReexec 以免与 AlignmentClass 值撞名
    priorVerdict: "pass"|"fail"|"unknown";  // 呈现用：门槛判据不读它（2026-07-28 裁决，见 §5.4）；无 verdict 但有生效 attempt → 记 "unknown"
    effect: "mutating";
  }[];
}
```

### 5.3 `judgeDirty`：离线重判的机制与边界

这是双哈希设计兑现处（骨架宪法条款）：`effectHash` 覆盖「对世界做什么」，`judgeHash` 覆盖「如何被判定」。只改断言（加一条 `expect`、收紧文本匹配、调整 `verify_via` 顺序）→ 只有 `judgeHash` 变 → 历史 Observation/Evidence 仍然是「那次动作之后世界的样子」的忠实存档 → 新断言直接对存档重新求值。

可行性由执行语义预先保证：`asserting` 是纯计算，输入 = ActionResult.output + before/after Observation + 本步 observe 产物，全部已本地化、无 I/O（骨架 §6.2）。重判因此是确定性的、免设备的、可任意重复的。

流程：resume 的对齐阶段对每个 `judgeDirty` 步执行新 `AssertionIR[]` 求值 → 折叠出新 Verdict（`supersedes` 指旧 verdict）→ 写 `assertionEvaluated` + `verdictRecorded` 事件（RunLog 留痕与在线判定同构）→ 新 verdict 为 pass/fail 即重判成功，该步继续视为已完成。**边界**：新断言需要旧记录没有的观测通道（如旧步只存了 screenshot，新断言要 `elementState` 需 uiSnapshot 而当时 `uiSnapshotOmission: driverUnsupported`）→ 缺料 → 该断言 unknown → 步 verdict unknown，报告标「补观测点」；作者可接受 unknown 继续，或把该步升级为 `effectDirty` 语义手动重跑（改法：resume 时 `--force-reexecute <stepId>`，进 `requiresConfirmation` 通道）。重判产生 fail 的处理同理——它如实推翻历史（旧 pass 被 supersede），resume 点回退到该步，走它的 `onFail`。

`verdict.record` 回写：重判发生时旧 DeviceRail session 早已结束，新 verdict 经当前活跃 session 的 `ProviderSession.recordVerdict` 回写存证；daemon 只校验持久化、不运行断言（骨架 §4.2），跨 session 回写不构成语义问题。

**preflight-only 的 `judgeDirty`（边界补全，规则钉死于 02 §12.3 裁决 6）**：judgeHash 域含 `preflight`（02 §12.3），但离线重判的对象只有 assertions/observe——对 `call`/`human`/`if`/`foreach`/`let` 步（judgeHash 域**只有** preflight）重判无对象；对 action/assert 步，新探针也没有对应历史时点的「入场观测」存档可供求值。处置：对齐器对每个 `judgeDirty` 先做**子域比对**（新旧 IR 均按 irHash 存档可得）——judgeHash 变化仅由 `preflight` 子域引起（其余 judge 域逐字相等）→ 该步按 `reusable` 采认（verdict/output/evidence 全复用，不重判；human 步不重问——旧回答绑定的问题域在 effectHash，未变），`AlignmentReport` 条目的 `reason` 标注 `preflightChanged`；新 preflight 仅在该步成为 resume 首个待执行步时才实际生效（probing 只发生在那里，§4.2）。preflight 与 assertions/observe 同时变化 → 走上文常规离线重判，重判只针对 assertions/observe 子域，preflight 部分仍不影响历史效力。

### 5.4 mutating 步重执行的统一门槛（`requiresConfirmation` 通道）

典型场景：作者修改一个**已经 pass 的 mutating 步**（例如改 `submitLogin` 的目标元素）——逻辑上唯一诚实的语义是旧记录失效、该步起重新执行，但世界已经承载了旧动作的效果，重新执行是「在被改过的世界上再来一次」，可能双重提交。这个门槛**不只守被修改的那个步**：resume 点起的一切重执行，凡涉及非幂等 mutating 动作的二次执行，都必须过同一道 fail-closed 关卡。

**副作用判据（统一，2026-07-28 裁决：以「有无生效 attempt」为准）**：有 StepRecord 的步，`effect: mutating`、未声明 `idempotent: true`、且记录中存在**生效 attempt**（`succeeded`/`timedOut` 终局——效果已发生，或超时无法排除已发生）；call step 视同 mutating 当且仅当其 callee 传递闭包内含 `effect: mutating` 步。判据不读 verdict：verdict 判定的是动作之后的世界（断言层），动作有没有落地是 attempt 终局的证词——**priorVerdict=fail 但存在 succeeded attempt 的步照设门槛**（动作生效了、只是断言否决了它，重执行仍是二次效果）；反之 priorVerdict=unknown 而全部 attempt 皆 `failed`/`cancelled` 的步（如 `session_degraded` 终局折出的 unknown）不设门槛——终局证明动作未生效。无生效 attempt 的步一律**不设门槛**——旧动作未生效，重新执行正是修复的目的。

**收集规则（对齐阶段执行，钉死）**——满足副作用判据的以下四类步，全部进入 `alignmentReport.requiresConfirmation`，逐一带 `cause`（封闭枚举 `mutatingReexec | positionalReplay | orderInvalidated | frontierUnknown`，M1 收编，骨架 A.4）：

| `cause` | 来源 |
|---|---|
| `mutatingReexec`（M1 收编：原记 `effectDirty`，封闭集定名改此，以免与 `AlignmentClass` 值撞名） | 作者修改的步本身（含 inputs 变化导致整帧失效的 call step，§5.2） |
| `positionalReplay` | 未被修改、双哈希未变、逐步分类本为 `reusable`，但位置在 resume 点之后将被作废重执行的步（§5.2 位置失效条款；I2 (ii)） |
| `orderInvalidated` | 因 §5.2 顺序一致性校验自逆序点起降级失效的步 |
| `frontierUnknown` | frontier 步存在 `succeeded`/`timedOut` 的 mutating attempt 而无 verdict（§4.1 交叉语义） |

**处置流程（四类一视同仁）**：

1. 对齐把命中步列入 `alignmentReport.requiresConfirmation`；
2. `pointlock resume` 默认**拒绝**并打印报告；作者复核后以 `--allow-mutating-reexec <stepId>`（可多次给出，逐步显式点名，不接受通配）放行——一次 resume 的确认不泛化到下次；
3. 放行后该步作为全新执行进入 `probing`：它的 `preflight` 会撞见旧效果残留的世界，`drifted` → `onResumeDrift` → repair/人裁决；无 `preflight` 声明则只能标 `unprobed` 放行（§4.2——flow 级默认探针不存在）。**本条 preflight 守护对 `positionalReplay`/`orderInvalidated`/`frontierUnknown` 的步同样强制适用**，不因「步没被修改」而跳过。强烈建议：任何可能被修复重跑或位置重放的 mutating 步都写 `preflight`；
4. 若作者判断旧效果必须先撤销，正确工具是 `onResumeDrift` 挂 `{kind:"repair"}` 修复 subflow（把世界修回该步的前置锚点），而不是指望 runner 有补偿事务——它没有，也不假装有（I3）。

### 5.5 端到端示例

初始 run `run-7f21`：`checkout@a1f3c9d2` 在 `purchase/call→login@9c2e77b0/submitLogin#2!welcomeVisible` 处 fail（登录按钮选择器过时，两次 attempt 均 `target_stale` 后断言否决）。此时 `loadCart`、`eachItem[0..2]`（三次迭代全 pass）已完成。

修复：`login.flow.yaml` 中 `submitLogin` 的 `tap.element` 从 `{ name: "登录" }` 改为 `{ identifier: "loginSubmit" }`（不改 id）。重编译：`login` 新 irHash `sha256:d41f…`，`checkout` 新 irHash `sha256:b02e…`。

```
$ pointlock resume run-7f21 --ir dist/checkout.flowir.json
alignmentReport (old a1f3c9d2 → new b02e44aa):
  loadCart                                   reusable
  eachItem[0]/addToCart                      reusable
  eachItem[1]/addToCart                      reusable
  eachItem[2]/addToCart                      reusable
  purchase                                   effectDirty (callee irHash 9c2e77b0 → d41f09c3) → 下钻（帧未完成）
    ├─ focusAccount                          reusable
    ├─ enterAccount                          reusable
    ├─ enterPassword                         reusable
    ├─ submitLogin                           effectDirty (binding.attempts 变)
    └─ readWelcome                           new-relative（在旧 run 中未执行）
  resumePoint: checkout@b02e44aa/purchase/call→login@d41f09c3/submitLogin
  requiresConfirmation: (无 —— submitLogin 旧 verdict 为 fail 且无 succeeded attempt，直接放行；
                         purchase 的 inputs 未变、变更全来自 callee irHash，帧未完成 → 合法下钻；
                         resume 点之后无已完成 mutating 步记录 → 无 positionalReplay 项)
runResumed → probing(submitLogin.preflight) → acting(#3, 新 callId) → … → judged{pass}
```

设备上实际发生的只有 `submitLogin` 起的动作：三次 `addToCart`、账号密码输入全部复用旧记录。若这次修的不是选择器而是 `welcomeVisible` 断言的文本匹配（`mode: "exact"` → `"contains"`），则 `submitLogin` 只是 `judgeDirty`：对旧 run 存档的 after Observation 离线重判——但注意本例旧 verdict 是 fail 且根因在 act 阶段，重判多半仍 fail，resume 点仍回到该步；`judgeDirty` 真正的高价值场景是**已 pass 历史在断言收紧后的免设备复核**（改了第 3 步的断言，第 4~20 步的设备执行一秒都不用重来）。

---

## 6. 为什么不是 LangGraph checkpoint / Prefect state

两者都是优秀的先例，本节说明为什么直接采用（或模仿）它们都会在本场景失守，以及 Pointlock 设计为什么同时**更对**且**更轻**。对照基线见系列第 1 篇 §1.3。

| 维度 | LangGraph checkpointer | Prefect state / result persistence | Pointlock（本篇） |
|---|---|---|---|
| checkpoint 单位 | 每个 superstep 后的 channel values 快照（thread + checkpoint 序列） | task run 状态机 + 可选 result 持久化 | RunLog 事件溯源；step 边界物化 + **act 前 WAL 意图点** |
| 节点/任务中途崩溃 | 节点整体重执行；外部副作用是否重复由节点代码自理 | task 重跑；重试语义以幂等为前提假设 | `actionIntent{callId}` 先落盘，重启凭 `callId` 向 DeviceRail 事件日志 `reconcile`，四分终态判明后采认或重放（I2） |
| 代码修改后的旧 checkpoint | 快照不含图代码身份；改图后旧 thread 的语义漂移由用户自担 | cache key 可自定义，但默认状态与代码版本无关联 | `irHash` + 每步 `effectHash/judgeHash` 双哈希，五类对齐（`AlignmentClass`）机械判定复用边界 |
| 「改断言不重跑」 | 无此概念（判定逻辑就是节点代码，重判 = 重跑） | 无（验证逻辑内嵌 task，重验 = 重跑） | `judgeDirty` 离线重判：断言是纯函数、Evidence 已本地化，重判零设备 I/O |
| 成功的含义 | 图跑到 END | task Completed（执行状态） | 执行状态与语义 verdict 硬分离；三值 + `degraded` + evidence 引用 |
| 世界漂移检测 | 无（状态 = 进程内 channel values，无「外部世界」概念） | 无 | resume 强制 `preflight` 探针 + `onResumeDrift`（I3） |
| 基础设施 | 需自配 persistence backend；状态语义随图代码走 | server/cloud 编排面 + 数据库 | 单进程 + 单 SQLite 文件 + 证据目录；零常驻服务 |

三个结构性理由：

1. **状态的本体不同**。LangGraph/Prefect 的 checkpoint 保存的是**进程内状态**（channel values、task results）——对它们，恢复 = 恢复内存。Pointlock 的关键状态在**设备世界里**，进程内快照只是索引：真正要恢复的是「哪些副作用已经发生」。所以 checkpoint 的核心不是变量快照（那只占 `CheckpointView` 一小角），而是 `pendingIntent + eventCursor`——指向 DeviceRail append-only 事件日志的核对凭据。DeviceRail 已经提供了 `device.execute` 的 caller-UUID 落账与终态 shield（协议白送的 effectively-once 素材，骨架 R2），Pointlock 只需做水位与 reconcile；换用通用框架反而用不上这份红利，还得在其 checkpoint 模型外自建同样的 WAL。
2. **重跑与重判必须分家**。两个框架里「验证」都内嵌在节点/任务代码中，代码变更后想重新验证历史只能重执行——对数据管道可以（幂等重算），对真实设备不行（`tap` 不可幂等，重放即污染）。Pointlock 把 assertion 抽成对存档 Observation 的纯函数，用 `judgeHash` 单独跟踪其身份，「改判定标准」从此不再牵连设备。这条能力在两个参照系里**没有对应物**，也无法靠包装它们获得——它要求执行语义从第一天起就把 observe 产物完整落盘并与判定解耦（原则 3 的存储学后果）。
3. **轻的方式不同**。「轻」不是少写代码，而是少运维组件。Prefect 的状态库 + 编排面、LangGraph 的 backend 选型，对单机跑设备流程的团队都是纯负担；Pointlock 的全部持久化是一个 SQLite 文件加一个目录（§3.3），`cp -r .pointlock/` 就是完整备份，正确性论证收敛在本篇三条不变式上（原则 10）。同时刻意保留了升级通道：RunLog 事件溯源 + 确定性折叠正是 Temporal 心智的单机化（系列第 1 篇 §1.3），若未来上量，runner 状态机语义可平移，checkpoint 格式不必推倒。

---

## 7. 边界与非目标（v0.1）

- **无补偿事务 / saga**：Pointlock 不撤销世界效果，修复靠 repair subflow 与人（§5.4）。
- **无并行 step**：顺序模型使「N 起重新执行」的对齐语义简单成立；并行化会把 resume 点从「一个位置」变成「一个前沿集合」，留给后续版本。
- **foreach 位置制对齐**：items 列表在修复后增删中间项会造成 index 错位、迭代记录整体失效；keyed foreach（`PathFrame.iteration.key`）是已预留的解法，v0.1 不实现。
- **checkpoint 不可跨设备迁移**：`CheckpointView.binding.deviceId` 是硬绑定，换设备 = 新 run（世界状态不可迁移）。
