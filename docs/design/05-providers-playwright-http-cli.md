# Provider 扩展设计：Playwright / HTTP / CLI

> 本文是 Pointlock 设计文档系列第 5 篇，骨架见 00-architecture-spine.md。

本文定义三个 DeviceRail 之外的 Provider：**Playwright provider**（直连浏览器）、**HTTP/API provider**、**CLI provider**，并给出一条重要架构裁决：到达 Web 的两条路径（直连 Playwright vs 经 DeviceRail 的 web 设备）各自适用的场景。三个 Provider 全部实现骨架 §4 的 SPI（`Provider` / `ProviderSession`；R12 后 SPI 权威形态为 Rust trait，非 Rust 实现经 sidecar 接入，见 §1.5），全部服从骨架 §5 的 `ErrorClass` 封闭分类与十条设计原则；本文不新增任何执行语义——runner 对这三个 Provider 的调度、WAL、reconcile、verdict 折叠与骨架 §6 完全一致。

> **版本定位**：骨架规定 v0.1 唯一 Provider 是 `devicerail`。本文是这三个 Provider 的**开工即用设计**，其落地排期归 roadmap；设计现在锁定，防止 SPI 在 v0.1 期间被做成「DeviceRail 形状的私有接口」。依 R12（骨架 §0.1、§4），三个扩展 Provider 的**包名归属（Rust crate 还是 sidecar 包）列为 v0.2 收编议题，本轮不定名**（见 §1.5 与 §7 汇总）；本文以 manifest 名 `playwright` / `http` / `cli` 指称三者，文中如出现具体包名均为**待定**记号。
>
> **事实来源声明**：本文涉及 DeviceRail 的全部事实（§4 路径 B）已对照 DeviceRail 源仓库 `crates/playwright-remote/src/driver.rs` 逐项核实；引用的 DeviceRail 词汇与骨架附录 A.8 逐字一致。

---

## 1. 共同基座：非 DeviceRail Provider 如何满足 SPI

骨架的 SPI 是按 DeviceRail 的能力上限写的（外部 daemon、持久事件日志、verdict 回写、远程 Evidence）。这三个 Provider 都是**无外部账本**的适配器（进程形态见 §1.5：HTTP/CLI 可 in-process，Playwright 需 sidecar），必须诚实回答 SPI 每个方法「在我这里意味着什么」，而不是抛 not-implemented。本文其余处「进程内 Provider」一语均指「无外部 daemon/账本的 Provider」，与 §1.5 的 sidecar 进程形态不冲突。

### 1.1 能力三层的对应

| 层 | DeviceRail | Playwright / HTTP / CLI |
|---|---|---|
| `ProviderManifest`（静态，随包版本化） | 内置协议五件套 + feature 条件表 | **完整真相**。三者的动作集不依赖在线设备，`manifest.knownActions` 即全量动作，feature 全部 `guaranteed` |
| `CapabilityLockfile`（`pointlock lock` 固化） | 必需（`system.hello` + `device.capabilities`） | **可选**。骨架 §4.1 已允许无 lockfile 编译（退回 `manifest.knownActions`，仅 guaranteed feature）。Playwright provider 建议仍生成 lockfile：固化 `{ playwright 库版本, browserName, 浏览器版本 }`，使「浏览器升级导致行为漂移」成为可检测的 `capability_drift` 而非 flaky。HTTP/CLI 无环境事实可固化，直接 manifest 编译 |
| `CapabilityAttestation`（`openSession` 复核） | `system.hello` 重放 + digest 比对 | Playwright：启动浏览器后核对库/浏览器版本与 lockfile；不符 → `capability_drift`，拒跑。HTTP/CLI：以 manifest 规范形 digest 自证（见下） |

**lockfileDigest 代位约定（待骨架收编）**：`FlowIR.lockfileDigest` 与 `OpenSessionOptions.lockfileDigest` 是必填字段。无 lockfile 编译时，取 **manifest 规范形的 sha256** 作为代位 digest，语义不变：编译期看到的能力事实与运行期 attestation 必须逐字节同源，否则 `capability_drift`。

**manifest.protocol 的语义泛化（待骨架收编）**：骨架的 `protocol: { major, minMinor, maxMinor }` 对 DeviceRail 指 Protocol 1.5 窗口。对自有 Provider，该字段泛化为「Provider 后端契约版本窗口」：Playwright provider 填 Playwright 库 major 窗口；HTTP/CLI 填自身动作契约版本（`{ major: 1, minMinor: 0, maxMinor: 0 }`）。

### 1.2 reconcile 的普适答案：Provider 侧 dispatch journal

DeviceRail 的 `reconcile()` 靠 daemon 的 append-only 事件日志（`events.list` 按 `callId` 匹配；内存账本，限 daemon 存活期）白送 effectively-once。进程内 Provider 没有外部日志，但 **`ReconcileResult` 的四分语义不允许打折**——`neverDispatched` 是「可安全重放」的许可证，说谎会造成重复副作用。

统一机制：三个 Provider 各自维护一份 **dispatch journal**（append-only NDJSON，落在 run 的 store 目录下，随 Evidence 一起归档），每次 `execute(call)` 写两条记录：

```
{ "callId": ..., "phase": "dispatching", "atMs": ... }   ← fsync 后才发出真实副作用
{ "callId": ..., "phase": "terminal", "outcome": ..., "resultDigest": ..., "atMs": ... }
```

`reconcile(callId)` 的判定是纯查表：

| journal 状态 | `ReconcileResult.fate` | 依据 |
|---|---|---|
| 有 terminal 记录 | `completed`（复原 `ActionResult`，Evidence 已本地化） | 终态先于返回落盘 |
| 只有 dispatching 记录 | `startedNoTerminal` | 副作用可能已发出（HTTP 请求可能已抵达服务器、子进程可能已跑完） |
| 无任何记录 | `neverDispatched` | dispatching 先于副作用 fsync，无记录 ⟹ 副作用必未发出 |
| journal 文件损坏/缺失 | `logUnavailable` | 走骨架 §6.7-B 不确定分支 |

这与 runner 自己的 `actionIntent` WAL 是两层：runner 的 WAL 回答「我打算做什么」，Provider 的 journal 回答「我做到哪一步」。`startedNoTerminal` 在 DeviceRail 是「理论不应出现」（终态落盘有 shield），在进程内 Provider 是**正常可达状态**，其处置沿用骨架 §6.7-B：`idempotent: true` 或 `effect: "readonly"` → 重放；否则 `onResumeDrift` → 默认升级 `repairWorld` 人机节点。

### 1.3 其余 SPI 方法的最小义务

| 方法 | Playwright | HTTP | CLI |
|---|---|---|---|
| `observe()` | 截图 + domSnapshot（§3.5） | 编译期即不可达（无 dom/uiTree/vision 通道，`observe`/`screenshot` 动词绑不上——原则 5 在 bind 阶段拒绝）；防御性实现返回双 omission | 同 HTTP |
| `uiSnapshot(observationId)` | `{ ok: true, snapshot }`（domSnapshot 本地即取） | `{ ok: false, reason: "driverUnsupported" }` | 同 HTTP |
| `fetchEvidence(ref)` | 本地文件流（trace/截图/domSnapshot 生成时已在本地） | 同左（交换快照本地生成） | 同左（stdout/stderr 归档本地生成） |
| `recordVerdict(v)` | **先校验，后幂等 no-op 成功**。SPI 的 schema 上限（`summary ≤ 16384` 字符、`evidence ≤ 64` 条，骨架 §4）对一切实现生效：超限输入 fail-closed 抛 `bind_arguments_invalid`（与 04 §5 同一契约，一致性套件的「oversized verdict 拒写」断言对三者同样通过）；合法输入因三者均无外部 verdict 存证方而幂等 no-op 成功——Pointlock store 本就是判定权威（骨架 §6.3：判定权在 Pointlock），daemon 存证只是 DeviceRail 的附加福利 | 同左 | 同左 |
| `currentCursor()` | `{ sessionId: 本 ProviderSession uuid, lastSequence: journal 单调序号 }` | 同左 | 同左 |
| `health()` | 浏览器连接存活性 | `{ ok: true }`（无长连接） | `{ ok: true }` |
| `end(outcome)` | 停 trace → 关浏览器 → journal 落 `sessionEnded` 记录 | journal 收尾 | 杀残留进程组（§6.6）→ journal 收尾 |

### 1.4 expr 断言的 verifyVia 约定

HTTP 与 CLI 没有 UI，可用的断言谓词只有 `expr`（对 `steps.<id>.output.*` 的纯表达式）与 `expect_schema` 收窄。约定（待骨架收编为骨架 §3 的补充说明）：**`predicate.type === "expr"` 的断言，`verifyVia` 固定为 `[]`**——它不消费世界通道，输入是 `resolvedInputs` 与已入账的 step output，编译器校验并强制为空。其 unknown 只有一种来源：引用的输出缺失（如上游 step 因 `timedOut` 无 output）→ `onMissingInput: "unknown"`（原则 4）。

编译器据此对 HTTP/CLI provider 拒绝一切 `elementState` / `elementText` / `visual` 谓词与 `tap`/`find` 等 UI 动词：manifest 未声明对应 channel/action，bind 阶段即编译错误——**能力缺失是编译错误，不是运行期惊喜**（原则 5）。

### 1.5 SPI 进程边界：sidecar 形态（R12，新增）

骨架 §4 的 R12 注记：**SPI 的权威形态是 Rust trait（in-process）**；同时预留 **stdio JSON-RPC sidecar 适配形态**（v0.2），作为非 Rust provider 的接入方式，**v0.1 不实现 sidecar**。对本文三个 Provider 的含义：

- **Playwright provider**：依赖 Playwright Node API，Rust 核心下无法 in-process 实现，落地形态必然是 **Node sidecar**（v0.2 的 sidecar 规格是其前置）。
- **HTTP / CLI provider**：无 Node 依赖，原则上可作 Rust crate in-process 实现。
- 尽管如此，**三者的包名归属（Rust crate 还是 sidecar 包）统一列为 v0.2 收编议题，本轮不定名**（骨架 §4）。
- 本文的全部设计内容——manifest、动作契约、dispatch journal、错误映射、一致性义务——是对 Provider **实现语义**的约束，不随进程形态改变；sidecar 传输层的 RPC 帧格式、journal 落盘与 fsync 的进程归属等细节属 v0.2 sidecar 规格，本文不定义。

---

## 2. Playwright Provider（manifest 名 `playwright`；包名待 v0.2 收编定名，见 §1.5）

适配器经 Playwright Node API 直接持有浏览器（launch 或 connect），无外部 daemon。**R12 注**：Rust 核心下本 Provider 的落地形态是 Node sidecar（stdio JSON-RPC，v0.2 预留，v0.1 不实现，见 §1.5）；原「无 wire 协议」表述据此收窄为「无 DeviceRail 式外部 daemon 协议」——sidecar 的 stdio RPC 帧属 Pointlock 自管进程边界，不是对外协议面。

### 2.1 Manifest

```
name: "playwright"
protocol: Playwright 库 major 窗口（如 { major: 1, minMinor: 49, maxMinor: 55 }）
features.guaranteed:
  pointlock.playwright.domSnapshot.v1     # domSnapshot 观测（dom 通道的观测基座）
  pointlock.playwright.trace.v1           # per-step trace chunk 证据
channels:
  { channel: "dom",        role: "both",   requiresFeature: "pointlock.playwright.domSnapshot.v1" }
  { channel: "vision",     role: "verify" }        # 截图恒可得；VisionVerifier 见 pointlock-vision
  { channel: "coordinate", role: "act" }           # 静态坐标兜底（page.mouse）
  # 不声明 uiTree：web 世界只有一棵树，domSnapshot 即是它；声明两个通道徒增 verify-chain 歧义
```

feature id 用 `pointlock.*` 前缀，遵守骨架 A.6.5，与 DeviceRail 的 `device.*`/`observation.*` 等命名空间不混淆。

### 2.2 动作集与 Playwright API 映射

原生动作名（`BoundAttempt.actionName`）采用命名空间点分风格。全部 `protection: "standard"`；每个动作带 JSON Schema Draft 2020-12 的 `inputSchema` 与 `outputSchema`（编译期形状校验 + 运行期二次校验的依据）。

| actionName | effect | 关键入参（inputSchema 摘要） | Playwright API | output |
|---|---|---|---|---|
| `web.navigate` | mutating | `url`（`^https?://`，长度上限）、`waitUntil?: "load"\|"domcontentloaded"\|"networkidle"`（默认 `load`） | `page.goto(url, { waitUntil })` | `{ finalUrl, httpStatus? }` |
| `dom.query` | readonly | `selector`（§2.3 通用选择器对象） | locator 解析 + `locator.count()` + 首匹配摘要 | `{ matched: boolean, count: number, element?: { text?, value?, visible, enabled } }` |
| `element.click` | mutating | `selector`、`button?: "left"\|"middle"\|"right"`、`clickCount?` | `locator.click(...)`（strict，auto-wait） | acknowledgement |
| `element.fill` | mutating | `selector`、`value`（长度上限） | `locator.fill(value)` | acknowledgement |
| `element.clear` | mutating | `selector` | `locator.clear()` | acknowledgement |
| `element.select` | mutating | `selector`、`option: { value } \| { label } \| { index }` | `locator.selectOption(...)` | `{ selectedValues: string[] }` |
| `element.press` | mutating | `selector`、`key`（封闭键名集） | `locator.press(key)` | acknowledgement |
| `element.waitFor` | readonly | `selector`、`condition: "present"\|"visible"\|"enabled"\|"absent"`（= 骨架 elementState 值域）、`timeoutMs` | 见下方条件映射 | `{ matched, condition }` |
| `page.observe` | readonly | `wants: ("screenshot"\|"uiSnapshot")[]` | 截图 + domSnapshot 捕获（§2.5） | Observation |
| `page.screenshot` | readonly | `region?` | `page.screenshot(...)` | Observation（仅截图） |
| `pointer.click` | mutating | `x`、`y`、`button?` | `page.mouse.click(x, y)` | acknowledgement（coordinate 通道专用，§2.7） |

`element.waitFor` 的条件映射：`present` → `locator.waitFor({ state: "attached" })`；`visible` → `{ state: "visible" }`；`absent` → `{ state: "detached" }`；`enabled` → attached 后轮询 `isEnabled()` 至真或超时。条件值域刻意与骨架 elementState / DeviceRail `WaitForElementCondition` 完全同拼写，使 `wait_for` 动词跨 Provider 语义一致。

**动词绑定**（`manifest.verbBindings`，声明式 `argMap`，编译器零代码执行——骨架 R7）：

| CanonicalVerb | actionName |
|---|---|
| `tap` | `element.click` |
| `set_value` | `element.fill` |
| `clear` | `element.clear` |
| `wait_for` | `element.waitFor` |
| `find` | `dom.query` |
| `observe` | `page.observe` |
| `screenshot` | `page.screenshot` |
| `invoke` | 逃逸门（裸原生名） |

`web.navigate`、`element.select`、`element.press`、`pointer.click` 无对应 canonical verb，YAML 层经 `invoke` 使用（骨架 A.6.3：driver 专有动作一律 invoke）。**动词晋升提案（待骨架收编）**：`navigate` 与 `select_option` 已被两个 Provider 语义一致实现（本 Provider 的 `web.navigate`/`element.select` 与 DeviceRail playwright-remote driver 的 `navigate`/`select`），满足 A.6.3 的晋升门槛，建议下一次骨架评审收编为第 9、10 个 canonical verb。收编前本文示例一律用 `invoke` 形式。

### 2.3 选择器：`ElementSelectorIR` → locator 策略

Pointlock 通用选择器（骨架 §3，同构 DeviceRail `ElementSelector`）到 Playwright locator 引擎的映射是**声明式、可组合**的：

| ElementSelectorIR 字段 | locator 引擎 |
|---|---|
| `css` | `page.locator(css)` |
| `role` + `name` | `page.getByRole(role, { name })` |
| `text: { value, mode, caseSensitive }` | `mode: "exact"` → `getByText(value, { exact: true })`；`contains` → `getByText(value)`；`caseSensitive: false` → 编译为不区分大小写正则 |
| `identifier` | `page.getByTestId(identifier)`（testId 属性名在 Provider 配置中声明，默认 `data-testid`） |
| `value` | 候选集上过滤 input 当前值（`locator.filter` + 求值） |
| `context.contextId` | frame 选择：`page.frameLocator(...)` 前缀；`contextKind` 恒为 `web` |

多字段并存 = **AND 细化**：以最具体引擎为基座，其余字段作 `locator.filter` 链。两条硬规则：

1. **act-chain 强制 strict**：mutating 动作的选择器解析出多于一个元素 → Playwright strict violation → `failed`（`retryable: false`）→ `action_failed_final`。歧义选择器是 flow 的 bug，不是可重试的环境问题。
2. **`dom.query` 允许多匹配**：它是探查（readonly），`count` 如实报告。

这个映射的价值：**同一份 YAML 的选择器写法在 DeviceRail 原生设备与 Playwright 浏览器之间可移植**——作者写 `role + name`，DeviceRail 走无障碍树，Playwright 走 ARIA locator，语义一致。

### 2.4 Auto-waiting 与 Pointlock 显式断言的分工（重要辨析）

Playwright 的 auto-waiting（actionability 检查：attached / visible / stable / receives-events / enabled）**全部属于 act 阶段的执行可靠性**，与判定无关：

| 关切 | 归属 | 机制 | 产物 |
|---|---|---|---|
| 「元素还没就绪，点了会脱靶」 | act 阶段（Provider 内部） | Playwright auto-wait，预算封顶于 `BoundActionCall.actionTimeoutMs` | 无 verdict；超时 → outcome `timedOut` |
| 「UI 需要时间到位再继续」 | 显式同步 step | `wait_for` 动词（`element.waitFor`，readonly） | 无 verdict（除非该步另配 `expect`） |
| 「页面此刻是否处于正确状态」 | assert 阶段（runner） | `expect` 断言对**已归档 Observation** 纯函数求值 | verdict `pass \| fail \| unknown` |

三条纪律：

1. **Provider 禁用 Playwright 的 `expect()` web-first 断言**。它是「轮询直到为真」，与骨架 §6.2「`asserting` 是纯计算、无 I/O、可离线重放」直接冲突。判定只能由 runner 对存档观测求值——这是双哈希离线重判（骨架 §6.7 `judgeDirty`）成立的前提。
2. **auto-wait 成功 ≠ 语义通过**（原则 3）。click 完成只说明「点到了一个可点的元素」，页面是否进入期望状态由 `expect` 判。无断言的 mutating step 不产生 verdict，报告标 `unverified`（骨架 R4）。
3. **断言不重试**（骨架 §6.5）。断言看到的是 observe 阶段定格的世界；如果世界「还差一拍」，正确写法是在 act 与 assert 之间加 `wait_for` step，而不是重跑断言。

### 2.5 Observation 与 dom 通道：domSnapshot

dom 通道断言（`elementState` / `elementText`）的求值输入是 **domSnapshot**：`page.observe` / mutating 动作的 before/after 观测时捕获的规范化 DOM 树 JSON——每个节点带 `{ role, name, text, value, visible, enabled, cssPath }`，状态在**捕获时**由 Provider 计算定格。Observation 结构同构 DeviceRail：`{ id, capturedAtMs, viewport, screenshot?, uiSnapshot?, ... }`，domSnapshot 作为 `uiSnapshot` 载荷、截图作为 `screenshot` 载荷，各自派生内容寻址 Evidence 并在 `observing` 阶段本地化。

runner 的断言求值于是保持纯函数：`elementText` 谓词 = 在存档 domSnapshot 上按 §2.3 同一套选择器语义匹配节点、比对文本。verify-chain 声明 `verify_via: [dom, vision]` 时，dom 通道因页面丢失/快照捕获失败而缺料 → 依骨架 §6.3 规则 3 降级到 vision（对截图问 VisionVerifier）；**dom 通道明确判否则终局，vision 无权翻案**（骨架 R5）。

### 2.6 Evidence：trace 与截图

| Evidence | 产生方式 | mediaType | 挂载点 |
|---|---|---|---|
| **trace chunk** | `tracing.start` 于 openSession；每个 action step 以 `tracing.startChunk` / `stopChunk` 包裹，产出 per-step `trace.zip` | `application/zip` | `ActionResult.evidence`——步级证据，`pointlock locate` 交付案卷时可直接 `npx playwright show-trace` 打开 |
| **截图** | mutating 动作 before/after 各一张（Provider 配置可关 before） | `image/png` | Observation → EvidenceRef |
| **domSnapshot** | 见 §2.5 | `application/json` | Observation → EvidenceRef |

全部按 sha256 进 `pointlock-store` 内容寻址库。trace 是**证据不是判据**：verdict 的折叠输入只有断言结果；trace 供人审计与调试，避免「trace 里看起来对就算过」的隐性判定。

### 2.7 ExecutionMode 与 coordinate 通道

本 Provider **从不自行降级**：语义动作的 `ActionResult.execution.mode` 恒为 `webSemantic`。作者在 `locate_via` 显式声明了 `coordinate` 通道时，编译器要求静态坐标（骨架 A.7：写了 coordinate 就必填 `coordinate:`），绑定为 `pointer.click` attempt，其 execution 如实报 `{ mode: "coordinateFallback", fallbackReason: "semanticInteractionUnavailable" }`——封闭 `ExecutionMode` 三值中对坐标动作唯一如实的取值；该 attempt 只在语义 attempt 以 `action_failed_final` 落败后才被尝试。由于该通道是作者显式声明的，编译器已把 `coordinateFallback` 写入该 attempt 的 `acceptExecutionModes`，不触发骨架 §6.4 的 R-degrade。

**verdict 不因此标 `degraded`**：骨架 §6.3 的折叠规则是封闭的，`degraded=true` 只有两个来源——`degradedVerify`（验证走了非首选通道）与 R-degrade（daemon 未授权降级）。作者授权的 act-chain 兜底两者都不是：显式声明的 fallback 是正常执行路径，不是降级事故（原则 6），因此 verdict `degraded=false`，`verdict_policy: strict` 下不折叠为 unknown——与 DeviceRail 的坐标兜底 attempt（04 §9.4.2，driver `tap {x,y}`）折叠行为一致，同一份 YAML 在两个 provider 下结果相同。「act 推进到了第几个 attempt、走的哪个通道」的事实完整留痕于 `AttemptRecord`（attempt 序号、前序 attempt 的 `action_failed_final`、`execution.mode`，骨架 §6.6），供报告与审计呈现；坐标点击命中后页面是否真的进入期望状态，仍由该步 `expect` 断言判定（原则 3）。

### 2.8 错误映射（`ErrorInfo.code` 为 Provider 自有码，`pw_` 前缀；封闭映射到 ErrorClass）

| 现象 | outcome | code | retryable | ErrorClass |
|---|---|---|---|---|
| auto-wait / 动作超时（TimeoutError） | `timedOut` | `pw_timeout` | — | `action_timed_out`（`idempotent: true` 才自动重试，否则 reconcile/observe 确认，确认不了 → unknown 路径） |
| strict violation（选择器多匹配） | `failed` | `pw_strict_violation` | false | `action_failed_final` |
| 元素中途 detach / execution context destroyed | `failed` | `pw_target_detached` | true | `target_stale`（重试前强制重新 observe） |
| 导航网络错误（net::ERR_*） | `failed` | `pw_navigation_failed` | true | `action_failed_retryable` |
| 页面意外关闭但浏览器存活 | `failed` | `pw_page_lost` | false | `session_degraded` |
| 浏览器崩溃 / 连接断开 | （throw） | `pw_browser_lost` | — | `transport_lost` → run suspend → 重开 ProviderSession（新浏览器上下文，记 `sessionLineage`）→ checkpoint resume |
| `AbortSignal` 取消 | `cancelled` | `pw_cancelled` | — | `action_cancelled` |
| 实参未过 inputSchema | （先于 dispatch） | — | — | `bind_arguments_invalid` |

注意 `transport_lost` 后的 resume 语义与 DeviceRail 断代重开一致，但世界状态更糟：浏览器上下文死了，页面状态全丢。因此 Playwright flow 的 resume 探针（`preflight`）与 `onResumeDrift` 修复 subflow（如 `ensureLoggedIn`）**不是可选项而是标配**——骨架 §6.7-C「Pointlock 从不假设设备在暂停期间没被动过」在这里升级为「暂停后世界必然重置」。

### 2.9 完整示例

```yaml
flow: web-login
provider: playwright
params:
  - name: baseUrl
    schema: { type: string }
  - name: username
    schema: { type: string }
  - name: password          # v0.2 起应迁移为 secrets.*（骨架预留），v0.1 明文参数自担风险
    schema: { type: string }
verdict_policy: standard

steps:
  - id: openLogin
    invoke: { action: "web.navigate", args: { url: "${{ concat(params.baseUrl, '/login') }}" } }
    effect: mutating
    idempotent: true                      # 重复导航到同一 URL 无害 → 崩溃恢复可安全重放
    expect:
      - element: { role: "textbox", name: "Username" }
        state: visible
        verify_via: [dom]

  - id: typeUsername
    set_value:
      element: { role: "textbox", name: "Username" }
      value: "${{ params.username }}"
    effect: mutating
    idempotent: true                      # fill 是覆写语义，天然幂等

  - id: typePassword
    set_value:
      element: { role: "textbox", name: "Password" }
      value: "${{ params.password }}"
    effect: mutating
    idempotent: true

  - id: submit
    tap:
      element: { role: "button", name: "Sign in" }
      locate_via: [dom, coordinate]       # 显式兜底：语义定位失败才用静态坐标
      coordinate: { x: 640, y: 520 }
    effect: mutating                      # 提交不幂等：崩溃恢复走 reconcile，不盲目重放
    retry: { max_attempts: 2, backoff_ms: 500, retry_on: [target_stale] }

  - id: settled
    wait_for:
      element: { css: "[data-testid=dashboard]" }
      state: visible
    timeout_ms: 10000
    effect: readonly

  - id: verifyLoggedIn
    observe: {}
    effect: readonly
    expect:
      - element: { css: "[data-testid=dashboard] .welcome" }
        text: { value: "${{ params.username }}", mode: contains }
        verify_via: [dom, vision]         # dom 缺料才降级 vision；dom 判否即终局
      - visual: { prompt: "页面处于已登录的仪表盘状态", region: { x: 0, y: 0, w: 1280, h: 200 } }
        verify_via: [vision]              # 纯视觉断言只能做补充验证（原则 7）
    on_unknown:
      - escalate:
          mode: judge
          prompt: "无法机判登录态，请人工判定"
          presents: ["${{ steps.verifyLoggedIn.output }}"]
          decisions: [pass, fail]
          on_timeout: unknown
```

---

## 3. 到达 Web 的两条路径（重要架构说明）

DeviceRail 自己带一个 playwright driver（`crates/playwright-remote`，`Platform::Web`）。于是 Pointlock 到达浏览器有两条路径，**必须说清取舍，否则用户必然困惑**：

```
路径 A（直连）：
  runner ── Playwright provider（包名待定；Rust 核心下为 Node sidecar，v0.2）── Playwright Node API ── 浏览器

路径 B（经 DeviceRail）：
  runner ── pointlock-provider-devicerail ── NDJSON/JSON-RPC ── daemon
         ── playwright-remote driver ── 浏览器（daemon 眼中的一台 web 设备）
```

### 3.1 路径 B 的实测事实（对照 driver 源码逐项核实）

- **动作集是有界的 10 个 driver 专有 action**：`navigate`、`click`、`fill`、`fillSecret`、`press`、`select`、`scroll`、`waitFor`、`elementExists`、`textContains`。**不含语义五件套**（无 `findElement`/`tapElement`/`setElementValue`/`clearElement`/`waitForElement`），即该设备不提供 `device.semanticActions.v1`——canonical verbs `tap`/`set_value`/`clear`/`wait_for`/`find` 在此设备上**绑不上**，全部动作走 `invoke`（能力绑定编译如实反映这一点，原则 5）。
- **选择器只有 strict CSS**：`selector` 是长度受限、仅可打印 ASCII（`^[ -~]+$`）的 CSS 字符串。无 role/name/text/testId 引擎。
- **无 uiSnapshot**：Observation 恒为 `ui_snapshot: None`（且无 omission 申报）。**uiTree/dom 观测通道在此设备不存在**，`elementState`/`elementText` 谓词无结构化求值输入。结构化「断言」改由两个 **readonly 探查 action** 承担：`elementExists` → output `{ exists: boolean }`；`textContains` → output `{ contains: boolean }`——先 invoke 探查、再对 output 做 `expr` 断言。
- **截图可用**：受 `ScreenshotPolicy` 管辖，omission 原因 `policy` / `protectedAction`（与骨架 A.8 枚举一致）→ vision 验证通道可用。
- **不申报 execution**：`ActionResult.execution` 恒缺席（`execution: None`）。缺席 ≠ 降级申报，不触发 R-degrade；报告层如实记「provider 未申报执行模式」。
- **`fillSecret` 是 `protection: protected`** 的动作（填敏感值、不留截图与持久实参）——v0.1 编译器在 bind 阶段 fail-closed 拒绝（骨架 R6），它是 v0.2 `secrets.*` 路径的着陆点。
- **daemon 账本可用（内存态）**：session 事件日志（`events.list` 按 `callId` reconcile）、`verdict.record` 回写存证、`session.export` 导出——这是路径 A 没有的。注意账本随 daemon 进程消亡（04 §9.1 同一事实），持久留档须经 `session.export` 落盘。

### 3.2 取舍对照

| 维度 | 路径 A：直连 provider-playwright | 路径 B：经 DeviceRail web 设备 |
|---|---|---|
| 动作词汇 | canonical verbs 全量可绑 + 富动作集 | 仅 `invoke` + 10 个有界 action |
| 选择器 | ElementSelectorIR 全引擎（role/text/testId/css），可移植 | strict CSS 字符串 only |
| 元素断言 | `elementState`/`elementText` 谓词 ↔ domSnapshot，离线可重判 | 无 dom/uiTree 通道；readonly 探查 action + `expr` 断言，或 vision 对截图 |
| 离线重判（judgeDirty） | 强：domSnapshot 存档，改断言零设备重跑 | 弱：探查结果是 act 阶段产物，改「断言」（实为探查+expr）动的是 effectHash → 历史失效 |
| Evidence | per-step trace chunk + 截图 + domSnapshot | 截图 before/after + daemon session log/`session.export` |
| reconcile | Provider dispatch journal（§1.2，NDJSON 落盘，Pointlock 任何崩溃后仍在） | daemon `events.list`（内存账本）：transport 断裂或 attach 模式下 Pointlock 崩溃可查；daemon 进程消亡（spawn 模式下 Pointlock 崩溃 → stdin EOF → daemon 退出，04 §9.1）→ `logUnavailable` |
| protected 动作 | 无 | `fillSecret` 已存在（v0.2 `secrets.*` 的现成落点） |
| 治理与审计 | Pointlock store 单账本（落盘） | 双账本：Pointlock store + daemon session 存证（`verdict.record`，内存态；持久留档需 `session.export` 导出） |
| 浏览器归属 | Pointlock 进程内自管 | daemon 管辖，与 android/ios 等设备同一设备场、同一套运维 |
| 部署 | 零外部依赖 | 需 daemon 进程 |

### 3.3 推荐（决策规则，写进用户文档）

> **R12 加注**：Rust 核心下，直连 Playwright（路径 A）需 Node sidecar（stdio JSON-RPC，v0.2 预留形态，v0.1 不实现，见 §1.5），可用时点后移；「经 DeviceRail web 设备」（路径 B）因 `pointlock-provider-devicerail` 是 v0.1 既有 Rust 路径，**在 v0.x 的权重上升**。下列取舍对照与决策规则本身不变；在路径 A 落地前，v0.x 实际可用的到达 Web 路径只有路径 B。

1. **纯 Web 端到端测试 → 路径 A。** 富选择器、domSnapshot 离线重判、trace 证据是核心生产力；这是 Web 流程的默认路径。
2. **Web 是设备场的一员 → 路径 B。** 团队已运行 DeviceRail daemon 管理手机/桌面设备，希望 web「设备」纳入同一套设备清单、运维与远程接入时，用路径 B——代价是接受有界动作集与 CSS-only 选择器。注意 daemon 侧存证是内存态的，持久审计账本在两条路径下都以 Pointlock store 为准（路径 B 可辅以 `session.export` 落盘），审计持久性不构成选 B 的理由。
3. **需要 protected 填值（v0.2）→ 路径 B。**`fillSecret` + `action.protected.v1` 是现成协议事实；路径 A 侧尚无对等物。
4. **同一 flow 禁止混用两条路径。**`FlowIR.provider` 是单值，一次 Run 绑定恰好一台设备/一条会话（骨架 §2 概念 1），这是结构性禁止，不是风格建议。需要注意：v0.x 的 Run 模型本就不支持单 Run 跨设备编排，「app + web 混合流程」在两条路径下都不可行，不构成选 B 的理由（跨 Run 编排见 openQuestions）。

### 3.4 路径 B 示例片段（注意与 §2.9 的形态差异）

```yaml
flow: web-login-via-devicerail
provider: devicerail            # 设备选择在 binding 层指向 web 平台设备
steps:
  - id: openLogin
    invoke: { action: "navigate", args: { url: "${{ concat(params.baseUrl, '/login') }}" } }
    effect: mutating
    idempotent: true

  - id: submit
    invoke: { action: "click", args: { selector: "#signin" } }   # strict CSS only
    effect: mutating

  - id: probeDashboard                       # 结构化探查是 readonly action，不是断言谓词
    invoke: { action: "elementExists", args: { selector: "[data-testid=dashboard]" } }
    effect: readonly
    expect:
      - expr: "${{ eq(steps.probeDashboard.output.exists, true) }}"
      - visual: { prompt: "页面处于已登录的仪表盘状态" }
        verify_via: [vision]
```

---

## 4. HTTP/API Provider（manifest 名 `http`；包名待 v0.2 收编定名，见 §1.5）

### 4.1 Manifest 与动作契约

```
name: "http"
protocol: { major: 1, minMinor: 0, maxMinor: 0 }        # 动作契约版本
features.guaranteed: [ pointlock.http.exchangeEvidence.v1 ]
channels: []                                             # 无 UI 通道；断言仅 expr（§1.4）
verbBindings: []                                         # 无 canonical verb 适用；一律 invoke
knownActions: [ http.request ]
```

`http.request` 契约：

```
inputSchema（摘要）:
  method   : enum [GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS]（必填）
  url      : string, ^https?://, 长度上限, 禁 userinfo（user:pass@）
  headers? : object<string, string>（键规范化为小写）
  query?   : object<string, string>
  body?    : { json: any } | { text: string } | { form: object<string,string> }（互斥）
  timeoutMs?          : 整数, 上限受 actionTimeoutMs 封顶
  idempotencyKey?     : "callId" | "none"（默认见 §4.4）

outputSchema:
  status     : integer            # 任何送达的响应都在这里，包括 4xx/5xx
  statusText : string
  headers    : object<string, string>   # 小写键；已脱敏视图（§4.3）
  body?      : any                # content-type 为 JSON 且 ≤ 内联上限（默认 256 KiB）时为解析值；文本同理
  bodyEvidenceSha256? : string    # 超限或二进制时，正文只入 Evidence，此处留内容地址
  timing     : { startedAtMs, firstByteMs, totalMs }
  redirects  : [{ url, status }]  # 每一跳留痕
```

**effect 推导**：编译器默认 `GET/HEAD/OPTIONS → readonly`，其余 `mutating`；`idempotent` 默认按 RFC 语义 `GET/HEAD/PUT/DELETE/OPTIONS → true`、`POST/PATCH → false`。两者作者均可显式覆盖，覆盖以作者为准（作者最懂自己的 API）。

### 4.2 判定纪律：响应不是失败（原则 3 的 HTTP 化）

**任何被送达的 HTTP 响应——包括 500——都是 outcome `succeeded`**，状态码进 `output.status`，好坏由断言判。只有传输层事故才是动作失败：

| 现象 | outcome | code | retryable | ErrorClass |
|---|---|---|---|---|
| DNS 失败 / 连接拒绝 / 连接重置 | `failed` | `http_transport_error` | true | `action_failed_retryable` |
| TLS 证书校验失败 | `failed` | `http_tls_error` | false | `action_failed_final` |
| 超时（连接或整体） | `timedOut` | `http_timeout` | — | `action_timed_out` |
| 重定向越出 allowlist（§4.5） | `failed` | `http_policy_denied` | false | `action_failed_final` |
| `AbortSignal` 取消 | `cancelled` | `http_cancelled` | — | `action_cancelled` |
| 实参未过 inputSchema | — | — | — | `bind_arguments_invalid` |

「等 503 恢复再试」不属于 act 重试（响应是 succeeded），正确写法是断言 + `on_fail` handler 重试——骨架 §6.5 的第二挂载点，示例见 §4.6。

### 4.3 断言与 Evidence（含脱敏）

断言全部是 `expr` 谓词（`verifyVia: []`，§1.4）：

- **status**：`${{ eq(steps.r.output.status, 201) }}`
- **header**：`${{ eq(jsonPath(steps.r.output.headers, "$['content-type']"), "application/json") }}`
- **jsonpath**：`${{ eq(jsonPath(steps.r.output.body, "$.data.role"), "admin") }}`；对 `invoke` 输出取深层字段前按骨架 §7 用 `expect_schema` 收窄。

**Evidence = 交换快照**：每次 `http.request` 归档一份 JSON 文档 `{ request: { method, url, headers, bodyDigest 或内联 }, response: { status, headers, body 或 bodyDigest }, timing, redirects }`，内容寻址入库，挂 `ActionResult.evidence`。

**脱敏（宪法级顺序）：redaction 发生在归档与哈希之前**，Evidence 库中从不存在密文可回溯的字节：

- header 默认脱敏名单：`authorization`、`cookie`、`set-cookie`、`proxy-authorization`、`x-api-key`（Provider 配置可增删）；值替换为固定标记 `[REDACTED]`——**不用密文哈希替换**（低熵秘密的哈希可被字典碰撞）。
- body 脱敏按 Provider 配置的 jsonPath 路径表执行，同样替换为 `[REDACTED]`。
- `output.headers` / `output.body` 暴露给表达式的也是脱敏后视图——被脱敏字段不可断言，这是刻意的：断言秘密值本身就是把秘密写进 flow 的反模式；v0.2 `secrets.*`（骨架预留）落地前，鉴权头的值只从 Provider 配置注入，不经 YAML。

### 4.4 幂等与 reconcile

- dispatch journal 语义同 §1.2。`startedNoTerminal` 在 HTTP 上含义具体：请求可能已抵达服务器并被处理。
- **`idempotencyKey: "callId"`（mutating 动作默认开启，可显式关）**：Provider 把 `callId` 注入 `Idempotency-Key` 请求头。服务端支持该约定时，`startedNoTerminal` 分支的人工裁决（骨架 §6.7-B 的 adopt/redo/abort）可以放心选 redo——与 DeviceRail 用 `device.execute` caller-UUID 达成 effectively-once 是同一思想的应用层版本。

### 4.5 安全约束

- **host allowlist**：Provider 绑定配置声明允许的 host 模式表；`url` 求值后逐跳（含每次重定向）核对，越界 → `http_policy_denied`。策略属绑定配置不属 flow——flow 要可移植，策略随环境走。
- 仅 `http/https`；禁 URL userinfo；重定向默认跟随（上限 5 跳），每跳留痕于 `output.redirects`。
- 表达式产物进 URL/query 前由 Provider 做 RFC 3986 编码；秘密值禁入 URL（脱敏表无法覆盖 URL，编译器对 `url`/`query` 中引用将来 `secrets.*` 的表达式直接拒绝——现在即写入规则，v0.2 生效）。

### 4.6 完整示例

```yaml
flow: api-create-user
provider: http
params:
  - name: apiBase
    schema: { type: string }
steps:
  - id: health
    invoke: { action: "http.request", args: { method: GET, url: "${{ concat(params.apiBase, '/healthz') }}" } }
    effect: readonly
    expect:
      - expr: "${{ eq(steps.health.output.status, 200) }}"
    on_fail:                                  # 等待服务就绪：断言失败 → handler 重试整步
      - retry: { max_attempts: 5, backoff_ms: { initial: 1000, factor: 2, max: 10000 }, retry_on: [] }

  - id: createUser
    invoke:
      action: "http.request"
      args:
        method: POST
        url: "${{ concat(params.apiBase, '/users') }}"
        body: { json: { name: "ada", role: "admin" } }
        idempotencyKey: callId                 # 崩溃恢复的 redo 安全
    effect: mutating
    timeout_ms: 15000
    expect:
      - expr: "${{ eq(steps.createUser.output.status, 201) }}"
      - expr: "${{ eq(jsonPath(steps.createUser.output.body, '$.role'), 'admin') }}"
      - expr: "${{ not(eq(jsonPath(steps.createUser.output.body, '$.id'), null)) }}"

outputs:
  - name: userId
    schema: { type: string }
    from: "${{ jsonPath(steps.createUser.output.body, '$.id') }}"
```

---

## 5. CLI Provider（manifest 名 `cli`；包名待 v0.2 收编定名，见 §1.5）

### 5.1 Manifest 与动作契约

```
name: "cli"
protocol: { major: 1, minMinor: 0, maxMinor: 0 }
features.guaranteed: [ pointlock.cli.outputEvidence.v1 ]
channels: []                                             # 断言仅 expr（§1.4）
verbBindings: []
knownActions: [ proc.run ]
```

`proc.run` 契约：

```
inputSchema（摘要）:
  argv        : string[]（minItems 1）—— argv 数组，永不经 shell 解释
  cwd?        : string（相对路径，锚定于策略 root 之下）
  env?        : object<string, string>（仅显式键值，见 §5.5 的环境策略）
  stdin?      : string
  killGraceMs?: 整数（默认取 Provider 配置 defaultKillGraceMs = 5000）

outputSchema（仅 outcome succeeded 时存在）:
  exitCode        : integer        # 非零不是失败！见 §5.3
  durationMs      : integer
  stdoutTail      : string         # 末尾 N 字节（默认 8 KiB），供 expr 断言
  stderrTail      : string
  stdoutTruncated : boolean
  stderrTruncated : boolean
  json?           : any            # 完整 stdout 是合法 JSON 且 ≤ 内联上限时的解析值
```

**无 shell 是硬规则**：`argv` 直接 spawn，表达式插值只能落在单个 argv 元素内、作为值而非 shell 词法参与——命令注入在结构上不存在。需要管道/重定向的场景写一个受控脚本进白名单，而不是给 flow 开 shell。

### 5.2 捕获为 Evidence

- 完整 stdout / stderr 各自流式写入内容寻址 Evidence（`mediaType: text/plain` 或按嗅探），上限 `maxOutputBytes`（默认 16 MiB），超限截断并在 Evidence 元数据记截断标记；`ActionResult.evidence` 挂两条 `EvidenceRef` + 一条执行摘要 JSON（argv 脱敏视图、cwd、退出方式、时长）。
- `output.stdoutTail`/`stderrTail` 是**断言友好的内联窗口**；判据要更多内容时，作者应让命令输出 JSON（走 `output.json`）或落文件后用后续 readonly step 读取，而不是指望把 16 MiB 塞进表达式。

### 5.3 断言：exitCode 不是失败（原则 3 的进程化）

进程正常结束——**无论退出码几**——outcome 都是 `succeeded`，好坏由断言判：

```yaml
expect:
  - expr: "${{ eq(steps.build.output.exitCode, 0) }}"
  - expr: "${{ regexMatch(steps.build.output.stdoutTail, 'Compiled successfully') }}"
```

只有「跑不起来 / 跑不完」才是动作层事故：

| 现象 | outcome | code | retryable | ErrorClass |
|---|---|---|---|---|
| 可执行文件不存在 / 无权限（ENOENT/EACCES） | `failed` | `cli_spawn_failed` | false | `action_failed_final` |
| 白名单/沙箱策略拒绝（§5.5，dispatch 前检查，journal 无 dispatching 记录） | `failed` | `cli_policy_denied` | false | `action_failed_final` |
| 超时被杀（§5.4） | `timedOut` | `cli_timeout` | — | `action_timed_out`（`ErrorInfo.details` 携带已捕获的 tail 与 Evidence 引用，尸检不缺料） |
| `AbortSignal` 取消 | `cancelled` | `cli_cancelled` | — | `action_cancelled` |
| 被外部信号杀死（非 Pointlock 所为） | `failed` | `cli_killed_externally`（details 含 signal） | true | `action_failed_retryable` |
| 实参未过 inputSchema | — | — | — | `bind_arguments_invalid` |

### 5.4 超时与 kill

子进程 spawn 进**独立进程组**。`actionTimeoutMs` 到点：对进程组发 `SIGTERM` → 等 `killGraceMs` → 仍存活则 `SIGKILL` 进程组。outcome `timedOut`；两段式确保「清理型」进程有机会自己收尾，同时保证进程树（含孙进程）不逃逸。`AbortSignal` 取消走同一杀链，outcome `cancelled`。

### 5.5 安全约束（策略属 Provider 绑定配置，不属 flow）

```
Provider 配置（进 binding，不进 YAML；flow 保持可移植）:
  root      : 绝对路径。cwd 解析结果必须位于 root 之下（realpath 后校验，防 .. 与符号链接逃逸）
  allow     : [ { bin: "/usr/bin/git" | "pnpm", argvPattern?: ["build", "*"] } ]   # 可执行白名单 + 可选实参模式
  envMode   : "none" | "allowlist"（默认 none：子进程环境 = 空 + PATH 极小集 + flow 显式 env）
  envAllow  : [ "CI", "LANG", ... ]                       # envMode=allowlist 时放行的宿主变量
  maxOutputBytes / tailBytes / defaultKillGraceMs
```

三条理由性说明：(1) 白名单按**解析后的可执行绝对路径**匹配，不按 argv[0] 字符串——防 PATH 劫持；(2) 环境默认不继承——宿主环境里的 token 类变量不该悄悄流进被测命令，也不该流进 Evidence（执行摘要里的 env 只记键名不记值）；(3) 策略校验发生在 journal `dispatching` 记录**之前**，被拒的调用在 reconcile 视角等同 `neverDispatched`。

### 5.6 reconcile 与孤儿进程

journal 语义同 §1.2，附加进程级事实：`dispatching` 记录携带 `{ pid, pgid, processStartTime }`。Pointlock 崩溃时子进程可能沦为孤儿继续运行。resume 时 `openSession` 先做**孤儿清理**：扫描 journal 中无 terminal 的记录，凭 `pid + processStartTime` 双因子确认仍是同一进程（防 pid 复用误杀）后杀其进程组，然后 `reconcile(callId)` 如实返回 `startedNoTerminal`——命令效果是否已发生（比如 `git push` 推没推出去）交给骨架 §6.7-B：readonly/幂等步重放，否则 `onResumeDrift` 升级人工 `repairWorld` 裁决。**Provider 不猜测半途命令的世界效果**（原则 4）。

### 5.7 完整示例

```yaml
flow: build-and-smoke
provider: cli
steps:
  - id: build
    invoke: { action: "proc.run", args: { argv: ["pnpm", "build"], cwd: "app" } }
    effect: mutating            # 写 dist/，非幂等场景保守申报；纯可重入构建可标 idempotent: true
    idempotent: true
    timeout_ms: 600000
    expect:
      - expr: "${{ eq(steps.build.output.exitCode, 0) }}"

  - id: smoke
    invoke: { action: "proc.run", args: { argv: ["node", "scripts/smoke.mjs"], cwd: "app" } }
    effect: readonly
    timeout_ms: 60000
    expect:
      - expr: "${{ eq(steps.smoke.output.exitCode, 0) }}"
      - expr: "${{ eq(jsonPath(steps.smoke.output.json, '$.failures'), 0) }}"
    on_unknown:
      - escalate:
          mode: judge
          prompt: "smoke 输出无法机判（可能非 JSON 或被截断），请人工判定"
          presents: ["${{ steps.smoke.output.stdoutTail }}", "${{ steps.smoke.output.stderrTail }}"]
          decisions: [pass, fail]
          on_timeout: unknown
```

---

## 6. 一致性义务

三个 Provider 必须通过 `pointlock-provider-kit` 的一致性测试套件（与 FakeProvider 同一套契约测试）：SPI 全方法语义、四分终态不折叠不翻译、journal 三态 reconcile、`ErrorClass` 映射表、omission 传导为断言 unknown、`recordVerdict` 契约（oversized 输入 fail-closed 拒写——与 04 §8 是同一条断言；合法输入幂等 no-op 成功——无外部存证方的实现专属断言）、`end` 四值 outcome。Playwright provider 另需通过 domSnapshot 的**离线重判回归**：对存档 Observation 重放 `elementState`/`elementText` 断言必须与在线求值逐位一致——这是 `judgeDirty` 路径（骨架 §6.7-A）的正确性底线。

## 7. 待骨架收编清单（本章新增，需下次骨架评审裁决）

1. ~~包名三枚~~ **已由 R12 裁决改道**：三个扩展 Provider 的包名归属（Rust crate 还是 stdio JSON-RPC sidecar 包）列为 **v0.2 收编议题，本轮不定名**（骨架 §4、§0.1 R12；本文 §1.5）。届时随 sidecar 规格一并定名三枚包名；骨架 A.1 当前为 10 crate + 4 package 封闭清单（R14 增 `@pointlock/projection-types`）。
2. `FlowIR.provider.name` 类型由字面量 `"devicerail"` 放宽为 provider 名字符串联合。
3. lockfileDigest 代位约定（无 lockfile 编译时取 manifest 规范形 digest，§1.1）与 `manifest.protocol` 语义泛化。
4. `expr` 谓词 `verifyVia: []` 的编译强制约定（§1.4）。
5. canonical verb 晋升提案：`navigate`、`select_option`（已满足「两个 Provider 语义一致实现」门槛，§2.2）。
6. `invoke` 的实参子键拼写（本文用 `args`）进入骨架 A.7 封闭清单。**已收编（M2 收编）**：骨架 A.7「动词键」行已补 invoke 子键 `action`、`args`；03 §1.3 同步定名。
7. feature id 四枚：`pointlock.playwright.domSnapshot.v1`、`pointlock.playwright.trace.v1`、`pointlock.http.exchangeEvidence.v1`、`pointlock.cli.outputEvidence.v1`。
