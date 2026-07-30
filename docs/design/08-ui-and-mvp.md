# 08 · @pointlock/ui 信息架构与 MVP 里程碑

> 本文是 Pointlock 设计文档系列第 8 篇（文件编号 08），骨架见 `00-architecture-spine.md`；覆盖需求产出 12（@pointlock/ui 信息架构）与产出 13（MVP 里程碑与 v0.1 非目标）。凡与骨架 Canonical Vocabulary 冲突之处，以骨架为准；本文新增且骨架未收编的命名均显式标注「待骨架收编」并汇总于 §8。涉及 DeviceRail 与 `@devicerail/live-visualizer` 的名称、常量、限额均已对照源仓库逐字核实。

---

## 1. UI 定位与四条铁律

@pointlock/ui 是**本地单用户操作台**：一个只读投影 store 数据的 SPA，外加一组等价于 CLI 命令的本地操作按钮。它不是 SaaS、不是协作平台、不是图形化编排器。四条铁律，全部由骨架既有契约推出：

1. **UI 是 RunLog/store 的只读投影，不做任何判定。** 页面上出现的每一个 `pass` / `fail` / `unknown`、每一个 `StepState`、每一条 `alignmentReport`，都直接来自 `pointlock-store` 中 runner 落盘的事实。UI 不重算断言、不折叠 verdict、不推断状态——否则就出现了第二个 runner（违反原则 2/3 的精神：判定权唯一）。本铁律的类型化落地即**投影协议**（骨架 00 §10，R14）：UI 与 store 之间的唯一契约是五族渲染器无关的只读投影 DTO + 传输约定（见 §3.0），UI 不触碰 store 内部、不读 RunLog 原始事件。
2. **UI 不碰 DeviceRail daemon。** 一切设备交互经 runner 走 Provider SPI。UI 需要的证据字节来自本地内容寻址 Evidence 库（骨架 §6.6：Evidence 在 `observing` 阶段已本地化），无需也不得直连 daemon——`ui.snapshot.get` 仅本 Session 活跃期可读，事后审计只有本地库这一条路，UI 与审计走同一条路。
3. **UI 的写操作 v0.1 只有一类，且等价于既有 CLI 命令：** 以子进程方式触发 `pointlock compile` / `pointlock resume`（修复入口，见 §2.7）。human 回应在 v0.1 **不经 UI**——06 篇 §4.2 已钉死 `webUi` 通道「不交付，接口占位」（响应端 authn/authz 是 collect 能力的前置依赖，v0.2 与远程部署一起做），v0.1 收件箱是只读呈现，回应走 `pointlock-human-cli`（见 §2.6）。UI 没有任何 CLI 做不到的能力——UI 挂了，命令行流程完整可用。
4. **本地 loopback，单用户。** UI host 只绑定 `127.0.0.1` 的临时端口，启动时打印带随机 token 的 URL（对齐 DeviceRail live visualizer host「endpoint 是临时 capability」的立场）。不做鉴权体系、不做多用户——那是 §7 非目标。

**部署形态与技术栈**：`@pointlock/ui`（R12 已正式收编进骨架 A.1，位于混合 monorepo 的 `packages/` 一侧——本文原「包名待骨架收编」开放问题关闭，见 §8）= Vite 构建的**纯静态 React SPA，运行时没有 Node host**。

> **已裁决（2026-07-18，M3a-W4 实现反哺）**：本段原文「React SPA + 一个薄 Node host」与 R12「`pointlock` CLI 交付 Rust 单一静态二进制」相矛盾（第二个运行时进程 + Node 运行时依赖破坏分发面主张）。裁决：**投影 host 即 Rust CLI 本体**——`pointlock inspect --serve` 内建 loopback HTTP host（tiny_http，原则 10 的轻量约束下选小型同步实现），以只读投影查询打开 store 的 SQLite（WAL 模式天然支持 runner 写、UI 读并发），提供 JSON API（返回形状即投影协议五族 DTO，§3.0/§5）、SSE `{revision}` 通道与 Evidence 字节路由，并经 `--ui <dist>` 托管 SPA 构建产物（SPA 壳是公开代码、无 token 直达；数据面 `/api` 与 `/evidence` 全部 token 门控）。修复闭环的「host 子进程执行 pointlock …」（§2.7）落地为**同一二进制自执行**。Node ≥ 22 自此只是 `packages/` 一侧的**构建期**约束，运行时零 Node 依赖。

`@pointlock/ui` 依赖 `@pointlock/projection-types`（type-only，R14——由 `pointlock-store` projection 模块生成，00 §10.2）消费投影 DTO 类型。React Flow（`@xyflow/react`）渲染流程图——经 `FlowGraphView` → React Flow 类型的内部 adapter（§3）。启动方式：`pointlock inspect --serve`（在既有 7 命令内加 flag，不新增命令；R14 已将该 flag 收编为投影协议 HTTP+JSON 规范形入口——00 §10.4，原「是否收编为第 8 条命令 `pointlock ui`」开放问题关闭，见 §8）。R12 后的分工口径：Node ≥ 22 约束只保留在 `packages/` 一侧（`@pointlock/ui`、`@pointlock/nl-drafter` 及 `@pointlock/ir-types`、`@pointlock/projection-types` 消费方——R14）；`pointlock` CLI 本体交付为 Rust 单一静态二进制——分发面是选 Rust 的首要理由，性能不是（runner 是 I/O 密集型，语言速度非决策依据）。

---

## 2. 页面清单与信息架构

### 2.1 路由总表

| 路由 | 页面 | 数据源 |
|---|---|---|
| `/flows` | Flow 列表 | 编译产物目录（FlowIR 文件）+ store 的 run 聚合 |
| `/flows/:flowId` | Flow 详情：流程图（§3）+ 运行选择器 | `FlowGraphView`（按 `irHash` 从 FlowIR 投影，00 §10.1）+ store run 索引 |
| `/runs/:runId` | Run 详情：图状态叠加 + step timeline + step 检查器 | `RunOverview`（含 per-step 状态摘要 `steps`，状态叠加数据源——§5 已裁决项）+ `RunTimelineEntry` 分页 + `StepDossierView` |
| `/runs/:runId/steps/:runPath` | Step 检查器直达（可分享的深链） | `StepDossierView`（= `pointlock locate` JSON 形状，00 §10.1） |
| `/inbox` | Human 任务收件箱 | store 中未配对的 `humanRequested` |
| `/evidence/:sha256` | Evidence 字节路由（非页面，供 `<img>`/下载用） | 本地内容寻址 Evidence 库 |

路由参数纪律：`:runPath` 使用骨架 §9 的规范串（URL-encode），服务端用结构化 `RunPath` 解析——与 `pointlock locate <path>` 完全同一个入口函数，UI 深链就是 locate 的超链接形态。

### 2.2 Flow 列表（`/flows`）

每行一个 `flowId`，列：最新 `irHash`（短前缀）、`lockfileDigest` 状态（当前 lockfile 是否仍与 IR 内嵌 digest 一致——不一致标黄，提示需重 lock 或重编）、最近 N 次 run 的 verdict 色条（pass/fail/unknown/进行中）、最近一次 run 时间。点击进详情。无搜索、无标签、无收藏——v0.1 一个团队的 flow 数量在两位数以内，列表就够。

### 2.3 Flow 详情（`/flows/:flowId`）

- **主体**：流程图（§3），渲染选中版本的 `FlowGraphView`（由该 `irHash` 的 `FlowIR` 投影，00 §10.1）。
- **版本选择器**：该 flowId 的历史 `irHash` 列表（编译产物目录扫描），默认最新。切版本重画图。
- **运行选择器**：右侧栏列出该 flow（该 irHash 及历史 irHash）的所有 run：runId、开始时间、终态（`runFinished` 的 flow verdict / `suspended` / `aborted`）、设备（`binding.deviceId`）。选中一个 run → 跳转 `/runs/:runId`，图切换为状态叠加模式。
- **操作**：「新建 run」按钮 v0.1 不做（启动 run 需要设备参数与 params，留给 CLI；UI 只观测与修复）。这是刻意的减面：UI 发起 run 意味着 UI host 要管理 runner 子进程的完整生命周期与参数表单，收益不抵复杂度。

### 2.4 Run 详情（`/runs/:runId`）——三栏布局

```
┌────────────────────────┬──────────────────────┬─────────────────────┐
│  React Flow 图          │  Step timeline        │  Step 检查器          │
│  （状态叠加，§3.6）      │  （§4，五类过滤+分页）  │  （§2.5，选中即显）    │
│                        │                      │                     │
│  顶栏：runId · irHash   │                      │                     │
│  · deviceId ·          │                      │                     │
│  sessionLineage ·      │                      │                     │
│  flow verdict · 状态    │                      │                     │
└────────────────────────┴──────────────────────┴─────────────────────┘
```

三栏联动是这个页面的全部交互模型：
- 点图上节点 → timeline 滚动到该 step 的首个事件并过滤高亮，检查器载入该 step 的卷宗（`StepDossierView`，§2.5）。
- 点 timeline 条目 → 图上对应节点高亮，检查器跳到对应区块（如点 `assertionEvaluated` 条目 → 检查器展开对应 `assertId`）。
- run 进行中时，图与 timeline 经 §5 通道实时更新；`suspended` / `awaitingHuman` 的 run 顶栏出现状态横幅（`awaitingHuman` 横幅直达收件箱条目）。

顶栏还展示：`alignmentReport` 摘要（若本 run 由 `pointlock resume` 产生——各 `AlignmentClass` 计数：reusable / judgeDirty / effectDirty / new / orphaned，点开看逐步明细）；`sessionLineage`（断代重开次数）。

### 2.5 Step 检查器

选中 step 后按 `StepRecord`（骨架 §6.6）的结构分块呈现（交付形状为投影 DTO `StepDossierView`，见本节末），块顺序即执行流水线顺序：

| 区块 | 内容 | 来源字段 |
|---|---|---|
| 身份 | stepId、RunPath 规范串（可复制，即 `pointlock locate` 参数）、kind、effect、`effectHash`/`judgeHash` 短前缀 | `StepRecord` 头部 |
| 输入 | `resolvedInputs` 快照（JSON 树视图，有界呈现见 §4.5） | `resolvedInputs` |
| Preflight | 各探针的 pass/fail/unknown 与理由（resume 首步会有） | `preflightProbed` 事件 |
| Attempts | 每次 attempt 一行：n、channel、actionName、callId、outcome（四分不折叠）、errorClass?、`execution.mode`（`coordinateFallback` 高亮 + `fallbackReason`）、耗时 | `attempts[]`（`AttemptRecord`） |
| 输出 | `output`（JSON 树视图） | `output` |
| Observations | 每条 Observation 的 capturedAtMs、viewport、omission 原因（`uiSnapshotOmission` / `screenshotOmission` 以类型化原因展示，明确标「合法缺料 → 断言 unknown 的来源」） | `observations[]` |
| 断言结果 | 每条 `assertId`：result（pass/fail/unknown）、实际使用的 channel（非首选 → 标 degradedVerify）、reason 文本 | `assertionOutcomes[]` |
| Verdict | status + degraded 标记 + `supersedes` 链（重判历史全展示，旧 verdict 灰显不删——账本语义的 UI 化）；无 assertion 的 mutating step 显示 `unverified` 注记（明确文案：「已执行、未验证——这不是 verdict」） | `verdict` |
| 错误 | ErrorClass、`ErrorInfo{code, message, retryable}`、触发的 handler（hook、disposition、第几次 trigger） | attempts + `handlerTriggered` |
| Evidence 画廊 | 缩略图网格：每项显示 mediaType 图标/缩略图、sha256 短前缀、来源（哪次 attempt/observation）；点开大图/下载，字节经 `/evidence/:sha256` | `evidence[]`（`EvidenceRef`） |
| 修复 | 「在 YAML 中打开」+「重编译并对齐预览」+「从 checkpoint 继续」（§2.7） | sourceMap + CLI |

设计要点：**检查器就是 `pointlock locate` 的图形化**——骨架 §9 承诺 locate 交付「可判案卷」（IR 节点 + YAML span + 全部 attempts/observations/evidence），检查器与 locate 共享同一查询层，保证 CLI 与 UI 看到的案卷逐字节一致。R14 将此裁决类型化：`pointlock locate` 的 JSON 输出形状**即**投影 DTO `StepDossierView`（00 §10.1），检查器消费的正是这份 DTO——CLI 与一切渲染器同一查询层、同一形状。

### 2.6 Human 任务收件箱（`/inbox`）——v0.1 只读呈现

跨所有 run 聚合 store 中已写 `humanRequested`、尚无配对 `humanResponded` 的条目。条目类型即投影 DTO `HumanInboxEntry`（含 `purpose` 判别子，R13/R14——00 §10.1）：human step 与 supervision 请求的统一收件箱条目，一切渲染器经投影协议消费。每条：run/flow 上下文、RunPath、`mode`（confirm / judge / provideInput / repairWorld）、prompt、剩余超时（超时后固定产出 unknown——倒计时旁边就写明这一点，给回应者压力与预期）、`presents` 的呈现（值直接渲染，EvidenceRef 走画廊组件）。

**监督条目同箱（R13，M2 起）**：监督模式（骨架 §6.9）的 `humanRequested(purpose="supervision")` 进**同一收件箱**——复用同一 store 单写者仲裁、同一通知通道、同一收件箱，不新建第二套管道。此类条目以 `purpose` 判别子（`step | supervision`）区分呈现：presents 为 runPath / actionName / resolvedInputs 摘要，decision 封闭枚举为 `proceed | abort | suspend`（v0.1 刻意无 `skip`），**无超时倒计时**（默认无超时，可随时 suspend）、不产生 verdict；v0.1 回应同样经 `pointlock-human-cli`。

**v0.1 边界（与 06 篇 §4.2 一致，本文不推翻该裁决）**：收件箱只承担 `notify` 侧的呈现，**不收集回应**。06 篇把 `HumanChannel` 的 `webUi` 通道钉为「不交付，接口占位」——collect 能力的前置依赖是响应端 authn/authz，随 v0.2 远程部署一起做。v0.1 的回应路径：每个条目展示一条可复制的回应指引（`pointlock resume <runId>`——分离模式下 resume 即进入交互回应流程，06 §4.2），人切到终端经 `pointlock-human-cli` 完成回应；store 写入 `humanResponded` 后，收件箱经 §5 通道实时消项。

**v0.2 交付形态（预写规格，随 `webUi` 通道 collect 能力按 06 §4.2 启用，此前不实现）**：回应表单按 mode 分型——`confirm` / `judge` 用 `decisions` 枚举按钮；`provideInput` 用按 `outputSchema` 生成的 JSON 表单；`repairWorld` 无输入，按 06 §2.1 封闭词表出两钮——`done`（我已修复，重探；后续由 runner 的 `onResumeDrift` 流程重探）与 `cannotRepair`（修不了，本步 verdict fail，06 §2.2）；声明了 `decisions` 的请求（reconcile 裁决 `adopt|redo|abort`，07 §4.4）按声明出钮。提交经 store 的 `submitHumanResponse` 仲裁（first response wins、deadline、schema 校验——06 §4.3），写入的 `humanResponded{requestId, payload, actor, at}` **与 `pointlock-human-cli` 同一张 RunLog、同一事件类型、同一配对协议**；`actor.channel` 取值为 06 篇 `HumanChannel.id` 封闭枚举中的 `"webUi"`——不发明「UI 会话标识」这类第四种取值。runner 侧无感知差异。

### 2.7 修复入口——「失败 → 改 YAML → 重编译 → 续跑」闭环

修复是 Pointlock 的核心叙事（双哈希对齐为此而生），UI 的职责是把骨架 §6.7 的机制串成一条不需要记命令的路径：

```
step 检查器（fail/unknown 的 step）
  │ ①「在 YAML 中打开」
  ▼
sourceMap 反查：RunPath → IR path → YAML 文件 + span（含 macro 展开链 origin trace，
逐层显示「此步由宏 X 在 Y 行展开产生」）→ 以编辑器深链打开（vscode://file/<path>:<line>:<col>，
可配置模板；UI 不内嵌编辑器——见 §7 非目标）
  │ ② 用户在自己的编辑器里改 YAML，回到 UI 点「重编译并对齐预览」
  ▼
UI host 子进程执行 pointlock compile（同一 lockfile）
  ├─ 编译失败 → 原样展示编译错误（含 YAML span），回到 ②
  └─ 编译成功 → 新旧 IR 按 stepId 做对齐预演（复用 runner 的对齐算法，只读不写）：
     逐步展示 AlignmentClass —— reusable（绿）/ judgeDirty（蓝：「将离线重判，
     不重跑设备」）/ effectDirty（橙：「该步及数据依赖下游失效，将从此步重跑」）
     / new / orphaned；并给出计算出的 resume 点
  │ ③ 用户确认「从 checkpoint 继续」
  ▼
UI host 子进程执行 pointlock resume --run <runId> --ir <新irHash>
  → 产生新的 runResumed（携带正式 alignmentReport）→ run 详情页实时跟进
```

两条纪律：**对齐预演是只读的**——真正的 `alignmentReport` 只在 `pointlock resume` 执行时产生并入账，预演结果不落盘、不承诺（世界可能在预演与 resume 之间又变了，resume 时的 preflight 探针才是最终裁决）；**UI 不代用户改 YAML**——修复的编辑动作永远发生在用户的编辑器里，UI 只负责定位与后续机械步骤。

---

## 3. 图模型与 React Flow 适配：FlowGraphView（协议）→ adapter → React Flow（渲染器）

### 3.0 投影协议：五族 DTO 与两层结构（R14，00 §10）

R14 起，UI 与 runner/store 之间的**唯一契约**是投影协议（Projection Protocol，骨架 00 §10）：一组**渲染器无关**的只读投影 DTO + 传输约定，铁律 1 的类型化落地。任何渲染器（React Flow web UI、未来的其他 flow 库、TUI、native）都只消费这份协议，不触碰 store 内部、不读 RunLog 原始事件。

- **DTO 五族（封闭清单，00 §10.1 / A.3）**：`FlowGraphView`（图模型，本节）· `RunTimelineEntry`（timeline 条目，§4）· `StepDossierView`（step 卷宗，§2.5）· `HumanInboxEntry`（收件箱条目，§2.6）· `RunOverview`（run 摘要 + `revision`，§5）。
- **版本化（00 §10.3）**：DTO 携带 `projectionVersion: 1`；演进 additive-only，breaking 需 bump；与 `irVersion` **相互独立**——投影是读侧契约，不影响 IR/checkpoint。
- **传输中立（00 §10.4）**：HTTP+JSON 为规范形（`pointlock inspect --serve` 提供）；SSE 只推 `{revision}` 失效通知，为可选推送、轮询完全等价；同进程渲染器（如未来 TUI）可直调 store 查询层——三种消费方式返回同一 DTO。
- **真相源与生成（00 §10.2）**：投影 DTO 的 Rust 定义在 `pointlock-store` 的 projection 模块（不新增 crate）；schemars 生成 JSON Schema + type-only TS 包 `@pointlock/projection-types`（管线与 `@pointlock/ir-types` 同款）；golden fixtures 覆盖五族。
- **两层结构（00 §10.5）**：图模型自此分两层——`FlowGraphView`（协议层，渲染器无关）→ `@pointlock/ui` 内部 **adapter**（FlowGraphView → React Flow 类型的映射层）→ React Flow（渲染器）。本篇原定义的 React Flow node/edge type 字面量从协议命名**降级为 adapter 实现细节**，不进任何 Canonical Vocabulary（§3.1、§8）。

### 3.1 FlowGraphView = FlowIR 的确定性投影；node/edge type 表 = adapter 参考映射

`FlowGraphView` 由 `FlowIR.body` 递归投影产生（store projection 模块，纯函数）：节点 kind 与 step kind 对齐，另有 call 折叠节点与 foreach 聚合节点；边为**顺序 / 分支 / hook 三类语义边**（封闭清单）；分组 = subflow 按 `flowRef.irHash` 懒加载；节点携带 `runPath` 锚点（供状态叠加与深链）。**DTO 不含坐标与布局、不含任何 React Flow 概念**——dagre/elk 自动布局（纵向主轴）是渲染器职责，无布局状态持久化（00 §10.1）。

下两表是 `@pointlock/ui` 内部 adapter 的**参考映射**（`FlowGraphView` → React Flow custom type）：其中 node/edge type 字面量是 adapter 实现细节，**不属协议命名、不进 Canonical Vocabulary**（R14 裁决，00 §10.5）；协议层的对接面是 `FlowGraphView` 本身。

**节点映射**（adapter 参考映射：React Flow custom node type ↔ FlowGraphView 节点 kind，与 StepIR kind 对齐）：

| node type | 对应 IR | 节点主体内容 |
|---|---|---|
| `stepAction` | `ActionStepIR` | stepId、verb（元数据）或 actionName、effect 徽标（mutating 实心/readonly 空心）、act-chain 通道片（§3.4）、断言计数徽标 |
| `stepAssert` | `AssertStepIR` | stepId、observe 来源（fresh / fromStep 引用箭头）、断言列表摘要、verify-chain 通道片 |
| `stepCall` | `CallStepIR` | stepId、callee `flowId@irHash 短前缀`、折叠/展开控制（§3.3）、inputs 键名列表 |
| `stepHuman` | `HumanStepIR` | §3.5 显著标识 |
| `stepIf` | `IfStepIR` | cond 表达式摘要；then/else 为两个 group 子域 |
| `stepForeach` | `ForeachStepIR` | items 表达式摘要 + `as` 名；body 为 group 子域 |
| `stepLet` | `LetStepIR` | bindings 键名列表（小型节点） |

**边映射**（adapter 参考映射；协议层 `FlowGraphView` 的语义边封闭清单为**顺序 / 分支 / hook 三类**，00 §10.1）：

| edge type | 语义 | 视觉 |
|---|---|---|
| `seq` | 兄弟 step 的文本顺序（v0.1 执行顺序） | 实线箭头 |
| `branch` | if → then/else 子域入口，带 `then`/`else` 标签 | 实线，菱形出点 |
| `hook` | step/flow → handler 徽标（onFail/onUnknown/onError/onResumeDrift） | 虚线，默认折叠为节点角标，点开展开 handler 徽标（含 disposition 与 maxTriggers） |

本文原有第四类边 `data`（数据依赖，编译期 `check` 阶段依赖图的可视化：`steps.<id>.output.*` 引用）**已裁决 v0.1 从 UI 削除**（2026-07-17，骨架 00 §10.1 已裁决项），不在上表 adapter 参考映射之列；登记为 **v0.2 additive 扩展候选**——届时以 `FlowGraphView` additive 新增边类交付，additive 新增边类不 bump `projectionVersion`（00 §10.3）。在此期间 adapter 禁止私读 store/IR 内部自行推导数据依赖（既有禁令保留，00 §10.1）。

Handler 刻意不做成图中的常规节点：骨架钉死 handler「不出现在正常控制流」（§2 概念 5），把它画进主图会制造「handler 是一个会被顺序执行的 step」的错觉。它是节点角标 + 展开徽标，触发史在 timeline 与检查器里看。

### 3.2 节点身份：静态 stepId，运行叠加按 RunPath 聚合

`FlowGraphView` 的节点 id = flow 作用域内的 `stepId`（含嵌套：if/foreach 子域内的 stepId 仍全 flow 唯一，编译器保证），节点携带 `runPath` 锚点（R14 起由 `FlowGraphView` 承载，00 §10.1——状态叠加与深链的挂接点）。运行叠加时，step 卷宗以 `RunPath` 为键，而一个静态节点可能对应多条 RunPath（foreach 的多次 iteration、handler retry 的多次进入、call 展开后的多实例）。规则：

- **聚合投影**：静态节点显示其全部 RunPath 实例的折叠状态——foreach 聚合节点（`FlowGraphView` 内建节点种类，00 §10.1）显示 `7/10 judged (6 pass · 1 fail)` 式计数，节点色取「最坏实例」（fail > unknown > 进行中 > pass，与 verdict 折叠同序）。
- **实例展开**：点节点后检查器顶部出现实例选择器（按 `PathFrame{kind:"iteration"}` 的 index/key 列出），选中某实例即看该 RunPath 的完整卷宗（`StepDossierView`）。图不为每个 iteration 画节点——foreach 上限不可知，图必须有界。

### 3.3 Subflow 折叠与展开

call 折叠节点（`FlowGraphView` 内建节点种类，00 §10.1）默认**折叠**：一个节点，显示 callee 身份（`flowId@irHash`）与聚合 verdict（骨架 §6.3：call step 的 verdict = callee 的 flow verdict）。点展开控制后：

- 渲染器按 `flowRef.irHash` 懒加载 callee 的 `FlowGraphView`（分组与懒加载语义由协议层承载，00 §10.1；引用不内联——骨架 `FlowIR.subflows` 的设计在投影侧原样复现），adapter 以 React Flow group（子图容器）就地展开，边界框标注 callee 身份与 input/output 契约。
- 展开是**懒加载 + 递归**：嵌套 call 逐层点开；irHash 对不上（产物缺失）则展开位显示「产物不可用」占位而非报错——图永远可画。
- 运行叠加下，展开的子图按 `PathFrame{kind:"call"}` 前缀过滤 StepRecord 投影进来；作用域封闭在视觉上成立：跨边界不画任何数据依赖连线（callee 只见 inputs——编译器本就不允许，图只是如实呈现；`data` 边本身 v0.1 已从 UI 削除，见 §3.1 已裁决项，本条对 v0.2 候选恢复后依然有效）。

### 3.4 Fallback 双链的呈现

原则 6/7 的图形化：**每个声明的降级都看得见，每次实际发生的降级都醒目**。

- **act-chain**：`stepAction` 节点内一行有序通道片（chip），按 `binding.attempts` 顺序：如 `dom → uiTree → coordinate`。只有一个 attempt 就一个片——没写 fallback 就没有链，图不虚构。运行叠加：实际成功的 attempt 片加实心边框，被跳过的前序 attempt 片打叉；若 `execution.mode === "coordinateFallback"`（daemon 内部降级），该片叠加警示角标并显示 `fallbackReason`（`semanticInteractionUnavailable` / `platformLimitation`），且当该 mode 不在 `acceptExecutionModes` 白名单内时，整节点按 degraded 样式渲染（§3.6）——「未授权的降级不能变成静默成功」在图上也成立。
  > **已收编（2026-07-18 收编评审）——运行叠加的协议交付形**：act-chain 运行标记经 **`RunOverview.steps` per-step 条目的 additive 可选字段 `actChainMarks`** 交付（与 §3.6 状态叠加同通道，00 §10.1 既有裁决的延伸；additive 不 bump `projectionVersion`），按 RunPath 实例键控——foreach/call 多实例下良定义，`FlowGraphView` 保持纯静态（投影源 = FlowIR，不触 run/store）。每条目一片（**Wave D 实现定形**）：`{ chainIndex, mark: succeeded|crossed, executionMode?, fallbackReason? }`——只为**已结算**的派发发条目，`untried` 以缺席表达（图不虚构）；`degraded` 不入 DTO——白名单判定需要 IR 的 `acceptExecutionModes`，由持有 IR 的渲染器据 `executionMode` 原样值判定（封闭 chip 词汇 untried/crossed/succeeded 仍入骨架 A.4，为渲染词汇）。标记只呈现**最近一轮 acting pass**（完整轮次史归卷宗）。数据源：`actionIntent` 新增 `chainIndex`/`channel`/`actionName` 可选字段 + 帐面 `handlerTriggered` 作轮界定界（chainIndex 单调性**不**充当轮界判据——单 attempt 链上不可判别；崩溃恢复 adopt/replay 属同轮续行非新轮）。旧账本无这些字段 → 不渲染运行标记，只渲染静态链（图不虚构，原则 4）。
- **verify-chain**：断言摘要行尾一组通道片，按 `verifyVia` 顺序（如 `uiTree → vision`）；vision 片恒为链尾且用独立样式（眼睛图标 + 虚线边框），图例明写「vision 只作降级验证」。运行叠加：实际完成求值的通道片实心；非首选通道完成 → 该断言行标 degradedVerify。

### 3.5 Human 节点的显著标识

`stepHuman` 是图上唯一的**非矩形节点**（圆角八边形 + 人形图标），配色独立于 verdict 色系（紫色系），确保「此处流程会停下来等人」一眼可辨。节点内容：mode 徽标（confirm/judge/provideInput/repairWorld 四值各有图标）、prompt 首行、`timeoutMs` 与固定的 `on_timeout: unknown` 注记。运行叠加：`awaitingHuman` 状态时节点呼吸动效 + 顶栏横幅 + 收件箱红点，三处同源联动。handler 的 `escalate` 升级出的 human 节点不在主图（它属于 hook 路径），在 handler 徽标展开与 timeline 中呈现。

### 3.6 状态与 verdict 的视觉映射（封闭表）

运行叠加 = 静态图之上按 StepRecord/frontier 给节点着色；其协议交付形状是 `RunOverview.steps` per-step 状态摘要（§5 已裁决项，00 §10.1）——UI 只消费该投影，不直读 StepRecord。两层信息分开编码：**状态用边框/动效，verdict 用填充色**。

| 来源 | 值 | 视觉 |
|---|---|---|
| `StepState`（进行中） | `pending` | 灰边框 |
| | `ready`/`probing`/`acting`/`settling`/`observing`/`asserting` | 蓝色边框 + 进行动效（当前 frontier 唯一） |
| | `awaitingHuman` | 紫色边框 + 呼吸动效 |
| | `suspended` | 蓝灰边框 + 暂停图标 |
| | `drifted` | 橙色边框 + 漂移图标 |
| | `skipped` | 半透明 + 斜纹 |
| | `blocked` | 半透明 + 锁图标 |
| | `aborted` | 深灰 + 终止图标 |
| Verdict（judged 后填充） | `pass` | 绿填充 |
| | `pass` 且 `degraded` | 绿填充 + 黄色斜纹条（strict 策略下不会出现——已折叠为 unknown） |
| | `fail` | 红填充 |
| | `unknown` | 琥珀填充（**独立颜色，绝不与 fail 共用**——unknown 不是失败，是「没看清」，配色必须维护这个语义） |
| 报告注记 | `unverified`（无断言 mutating step） | 灰绿空心填充 + 虚线边框，tooltip 明写「已执行、未验证」 |

---

## 4. Step timeline：与 DeviceRail live visualizer 约定对齐

Pointlock 的用户同时也是 DeviceRail 的用户（`@devicerail/live-visualizer` 已给了他们一套 timeline 心智模型：按 sequence 排序、五类过滤、evidence 仅引用、硬分页上限、有界呈现）。Pointlock timeline 呈现的是 RunLog 而非 DeviceRail Session 事件，但**交互约定逐条对齐**，不发明第二套习惯。timeline 条目的呈现 DTO 即 `RunTimelineEntry`——R14 已收编为投影 DTO 五族之一（00 §10.1 / A.3，原 §8 待收编项关闭）；本节的五类过滤、50/页上限、evidence 纯引用等既有裁决**全部不变**，以下即该 DTO 的内容与交互规格。

### 4.1 排序：单调 sequence

条目按 RunLog 的单调 `seq` 升序（append-only、SQLite 单调序——骨架 §6.1），不按时间戳排序（时钟不可靠，序号是账本真相）。resume 断代（`sessionLineage` 增长）在 timeline 中以分隔条呈现 `runSuspended → runResumed`，序号连续不重置——与 DeviceRail「事件游标跨 session 不假装连续」同一立场，RunLog 本身跨 resume 是一条账。

### 4.2 五类过滤：`TimelineFilter` 五值原样采用

过滤器取值逐字采用 `@devicerail/live-visualizer` 的 `TimelineFilter`：`all | observations | actions | errors | verdicts`。RunLog 事件（骨架封闭枚举 17 种）到过滤器的映射表（钉死，UI 与 API 共用）：

| RunLog 事件 | `observations` | `actions` | `errors` | `verdicts` | 备注 |
|---|---|---|---|---|---|
| `actionIntent` `actionSettled` | | ✓ | | | WAL 意图与终态成对呈现；`actionSettled` 显示四分 outcome 与 `execution.mode` |
| `actionSettled`（outcome ∈ failed/cancelled/timedOut） | | ✓ | ✓ | | 同时进 errors，附 ErrorClass 与 `ErrorInfo{code, message, retryable}` |
| `observationRecorded` | ✓ | | | | 含 omission 原因（合法缺料如实呈现） |
| `preflightProbed` `assertionEvaluated` `verdictRecorded` | | | | ✓ | 断言求值与终审同属判定类；`verdictRecorded` 展示 status/degraded/supersedes |
| `handlerTriggered`（hook = onError） | | | ✓ | | 其余 hook 的触发仅 `all` 可见 |
| `runStarted` `runFinished` `runSuspended` `runResumed` `stepEntered` `stepExited` `callFramePushed` `callFramePopped` `handlerTriggered`（非 onError） `humanRequested` `humanResponded` | | | | | **仅 `all`**——结构性事件不污染四个专项过滤器；对齐 visualizer「media 生命周期边界仅 all 可见」的同款处置。human 事件另有收件箱专页 |

### 4.3 Evidence 仅引用

timeline 条目中的 evidence **只携带引用，绝不内联字节**：`{ id, mediaType, sha256 }`——语义对齐 visualizer 的 `ReferenceOnlyEvidence{ availability: "referenceOnly", id, mediaType, sha256? }`。差异只有一处且是自觉的：visualizer 面向不可信浏览器，连 URI 都不给；@pointlock/ui 是本地可信操作台，检查器画廊可经 `/evidence/:sha256` 解引用字节。但 **timeline JSON 本身仍是纯引用**——分页响应体积因此有界，且引用与解引用分离让 API 天然可加导出/审计钩子。

### 4.4 分页与有界呈现

- **每页上限 50 条**，逐字对齐 `LIVE_TIMELINE_MAX_PAGE_SIZE = 50`；默认页大小 50，只可调小。
- 页是**同步快照**：携带 `revision`（§5），翻页期间新事件到来不移动已看到的条目，只推高 revision 提示「有更新」。
- 呈现 DTO（`RunTimelineEntry`）有界，限额直接采用 visualizer 的默认量级：文本字段截断于 4 KiB、JSON 摘要截断于 16 KiB / 深度 12、单条目 evidence 引用上限 32（超出显示 `+N omitted`）。截断永远显式标 `truncated`，完整原文在检查器（检查器读 `StepDossierView` 完整卷宗，不受 timeline 呈现限额约束）。fail-closed：超限事件不产出半截条目，标记占位并指向检查器。

---

## 5. 实时更新通道（v0.1）

采用与 DeviceRail live visualizer host 相同的模式：**SSE 只推失效通知（revision invalidation），数据永远靠拉**。这消灭了「推送内容与查询内容两套序列化」的一致性问题，也让轮询成为天然降级。R14 将本节通道收编为投影协议的**传输中立**约定（00 §10.4），三种消费方式返回同一 DTO：① **HTTP+JSON 规范形**（`pointlock inspect --serve` 提供，即本节 API）；② **SSE** 只推 `{revision}` 失效通知，可选推送、轮询完全等价（既有裁决不变）；③ **同进程直调** store 查询层（如未来 TUI，v0.2+，§7）。

- store 中每个 run 维护一个单调 `revision`（= 该 run RunLog 的最大 seq 即可，无需新状态）。R14 起 `revision` 收编进投影 DTO `RunOverview`（run 级摘要 + revision，00 §10.1 / A.3——原 §8 待收编项关闭）。
- `GET /api/runs/:runId/stream`（SSE）：仅推 `{ revision }`。客户端收到后按需重拉当前视图（当前 timeline 页 `RunTimelineEntry[]`、run 概览 `RunOverview`、选中 step 的 `StepDossierView`）。收件箱同理有全局流 `GET /api/inbox/stream`。
- 降级路径：SSE 不可用（或用户禁用）时，前端以 2s 间隔轮询 `GET /api/runs/:runId/revision`，行为完全等价。**v0.1 两者都属可接受实现，SSE 为默认**。
- UI host 与 runner 是两个进程，同一 SQLite（WAL 模式：runner 单写者、UI 多读者）；host 对 revision 的感知靠低成本轮询 store（100–500ms），SSE 只是把这次轮询的结果扇出给浏览器。不引入进程间订阅机制——单机文件即总线，够用（原则 10）。

**API 端点清单**（JSON，全部只读除注明外；只读端点返回形状对齐投影 DTO 五族，返回体一律携带 `projectionVersion: 1`——additive-only，breaking 需 bump，与 `irVersion` 独立，00 §10.3）：

| 端点 | 语义 |
|---|---|
| `GET /api/flows` · `GET /api/flows/:flowId` | 列表 / 详情（含 irHash 版本表与 `FlowGraphView` 图模型，00 §10.1） |
| `GET /api/runs?flowId=` · `GET /api/runs/:runId` | run 索引 / `RunOverview`（含 `revision` 与 per-step 状态摘要 `steps`——图状态叠加的数据源，形状见下方已裁决项） |
| `GET /api/runs/:runId/timeline?filter=&page=&pageSize=` | §4 timeline 分页（`RunTimelineEntry`；filter 默认 `all`，pageSize ≤ 50） |
| `GET /api/runs/:runId/steps/:runPath` | `StepDossierView` 案卷（= `pointlock locate` JSON 形状，同一查询层，00 §10.1/§10.2） |
| `GET /api/runs/:runId/revision` · `GET /api/runs/:runId/stream` | 轮询 / SSE |
| `GET /evidence/:sha256` | Evidence 字节（Content-Type = mediaType；只认内容寻址，不认路径） |
| `GET /api/inbox` · `GET /api/inbox/stream` | 收件箱（`HumanInboxEntry`，v0.1 只读呈现，§2.6）/ SSE |
| `POST /api/inbox/:requestId/respond` | **v0.2 预留端点，v0.1 不实现**（`webUi` collect 通道，06 §4.2）：届时经 `submitHumanResponse` 仲裁追加 `humanResponded` |
| `POST /api/repair/compile` · `POST /api/repair/align-preview` · `POST /api/repair/resume` | **写**：§2.7 修复闭环的三个子进程动作 |

> **已裁决（2026-07-17，原 R14 传播时新登记 openQuestion 关闭）**：run 详情页的图状态叠加（per-step `StepState` / verdict 折叠态）经 **`RunOverview` 的 additive 扩展**交付：新增 per-step 状态摘要字段 `steps: Record<RunPath 规范串, { state: StepState; verdictStatus?: "pass" | "fail" | "unknown"; degraded?: boolean }>`——只含图叠加所需最小集，卷宗细节（attempts/observations/evidence/assertionOutcomes）仍走 `StepDossierView`；additive 故不 bump `projectionVersion`（00 §10.1/§10.3 同步）。UI 不得 import store 内部类型、不得绕过投影协议的约束不变（00 §10 定位；M3a 验收对此有负向断言，§6.4）。

---

## 6. MVP 里程碑

里程碑纪律（四条通用规则）：
1. 每个里程碑以**一个可脚本化的端到端 demo** 收口——验收标准全部是可检验断言，不是「基本可用」；
2. 后一里程碑不返工前一里程碑的公共契约——IR schema、SPI、RunLog 事件枚举在 M0 就以骨架定稿形状落地（内容可以先窄，形状不许临时化）；
3. 每个里程碑的 out of scope 是承诺而非默认——发现必须提前借用后续里程碑内容时，走骨架评审而不是悄悄做；
4. `provider-kit` 一致性测试套件从 M0 起存在，每接入一个 provider 先过套件再过 demo。

**版本边界（先于一切里程碑声明）**：**M0–M2 与 M3a 构成 v0.1；M3b 是 v0.2 的首个里程碑**。§7 的「v0.1 非目标清单」约束 M0–M3a；M3b 不受其约束，但 M3b 触发的每一项宪法级变更——`irVersion` bump（02 篇 §11）、`FlowIR.provider.name` 由字面量 `"devicerail"` 放宽为 string 联合（03 篇 §5-10、05 篇 §7-2 既定时机）、骨架 A.1 crate/package 封闭清单扩编（R12/R14 后为 10 crate + 4 package；**2026-07-28 经骨架扩编裁决为 10 crate + 5 package**——`@pointlock/walk-drafter` 入册，见 00 A.1）——都是**显式前置工作，先走骨架评审再开工**，不得由 provider 接入静默触发。v0.1 全程 `FlowIR.provider.name` 保持字面量 `"devicerail"`（骨架 §2 概念 6、01 篇 §1.2）。

### 6.1 M0 · Walking Skeleton——「账本先于功能」

**目标一句话**：用 mock provider 端到端跑通一个 3 步 flow，证明「YAML → 五阶段编译 → FlowIR → runner 状态机 → RunLog/checkpoint 落盘 → verdict 折叠」这条脊柱是通的。

**In scope**
- `pointlock-ir` 全量类型（Rust DTO，serde + schemars，类型唯一真相源）+ 三哈希（irHash/effectHash/judgeHash）与规范化——**形状即骨架定稿形状**；代码生成管线（schemars → JSON Schema（Draft 2020-12）+ `@pointlock/ir-types` + golden fixtures）从 M0 打通（R12）；
- `pointlock-expr`：`Expr` 三形态 + 10 个 `PureFn` 全量（都很小，不值得分期）；作用域 `params.*` / `steps.<id>.output.*` / `env.*`；
- `pointlock-compiler` 五阶段骨架：`parse`（saphyr，带 span marks，fail-closed 上限）→ `normalize`（默认值填充；无 macro）→ `check`（引用消解、依赖图）→ `bind`（对 manifest.knownActions 校验；单 attempt，无 fallback 链）→ `seal`（哈希、sourceMap、binding report）；
- `pointlock-provider-kit`：SPI 全量签名（十个方法定稿，权威形态 Rust trait）+ `FakeProvider`（内存假设备：一块可断言的假屏幕，实现协议五件套同形 action，`reconcile` 返回 `neverDispatched`/`completed` 可注入）+ 一致性套件 v1；
- `pointlock-runner`：顺序执行 `action` / `assert` / `let` 三种 kind；完整 step 状态机（未用到的状态也定义）；**`actionIntent` WAL（先 fsync 后 dispatch）从 M0 就在**；verdict 三值折叠 + degraded + `unverified` 注记；
- `pointlock-store`：SQLite（rusqlite，WAL + `synchronous=FULL`——语义逐条不变）RunLog append-only + `CheckpointView` 物化；17 种 RunLog 事件枚举定稿（未触发的类型允许 M0 不产生）；
- `pointlock-cli`：`pointlock compile` + `pointlock run`（装配层注入 FakeProvider：`--provider fake`）。

**Out of scope**：DeviceRail、`pointlock lock`/lockfile（bind 退回 `manifest.knownActions`，这是骨架 §4.1 明文允许的路径）、resume、reconcile 实跑、retry、fallback 链、handler、human、`if`/`foreach`/`call`/macro、vision、evidence 字节（FakeProvider 产出零 evidence 或极小占位）、UI。

**验收（demo：`examples/m0-hello/`，3 步 flow）**

```yaml
flow: m0_hello
provider: devicerail          # 装配层以 --provider fake 注入 FakeProvider（同 manifest 形状）
steps:
  - id: waitInput
    wait_for: { element: { identifier: "note-input" }, state: present }
  - id: typeNote
    set_value: { element: { identifier: "note-input" }, value: "hello pointlock" }
    effect: mutating
  - id: checkNote
    expect:
      - element: { identifier: "note-input" }
        value: { text: { value: "hello pointlock", mode: exact } }
```

1. `pointlock compile` 产出 FlowIR：irHash 稳定（同输入重编译哈希逐字节相同）；每步 effectHash/judgeHash 存在；sourceMap 可把每步映射回 YAML 行；
2. `pointlock run --provider fake` 三步 judged，flow verdict `pass`；改 FakeProvider 假屏内容 → `checkNote` verdict `fail`（证明断言真的在判，不是恒真）；删掉 `checkNote` 的 expect → 该步 `unverified` 注记且**无 verdict**；
3. RunLog 中事件序列与骨架 §6.1/6.2 完全一致（`runStarted → stepEntered → actionIntent → actionSettled → observationRecorded → assertionEvaluated → verdictRecorded → stepExited → … → runFinished`），`actionIntent` 的 seq 严格先于对应 `actionSettled`；
4. 在 `typeNote` 的 dispatch 前后两个注入点 `kill -9` runner → 重启后 `CheckpointView` 可从 RunLog 完整重建，frontier 上有 `pendingIntent{callId}`（M0 只要求账本正确，续跑是 M1 的事）；
5. FakeProvider 通过一致性套件 v1；
6. **代码生成管线（R12）**：CI 由 `pointlock-ir` 的 Rust DTO（schemars）生成 JSON Schema（Draft 2020-12）、`@pointlock/ir-types`、golden fixtures，管线打通；生成 schema 与验收基线 `schema/flow-ir.v0.1.schema.json` **行为等价验收通过**——两 schema 对 golden fixture 语料（正例 + 全部反例组）的接受/拒绝判定**逐一一致**，文本 diff 仅供人读、不作判据（判据钉死见 02 §1.1，2026-07-17 裁决；此后基线随生成物滚动，`irVersion` 不 bump——IR 形状不变，仅真相源与实现语言变化）。R14 注：同款管线复用于 `@pointlock/projection-types`（真相源在 `pointlock-store` projection 模块，00 §10.2），该包为 **M3a 前置产物，M0 不要求交付**（00 §10.6）。

### 6.2 M1 · DeviceRail Provider——「真设备 + 全套账本兑现」

**目标一句话**：接上真 DeviceRail daemon，把 M0 账本里预埋的钩子全部兑现：capability 闭环（lock→bind→attestation）、evidence 本地化、失败定位、崩溃后 reconcile、双哈希 resume；CLI 七命令齐。

**前置依赖（R12，工期标注）**：`devicerail-client` Rust crate——在 DeviceRail 仓库实现、经 path/git 依赖消费（其 workspace 全部 `publish=false`），当前**待实现**，是 `pointlock-provider-devicerail` 的前置；**M1 估算 +1~2 周**。`@devicerail/client`（TS）与 python-client 定位为协议稳定性佐证与参考实现。

**In scope**
- `pointlock-provider-devicerail`（依赖 `devicerail-client`）：`openSession` = spawn/attach → `system.hello`（`requiredFeatures` 进 `FeatureOffer.required`）→ `devices.list` / `device.select` / `device.connect` → `session.start` → attestation 比对；`execute` = `device.execute`（callId 透传 params.id）；`observe` / `uiSnapshot`（`observation.uiSnapshot.v1`）/ `reconcile`（`session.current` + `events.list`，按 `actionStarted`/`actionCompleted` 的 `call.id` 匹配）/ `fetchEvidence`（sha256 校验入本地库）/ `recordVerdict`（`verdict.record`）/ `currentCursor` / `health` / `end`（四值 outcome）；
- `pointlock lock`：对真 daemon 固化 `CapabilityLockfile`（digest 进 IR）；bind 阶段能力校验全量打开（feature 缺失 = 编译错；protected action 拒绝）；
- 断言全三型结构化谓词（`elementState` / `elementText` / `expr`；`visual` 编译可过、运行期由 `pointlock-vision` stub 恒返 unknown——链尾降级语义先通）；verify-chain `dom`/`uiTree` + omission → unknown 传导；
- act-chain fallback（`locate_via` 多 attempt + `coordinate` 静态坐标）与 `acceptExecutionModes` / `execution.mode` 降级审计（R-degrade 全流程）；
- `RetryPolicy`（attempt 内重试，新 callId 新 WAL）+ ErrorClass 九值映射（客户端错误类 `TransportClosedError` 等 → `transport_lost` 等；TS 类名为 DeviceRail 生态事实，Rust 侧等价错误类型名称以 `devicerail-client` crate 落地为准——R12）+ `target_stale` 强制重 observe；
- Evidence 本地内容寻址库落地；
- `pointlock resume` 全语义：A 双哈希对齐（五类 `AlignmentClass`、judgeDirty 离线重判、effectDirty 下游失效回退）+ B 悬挂意图 `reconcile` 四分处置 + C preflight 世界探针（`drifted` 状态；handler 未到，默认处置 = 报告并停在 `drifted`，escalate 留 M2）+ session 断代 `sessionLineage`；
- CLI 补齐：`pointlock inspect`（run/step 文本视图）、`pointlock locate`（可判案卷）、`pointlock report`（run 报告，含 `unverified` 注记与 degraded 汇总）。

**Out of scope**：handler 家族（`on_fail` 等编译报「M2 起支持」而非静默忽略）、human、`if`/`foreach`/`call`/macro、NL、UI、vision 实判、`events.stream.open` 订阅（reconcile 用 `events.list` 拉取即可）、media stream。

**验收（demo：`examples/m1-login/`，真机或模拟器 login flow）**
1. 无 lockfile 编译含五件套动词的 flow → 编译错误提示先 `pointlock lock`；lock 后编译通过；篡改 lockfile 的 featuresEnabled → 运行期 attestation 报 `capability_drift` 拒跑；
2. 完整跑通 login flow：flow verdict `pass`；`verdict.record` 已回写（daemon 侧 `session.export` 可见 verdictRecorded）；每步 evidence 已本地化且 sha256 与 daemon `AssetRef` 一致；
3. 人为改坏 selector → 目标步 `fail`；`pointlock locate <runPath>` 一次调用返回 IR 节点 + YAML 行号 + 全部 attempts/observations/evidence；
4. **双哈希两幕戏**：只改断言文本 → resume 时该步 `judgeDirty`，离线重判产生新 verdict（`supersedes` 旧值），**设备零交互**（daemon events.list 无新 action）；改动作实参 → `effectDirty`，resume 点回退，该步及下游重跑；
5. **崩溃安全**：mutating step 的 dispatch 之后 `kill -9` → `pointlock resume` 经 `reconcile(callId)` 得 `fate: completed`，采认结果续跑——设备侧 `events.list` 里该 action 恰好一次（effectively-once 实证）；dispatch 之前 kill → `neverDispatched`，安全重放；
6. 断言目标元素跨导航引用 → 编译期自动注入 revalidate（`findElement` 重定位）**（2026-07-28 勘注：v0.1 以更强的 fail-closed 兑现——动态元素目标整体编译期 typed 拒绝，跨导航引用写不出来；revalidate 注入随 04 §9.5 动态目标落地）**；`session_degraded` 注入 → 当前步 unknown。

### 6.3 M2 · Human + Handler + Authoring 完善——「流程会等人、会自救、写得顺」

**目标一句话**：human 四模式成为 durable 正式节点，handler 家族让失败路径显式化，YAML 作者面补齐（macro/if/foreach/call/subflow + NL 起草），监督式协作三件套（supervised run、编译期问询、LLM 修复提议循环 CLI 形态，R13）落地，核心叙事「过夜的 run 第二天由人接续」跑通。

**In scope**
- `human` step 四 mode 全量：`awaitingHuman → suspended` 可退进程；`pointlock-human-cli` 呈现/回应通道（`humanRequested`/`humanResponded` 配对协议，payload 携带 `purpose` 判别子）；超时固定 unknown；judge 的判定即 verdict（记 actor/at）；
- **监督模式（R13，骨架 §6.9）**：`pointlock run` / `pointlock resume` 的 `--supervise <mutating|all>` 运行级策略——不进任何哈希域，逐段记入本段起始事件（`runStarted` / `runResumed`）payload 的 `supervisePolicy`（不跨段隐式继承，resume 传入即覆盖、未传即本段无监督，payload 显式记 `null`，骨架 §6.9）；门控点在 step 进入 `acting` 之前；WAL 顺序 `humanRequested(purpose="supervision")` 先 fsync → 通知 → `humanResponded(decision)` → `decision = proceed` 才写 `actionIntent` → dispatch；decision 封闭枚举 `proceed | abort | suspend`（v0.1 刻意无 `skip`）；不产生 verdict、默认无超时；复用同一 store 仲裁/通知通道/收件箱；
- handler 家族：四 hook × 五 disposition 全量、`maxTriggers`、step 级覆盖 flow 级、`errorClasses` 过滤、`hook:` 审计帧；`onResumeDrift` 的 `{kind:"repair"}` 修复 subflow（跑完重探）与 `escalate` 升级（06 篇的交互式安全验证 handler 家族在此落地）；
- authoring 完善：macro 卫生展开 + origin trace、`if`/`foreach`/`let` 全量、`call` subflow（irHash 锁定、call frame、聚合 verdict、`callFramePushed/Popped`）、`expect_schema` 收窄 `invoke` 输出；
- NL 编译（起草器实体定名 `@pointlock/nl-drafter`，TS，信任边界外）：自然语言 → **YAML 草稿**（生成物必须经同一条五阶段管线编译；NL 永不直产 IR、生成即普通 YAML 文件进版本库——原则 1 的延伸：NL 是 YAML 的输入法，不是第二界面）；草稿中无法确定的 selector/断言以显式 `TODO` 占位，编译 fail-closed 逼作者补齐；
- **编译期问询（elicitation，R13，NL 链路）**：起草器在四类情形必须发结构化提问——必填 param 缺失、目标选择器歧义、fallback 链授权（coordinate/vision 进链需作者点头）、secret 处理策略；问题为 JSON 结构（question / options / 目标 YAML path），答案织回后重起草，循环至编译通过。LLM 永远只产 YAML 草稿，编译器是唯一执法者；
- **LLM 修复提议循环（R13，CLI 形态）**：失败 → `pointlock locate --format json` 卷宗 → 起草器提议 YAML patch（diff 形态）→ 人审批门（呈现 diff + align-preview 的 `alignmentReport` 预览：哪些历史保留、哪些重跑、哪些需确认）→ 批准 → resume（UI 审批表单属 M3a）；
- `pointlock report` 补 human/handler 维度（谁在何时判了什么）。

**Out of scope**：UI（收件箱仍是 human-cli）、Playwright/HTTP/CLI provider、vision 实判、`secrets`/`protected`（v0.2 预留不动）。

**验收（demo：`examples/m2-checkout/`，带人工闸门的 checkout flow）**
1. flow 含 `human: { mode: confirm }` 闸门：跑到闸门 → `awaitingHuman` → 杀掉 runner 进程 → 次日 `pointlock inspect` 可见待办 → `human-cli` 回应 confirm → `pointlock resume` 续跑至 pass；全程账本连续（同一 runId，`humanRequested`/`humanResponded` 配对完整）；
2. 断言失败步挂 `on_fail: escalate` → 升级 human judge，人判 `pass`（附理由）→ verdict 记 actor，flow 继续；`maxTriggers` 耗尽路径落 `aborted`；
3. resume 时世界被人为弄脏（登出）→ preflight 探针 fail → `drifted` → `on_resume_drift: repair` 拉起 ensureLoggedIn subflow → 重探通过 → 续跑（骨架 §6.7-C 的教科书路径）；
4. macro 展开的步失败时，`pointlock locate` 报出宏调用链（origin trace）直至作者写的那一行；
5. foreach（3 项商品）逐项加购，`iter.<as>` 作用域与 per-iteration RunPath（`kind:"iteration"`）正确；call subflow 的输出经显式 outputs 契约回传；
6. NL demo：一句「登录后把第一件商品加入购物车，确认购物车数量为 1」→ 生成 YAML 草稿 → 人补齐 selector → 编译通过 → 跑 pass。生成的 YAML 与手写的走完全相同管线（diff 只在内容不在形状）；
7. **监督模式（R13）**：`pointlock run --supervise mutating` 跑 checkout flow → 每个 mutating step 进入 `acting` 前产生 `humanRequested(purpose="supervision")`（presents 含 runPath / actionName / resolvedInputs 摘要），且其 seq 严格先于对应 `actionIntent`、`decision = proceed` 之后才出现 `actionIntent`；readonly step 不问，`--supervise all` 全问；`decision = suspend` → 进程可退，重启后 supervision 请求仍 pending（惰性结算与 human step 同款）；`runStarted` payload 含 `supervisePolicy`，且同一 YAML 带与不带 `--supervise` 编译产物 `irHash` 逐字节相同（不进哈希域实证）；监督问答不产生任何 verdict 事件；
8. **编译期问询（R13）**：NL demo 输入刻意缺必填 param、给出歧义选择器 → 起草器发结构化 JSON 提问（question / options / 目标 YAML path），答案织回后重起草循环至编译通过；coordinate 进 fallback 链的草稿必经作者显式点头，未点头不出现在生成 YAML 中；
9. **LLM 修复提议循环（R13，CLI）**：人为改坏 selector 使目标步 `fail` → `pointlock locate --format json` 卷宗喂给起草器 → 产出 YAML patch（diff 形态）→ CLI 审批门呈现 diff + align-preview 的 `alignmentReport` 预览 → 批准后 resume 续跑至 pass；拒绝批准则不落任何 YAML 变更、run 状态不变。

### 6.4 M3a · @pointlock/ui（v0.1 收口）——「看得见」

**目标一句话**：@pointlock/ui 全量落地（本文 §2–§5，收件箱为只读呈现），vision 首个可用实现补上 verify-chain 链尾；v0.1 在此收口——自始至终单 provider（devicerail）。

**In scope**
- 骨架收编评审（前置）：原「`@pointlock/ui` 收编」议题已由 R12 提前关闭（骨架 A.1 已列 `packages/` 四包，R14 增 `@pointlock/projection-types`）；原「`pointlock inspect --serve` flag（或第 8 条命令 `pointlock ui`）」议题亦已由 R14 关闭——该 flag 收编为投影协议 HTTP+JSON 规范形入口，不新增命令（00 §10.4；§8 的相关待收编项已全部清账）。另一前置产物：`@pointlock/projection-types` 在本里程碑前就绪（00 §10.6，M0 只打通管线不要求交付）；
- `@pointlock/ui` 全部页面与通道：flow 列表/详情、run 详情三栏（React Flow 状态叠加 + timeline 五类过滤 + 检查器）、human 收件箱（**只读呈现 + human-cli 回应指引**，§2.6，含 supervision 条目）、修复闭环（YAML 深链 → 重编译 → 对齐预演 → resume）、SSE/轮询；
- **LLM 修复提议循环的 UI 审批表单（R13）**：UI 呈现起草器的 YAML patch diff + align-preview 的 `alignmentReport` 预览（哪些历史保留、哪些重跑、哪些需确认）；**批准动作不经 UI 收集**——经 `pointlock-human-cli` / CLI 等价通道完成，遵守 06 §4.2 与本文 §2.6 的既有裁决（v0.1 `webUi` 不收响应），与 §2.7 修复闭环共用对齐预演组件，不引入矛盾；
- `pointlock-vision` 首个可用实现（仅 verify-chain 链尾；不可用时依旧 unknown）。

**Out of scope**：§7 全部非目标；UI 内嵌编辑器；UI 发起新 run；移动端适配；UI 收集 human 回应（`webUi` collect，06 §4.2 钉死 v0.2）；一切新 provider 与 `provider.name` 放宽（M3b / v0.2）。

**验收（demo：一场 15 分钟的完整叙事，全程单 provider）**
1. `pointlock inspect --serve` 打开 UI；CLI 启动 M1 的 login flow run，UI 实时跟进（frontier 蓝色动效推进、timeline 五类过滤各自正确、SSE 断掉后轮询无缝接替）；
2. 目标步 fail → 图上红节点 → 检查器案卷完整（attempts/observations/断言 reason/evidence 画廊可看图）→「在 YAML 中打开」落在正确行 →改一行 → 「重编译并对齐预览」显示 judgeDirty（蓝）→「从 checkpoint 继续」→ 图转绿；修复闭环全程不碰终端；
3. M2 的 checkout flow 跑到 human 闸门 → UI 收件箱红点，条目呈现完整（prompt / presents / 倒计时 / 回应指引）→ 人按指引经 `human-cli` 回应 → store 写入 `humanResponded` 后收件箱实时消项，run 续跑，图与 timeline 同步推进；
4. subflow 折叠节点显示聚合 verdict，展开后内部失败步可直达检查器；foreach 节点聚合计数正确；
5. 含 `visual` 断言（verify-chain 链尾）的步：vision 可用时产出真实 pass/fail；人为置 vision 不可用 → 该断言 unknown（降级验证语义实证，原则 7）；
6. timeline 分页在 10k+ 事件的 run 上仍流畅（50/页硬上限 + 有界 DTO 起作用）；
7. **LLM 修复提议的 UI 呈现（R13）**：M2 验收 9 的同一循环在 UI 中走一遍——UI 完整呈现 YAML patch diff 与 `alignmentReport` 预览，但页面上**不存在批准提交控件**（`webUi` 不收响应的负向验收）；人经 `pointlock-human-cli` / CLI 等价通道批准后，UI 实时跟进 resume 与图状态推进；
8. **投影协议边界（R14）**：`@pointlock/ui` 与 store 之间**只经投影协议**——以协议 golden fixtures + 适配层单测验证，UI 不 import store 内部类型（00 §10.6）。

### 6.5 M3b · 多 Provider（v0.2 首里程碑）——「不止一个世界」

**目标一句话**：Playwright/HTTP/CLI 三个 provider 按 05 篇接入，「同一份 YAML 动词表流程跨 provider 可移植」的承诺兑现。这是 v0.2 的第一个里程碑，**开工前置是一次显式的 IR 语义世代升级**——不许被 provider 接入静默触发。

**In scope（前两项为硬性前置，先评审后开工）**
- **irVersion bump**：`FlowIR.provider.name` 由字面量 `"devicerail"` 放宽为 provider 名字符串联合（03 篇 §5-10、05 篇 §7-2 的既定时机）。按 02 篇 §11，closed vocabulary 的枚举扩展是语义世代变更：`irVersion` 1 → 2，runner 精确匹配、跨版本 resume 不承诺、哈希 domain tag 随之全变，全部按 02 篇既定语义执行，不打折扣；
- **骨架扩编评审**：A.1 收编三个扩展 provider——其**包名归属（Rust crate 还是 stdio JSON-RPC sidecar 包）为 v0.2 收编议题，本轮不定名（R12；00 §4 SPI 进程边界预留：sidecar 适配形态 v0.2，v0.1 不实现）**，连同 05 篇 §7 收编清单其余各项（lockfileDigest 代位约定、`manifest.protocol` 语义泛化、canonical verb 晋升提案、`invoke` 实参子键、feature id 四枚）一次性裁决；
- Playwright / HTTP / CLI provider 实现（各自 manifest + lockfile 语义，细节以 05 篇为准；R12 注：Rust 核心下直连 Playwright 需 Node sidecar，「经 DeviceRail web 设备」路径在 v0.x 权重上升——05 篇 §3）；多 provider 下 `pointlock lock` 的 per-provider lockfile。

**Out of scope**：`webUi` collect 通道与远程部署 authn（06 §4.2：v0.2 的另一里程碑，与远程部署一起做，届时按 §2.6 预写规格启用）；单 flow 混编多 provider（骨架 A.6-4 的 `"<provider>:<action>"` 预留形式仍不启用）；`secrets.*` / `protected`（另按骨架 R6 路径推进）。

**验收（demo）**
1. 三 provider 各自通过 `provider-kit` 一致性套件（里程碑纪律 4）；Playwright provider 另过 domSnapshot 离线重判回归（05 篇 §6）；
2. 移植性：一份只用 `wait_for`/`set_value`/`tap`/`expect` 的 YAML，分别以 devicerail（**android 设备**，语义五件套可用）与 playwright provider lock + 编译 + 运行，两边 verdict 均 pass。对照对象刻意**不是** devicerail 的 web 设备：05 篇 §3.1 已逐项核实该设备不提供 `device.semanticActions.v1`、无 dom/uiTree 观测通道，canonical verbs 与 `elementState`/`elementText` 谓词在其上**编译期即被 bind 拒绝**（原则 5），其 web 流程形态是 `invoke` + `expr` 断言（05 篇 §3.4），与「同一份 YAML」主张不同构；
3. HTTP provider 跑一条 API 冒烟 flow（`invoke` + `expr` 断言 + `expect_schema` 收窄）；CLI provider 跑一条构建冒烟 flow（05 篇 §5 示例形态）；
4. 三 provider 的 run 在 @pointlock/ui 中呈现无特殊分支——UI 只投影 store，provider 差异被 manifest/IR 吸收，M3a 交付物零改动。

---

## 7. v0.1 非目标清单（明确不做，且写明为什么）

下表是承诺性的「不做」，不是「没排上」。每项给出不做的理由与重新评估的触发条件。凡试图在 v0.1 引入下列任何一项，须走骨架评审。

| # | 非目标 | 为什么现在不做 | 重估触发条件 |
|---|---|---|---|
| 1 | 分布式执行 / 水平扩展 | 原则 10：单进程、本地存储是宪法。RunLog 单写者是崩溃安全论证（WAL+reconcile）的前提，分布式会推翻整个 §6 的证明 | 单机吞吐成为真实瓶颈（不是想象的） |
| 2 | Temporal / 队列级 durable execution | 原则 10 点名排除。Pointlock 的 checkpoint/resume 是为「设备世界的副作用」定制的（reconcile 借 daemon 事件日志），通用 workflow 引擎解决的是另一个问题且带来巨大运维面 | 出现跨天、跨百 step、多方协作的 run 成为常态 |
| 3 | 多租户 / 用户体系 / RBAC | 本地单用户工具。租户边界会污染 store schema 与 UI 每一层 | 团队共享 run 库的真实需求出现 |
| 4 | 调度器 / cron / CI 集成产品化 | `pointlock run` 有确定 exit code，任何 CI/cron 都能裹；做产品化调度是重复造轮子 | 无（裹 CLI 永远够用） |
| 5 | step 并行执行 | v1 语义 = 顺序 + 有限控制结构（骨架概念 1）。并行破坏 RunPath 唯一 frontier 假设与 checkpoint 粒度论证 | 出现大量可证明无依赖且耗时的只读 step |
| 6 | `secrets.*` / `protected` action | R6 裁决：v0.1 bind 阶段 fail-closed 拒绝；关键字已预留、禁挪用 | v0.2 既定路径，按骨架 R6 原样实施 |
| 7 | 远程 / 共享 store，evidence 对象存储 | Evidence 本地内容寻址是审计论证的一部分；远端化引入可用性与一致性新轴 | 与 #3 同时评估 |
| 8 | 一个 run 绑多台设备 / 设备农场编排 | 骨架概念 1 钉死：一次 run 恰好一台设备、一条活跃 session（lineage 仅因断代）。多设备是矩阵层（外部循环跑多个 run），不是 flow 语义 | 需要设备间交互的场景（如 A 发消息 B 收）真实出现 |
| 9 | Flow registry / 市场 / 版本管理服务 | irHash + git 就是版本管理。subflow 按 irHash 锁定已给出可复现联编 | 跨团队分发 flow 的需求出现 |
| 10 | 录制器（record & replay 生成 YAML） | 好录制器 = 好选择器推断 + 好断言推断，难度不亚于主线；坏录制器产出不可维护 YAML，毒害「YAML 是界面」 | M2 的 NL 起草被验证不足以降低创作成本时 |
| 11 | 自愈 agent（失败后自动改 YAML 重跑） | 修复必须过人（编辑器改 YAML → 重编译 → 对齐预演）。自动改写 + 自动重跑 = 无人审计的语义变更，直接对抗原则 4。R13 的 LLM 修复提议循环**不属此列**——它有强制人审批门（diff + `alignmentReport` 预览，批准后才 resume），提议权与批准权分离 | 对齐/重判基础设施成熟且有人审环节可嵌入之后 |
| 12 | UI 内嵌 YAML 编辑器 / 图形化编排 | UI 是投影不是创作面（§1 铁律 1/3）；内嵌编辑器把 UI 变成第二个 authoring 真相源 | 无（深链到用户编辑器是长期立场） |
| 13 | UI 发起新 run | 需要 runner 子进程全生命周期管理 + 参数表单，v0.1 收益不抵复杂度（§2.3） | M3a 之后按用户反馈评估 |
| 14 | 性能 / 负载测试语义 | Pointlock 断言的是功能语义，不是延迟分布；两者的执行与判定模型都不同 | 无（更可能是另一个产品） |
| 15 | OTel / 外部 observability 集成 | RunLog 即遥测。导出器是薄适配，任何时候都能加，不占 v0.1 | 有真实消费方时做一个只读导出器 |
| 16 | 单 flow 混编多 provider | 骨架 A.6 预留 `"<provider>:<action>"` 形式；v0.1 一 flow 一 provider，跨基座用 subflow 组合都不支持 | M3b（v0.2）三 provider 落地并稳定后 |
| 17 | `events.stream.open` 实时镜像 / `media.stream.*` 录屏证据 | reconcile 用 `events.list` 拉取已满足正确性；视频证据体积与呈现成本高 | UI 用户明确需要逐帧回放时（feature `media.stream.v1` 与 `events.stream.v1` 协议侧已就绪，纯 Pointlock 侧工作量问题） |
| 18 | 非 web 渲染器（TUI / native / 其他 flow 库）（R14 加注） | v0.2+ 可选项，不进 v0.1 承诺——`@pointlock/ui` 是 v0.1 唯一渲染器。但投影协议自 v1 起即按渲染器无关设计（00 §10.5）：界面层技术选型波动大于内核，协议先行使渲染器可替换，届时零协议改动接入 | v0.2+ 出现非 web 消费方或界面层 flow 库更换的真实需求时 |

---

## 8. 待骨架收编命名汇总（R12/R14 后全部清账）

本表历史上登记本文所需、骨架 Canonical Vocabulary 尚未收编的命名。**R12/R14 后本表已全部清账**：

- **已关闭项（R12，2026-07-16）**：`@pointlock/ui` 原列此表为「第 11 包」待收编项，R12 已将其正式收编进骨架 A.1（混合 monorepo 的 `packages/` 一侧，与新增的 `@pointlock/ir-types`、`@pointlock/nl-drafter` 并列），该开放问题关闭、条目移除。
- **已关闭项（R14，2026-07-16）**：其余四条如下表处置，全部关闭。

| 命名 | 类别 | 用处 | R14 处置 |
|---|---|---|---|
| `pointlock inspect --serve` | CLI flag | §1：启动 UI host | **已收编**：该 flag 为投影协议 HTTP+JSON 规范形入口（00 §10.4），不新增第 8 条命令，条目关闭 |
| `RunTimelineEntry` / `RunTimelineFilter` | 投影 DTO 类型 | §4：timeline 条目与过滤器（filter 取值逐字采用 DeviceRail `TimelineFilter` 五值） | **已收编**：`RunTimelineEntry` 进投影 DTO 五族（00 §10.1 / A.3），既有名保留；五过滤器取值约定随该 DTO 的既有裁决原样进入协议查询参数，条目关闭 |
| `revision` | 投影 DTO 字段 | §5：run 级单调失效版本号（实现上 = RunLog 最大 seq，无新持久化状态） | **已收编**：收编进 `RunOverview` DTO（00 §10.1 / A.3），条目关闭 |
| 图模型 node/edge type 字面量（`stepAction` `stepAssert` `stepCall` `stepHuman` `stepIf` `stepForeach` `stepLet`；`seq` `branch` `hook` `data`） | `@pointlock/ui` 内部枚举 | §3.1：React Flow 自定义类型名 | **裁决为不收编**：降级为 `@pointlock/ui` 内部 adapter 实现细节（00 §10.5），不进任何 Canonical Vocabulary；协议层对接面是 `FlowGraphView`，条目关闭 |

R14 传播后本文新登记 openQuestion 两条，**均已于 2026-07-17 裁决关闭**：`data` 数据依赖边 v0.1 从 UI 削除、登记为 v0.2 additive 扩展候选（骨架 00 §10.1 已裁决项，本文 §3.1 同步）；run 详情图状态叠加经 `RunOverview.steps` additive 扩展交付（本文 §5 已裁决项，00 §10.1 同步）。

---

*本文与 00-architecture-spine.md 的 Canonical Vocabulary 逐项核对；与 `@devicerail/live-visualizer` 的对齐事实（`TimelineFilter` 五值、`LIVE_TIMELINE_MAX_PAGE_SIZE = 50`、`ReferenceOnlyEvidence`、SSE revision invalidation、有界呈现默认限额）已对照 DeviceRail 源仓库 `packages/live-visualizer/src/{types,limits}.ts` 与 `apps/live-visualizer/README.md` 核实。*
