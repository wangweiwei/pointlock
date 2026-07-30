# 04 · Provider 接口与 DeviceRail Provider

> 本文是 Pointlock 设计文档系列第 4 篇，骨架见 `00-architecture-spine.md`。覆盖需求产出 6（Provider interface 完整定义）与产出 7（DeviceRail provider 完整映射设计）。所有类型名、枚举值、方法名与骨架附录 A 的 Canonical Vocabulary 一致；所有 DeviceRail wire 名称已逐项对照 DeviceRail 源仓库（`protocol/schema/v1/*.schema.json`、`packages/client/src/`、`crates/android-adb/src/driver.rs`、`crates/daemon/`）核实，引用时不改拼写。

---

## 1. SPI 总则：Provider 是「能力声明 + 忠实执行」，不是「聪明的适配器」

> **SPI 进程边界（R12，引 00 §4）**：SPI 的权威形态是 **Rust trait（in-process）**；预留 stdio JSON-RPC sidecar 适配形态（v0.2，供非 Rust provider 使用），v0.1 不实现 sidecar。本文一切 TS 签名保留为**规范记法**（00 §3 类型真相源注）。

`pointlock-provider-kit` 定义的 SPI 遵循四条总则，全部 provider 实现（含 FakeProvider）受一致性测试套件强制约束：

1. **不折叠、不翻译、不美化**。`ActionOutcome` 四分终态（`succeeded | failed | cancelled | timedOut`）原样上抛；`Observation` 缺料（omission）原样上抛为类型化原因，不伪装成错误也不伪装成空成功。判定权在 runner（原则 3/4）。
2. **不即兴、不重试、不降级**。Provider 收到什么 `BoundActionCall` 就执行什么：不自行重试（会破坏 `callId` 与 `actionIntent` WAL 的一一对应）、不自行换 action、不自行放宽超时。基座内部降级（DeviceRail `coordinateFallback`）无法阻止，但必须在 `ActionResult.execution` 中如实上报，由 runner 按 `acceptExecutionModes` 裁决（骨架 §6.4 R-degrade）。
3. **声明先于执行**。编译器只消费静态 `ProviderManifest` + `CapabilityLockfile`；运行期 `openSession` 内完成 attestation，比对失败即 `capability_drift`，拒跑（原则 5）。
4. **一个 ProviderSession 独占一条底层连接**。DeviceRail 的 `devices.list` / `device.select` 是 connection-local，因此 ProviderSession 与 client 实例一一对应，绝不共享、绝不池化。

SPI 的完整类型骨架（`Provider` / `ProviderSession` / `ProviderManifest` / `CapabilityLockfile` / `CapabilityAttestation` / `BoundActionCall` / `ActionOutcome` / `ActionResult` / `ReconcileResult` / `ErrorInfo`）以骨架 §4 为准，本文不重复粘贴，只补齐骨架未展开的契约细则：生命周期时序（§2）、各方法的前置/后置条件与错误契约（§3–§5）、错误归一规则（§6）、超时与取消（§7）。

本文补充一个 SPI 辅助类型（**已收编**——骨架 §4.2 / A.3 / A.4，M1 收编）：

```ts
/** provider 方法抛出的统一错误载体：wire 事实 + 归一结论并置，两边都进 RunLog */
export class ProviderError extends Error {
  readonly errorClass: ErrorClass;          // 骨架 §5 封闭枚举
  readonly wire?: ErrorInfo;                // daemon 原始 { code, message, retryable, details? }
  readonly clientCode?: string;             // 客户端错误码（如 "transport_closed"；TS 参考实现 @devicerail/client 的 ClientErrorCode，Rust 侧以 devicerail-client 落地为准）
  readonly retryableSource: "daemon" | "classifier"; // retryable 判断出处（§6.3 审计要求）
}
```

注意：`execute()` 的四分终态**不通过 ProviderError 表达**——`failed | cancelled | timedOut` 是正常返回值（世界发生了一件确定的事）；ProviderError 只用于「调用本身没能得到终态」的情形（transport 断裂、握手失败、协议违例、信封超时）。这条区分是 runner 状态机 `settling` 与 `onError` 两条路径的分水岭。

---

## 2. 生命周期契约

### 2.1 状态机

```
            openSession(opts)
Provider ────────────────────► ProviderSession[active]
                                   │  execute / observe / uiSnapshot / fetchEvidence
                                   │  reconcile / recordVerdict / currentCursor / health
                                   │
                    ┌──────────────┼──────────────────┐
                    ▼              ▼                  ▼
             end(outcome)   transport 断裂       进程崩溃
                    │              │                  │
                    ▼              ▼                  ▼
                [ended]       [broken]           （无状态可言）
                              runner 收到 transport_lost，
                              走 suspend → 新 openSession（session lineage）
```

契约条款：

- **openSession 是原子的**：返回即意味着（a）底层连接建立；（b）协议协商完成且 `requiredFeatures` 全部满足；（c）设备已选中并连接；（d）基座会话已开启；（e）attestation 与 `lockfileDigest` 比对通过。任何一步失败，openSession 必须清理已建立的部分资源后抛 ProviderError，**不得返回半开 session**。失败分类：协商/能力不符 → `capability_drift`；设备不可用 → `action_failed_retryable`（daemon 申报 `device_unavailable` retryable=true）；连接失败 → `transport_lost`。
- **`end(outcome, reason?)` 幂等**：对已 ended/broken 的 session 调用 `end` 是 no-op。`end` 尽力而为（best-effort）：transport 已断时不得抛错阻塞 runner 的收尾。
- **broken 状态下**除 `health()`（返回 `{ ok: false }`）外一切方法抛 `transport_lost`。
- **health()**：轻量探活，不产生副作用。DeviceRail 实现 = transport 存活检查 + `session.current` 探询；`degraded` 字段回填最近一次观察到的降级原因（如 wire 错误码 `session_degraded` 的 message）。runner 在 resume 与长等待（`awaitingHuman`）唤醒后调用。

### 2.2 与 runner 状态机的对接点

| runner 时机 | ProviderSession 方法 |
|---|---|
| run 启动 / resume | `Provider.openSession`（内含 attestation） |
| step `acting`（WAL `actionIntent` fsync 之后） | `execute(call, signal)` |
| step `observing` | `observe(...)`（+ 需要 UI 树时 `uiSnapshot(observationId)`）+ `fetchEvidence(ref)` 本地化 |
| step `judged` 且有 verdict | `recordVerdict(v)` |
| checkpoint 落盘（step 边界） | `currentCursor()` |
| resume 且 frontier 有 `pendingIntent` | `reconcile(callId, issuing)` |
| run 收尾（finished/aborted/suspend） | `end(outcome, reason?)` |

监督模式对本表零影响（R13，骨架 §6.9）：`--supervise` 命中 step 时，runner 在进入 `acting` **之前**先走 `humanRequested(purpose="supervision")` fsync → 通知 → `humanResponded(decision)`，`decision = proceed` 才写 `actionIntent` 并调用 `execute`——provider 看到的仍是「WAL `actionIntent` fsync 之后才 dispatch」的同一不变量，只是 dispatch 到达更晚。

---

## 3. 执行契约：`execute(call: BoundActionCall, signal?: AbortSignal): Promise<ActionOutcome>`

**前置条件**（violated = provider 实现 bug，一致性套件覆盖）：

- `call.callId` 是 runner 生成的 UUID，且此刻已作为 `actionIntent` 落入 RunLog（WAL 先于 dispatch，骨架 §6.2）。provider 必须把它原样用作基座的动作身份（DeviceRail：`device.execute` 的 `params.id`），**不得自行生成或改写**——它是崩溃后 `reconcile` 的唯一钥匙。
- `call.arguments` 已由 runner 按 lockfile 中该 action 的 `inputSchema` 二次校验通过。provider 不再做语义校验，但 daemon 仍可能回 `invalid_arguments`（视为编译器/表达式 bug 信号，归一为 `bind_arguments_invalid`）。
- `call.actionName` ∈ attestation 的 actions 集合 ∪ provider-synthetic actions（§10.4.3）。不在集合内 → provider 立即抛 `capability_drift`，不发 wire 请求。

**后置条件**：

- 返回值与基座终态一一对应，绝不把 `timedOut` 折成 `failed`、把 `cancelled` 折成异常。
- `outcome === "succeeded"` 时 `result.callId === call.callId`；`result.execution` 若基座报告了执行模式必须透传（含 `coordinateFallback` 的 `fallbackReason`）。
- `result.evidence`（`AssetRef[]`）与 `before`/`after` Observation 原样透传，**不在 execute 内做本地化**（本地化是 `observing` 阶段经 `fetchEvidence` 的职责，保持 execute 的时延可预算）。
- 同一 `callId` 不得被 execute 两次（重试 = 新 callId + 新 `actionIntent`，骨架 §6.5）。一致性套件断言 FakeProvider 收到重复 callId 时抛错。

**无终态路径**：transport 断裂 / 信封超时 / abort 未及送达时，execute 抛 ProviderError；runner 把该 attempt 记为「悬挂」，凭 callId 走 `reconcile`（骨架 §6.7-B），绝不臆断动作发生与否（原则 4）。

## 4. 观测与证据契约

### 4.1 `observe(req, signal?)`

`req.wants: ("screenshot" | "uiSnapshot")[]` 是**意图声明而非 wire 参数**：DeviceRail 的 `device.observe` 请求体没有参数（schema 实测：params 为空对象），daemon 按自身策略决定 Observation 内含什么。`wants` 的作用是：（a）提示 provider 是否需要在 observe 后追加 `ui.snapshot.get`；（b）让 provider 在结果缺少所需件时把 omission 原因如实回填，供断言判 unknown。

后置条件：返回的 `Observation` 保持 DeviceRail 原始结构 `{ id, deviceId, capturedAtMs, viewport, screenshot?, screenshotOmission?, uiSnapshot?, uiSnapshotOmission?, metadata? }`；omission 是数据不是错误（`evidence_unavailable` 不是 ErrorClass，骨架 §5）。

### 4.2 `uiSnapshot(observationId)`

映射 `ui.snapshot.get { observationId }`（feature `observation.uiSnapshot.v1` 门控）。协议硬限制：**仅本 Session 活跃期可读**。因此 runner 的纪律是 `observing` 阶段立即调用并把结果连同哈希写入本地 Evidence 库；resume 后绝不回头读旧 session 的快照。返回值的 `ok:false` 分支携带类型化原因（`driverUnsupported | policy | protectedAction`），传导为断言 unknown。

### 4.3 `fetchEvidence(ref: AssetRef)`

> 任务简报中的 `captureEvidence` 一词，骨架已定名为 `fetchEvidence`（A.5 穷尽清单），本文从骨架。

按 `AssetRef.uri` 拉取字节流；**若 `ref.sha256` 存在，provider 必须边读边校验，不符即抛错**（`action_failed_final`，证据完整性无商量余地）；无 sha256 时由 store 层落盘时计算并记入 `EvidenceRef`。设计理由：DeviceRail 的 session 资产只能整段删除、`ui.snapshot.get` 有活跃期限制，远端不可依赖长存——Evidence 本地化是 resume 与离线重判（judgeDirty）的物质基础（骨架 §6.6）。

## 5. verdict 写回、游标与核对

- **`recordVerdict(v)`** → `verdict.record`（feature `verdict.record.v1`）。daemon 只校验持久化，不运行断言。schema 上限 `summary ≤ 16384` 字符、`evidence ≤ 64` 条是 wire 硬限制：**provider fail-closed 拒绝超限输入**（抛 `bind_arguments_invalid`），压缩/截断是 runner 报告组装层的职责（截断时在 summary 尾部附本地完整 verdict 的内容哈希指针）。verdict 写回失败不改变 Pointlock 本地 verdict（RunLog 是唯一真相），只在报告里标注「远端存证失败」。
- **`currentCursor()`** → 返回 `{ sessionId, lastSequence }`。DeviceRail 实现：**v0.1 用 `events.list` 的最新一页推算**；事件流增强路径（§9.8 范围标注）启用后改为事件消费通道维护的最高**已处理**序列水位。注意返回的是「provider 已交付给 runner 且 runner 已 fsync 的水位」而非「daemon 已产生的水位」——ack-after-persist（§9.8.3）。
- **`reconcile(callId, issuing)`**（2026-07-18 收编评审：签发凭据 `issuing: EventCursor` 显式入参——runner 侧由 harvest 逐 intent 精确解析（FromBinding=bind 时 run 行 cursor / Known=前最近携 cursor 的 resume / Unknown=不发 RPC 直接不确定分支）；`issuing.sessionId ≠ 当前 session` 且旧日志不可达 → 一律 `logUnavailable`，**绝不扫当前 session 日志充数**——可达而错误的日志会产出假 `neverDispatched`）→ `session.current` 确认会话 + `events.list { afterSequence }` 扫描（方法由 ★ `events.snapshot.v1` 门控，已列 required 基建，§9.2）。关联键**按事件类型分两式**（schema 实测两种 payload 形状不同）：`payload.type === "actionStarted"` 匹配 `payload.call.id === callId`（payload 为 `{ type, call }`，`call` 是 `RecordedActionCall`）；`payload.type === "actionCompleted"` 匹配 `payload.callId === callId`（payload 为 `{ type, callId, outcome }`，**没有 call 对象**，终态从 `payload.outcome` 读取）。判定：
  - 找到 `actionCompleted` → `{ fate: "completed", outcome }`（M1 收编：`payload.outcome` 是四分终态，原样上抛——DeviceRail 终态落盘有 shield，`cancelled`/`timedOut` 也是有记录的终态；旧形状 `{ result: ActionResult }` 迫使非成功终态降级为 `logUnavailable`，骨架 §4.2）；
  - 找到 `actionStarted` 无 `actionCompleted` → `{ fate: "startedNoTerminal" }`（理论不应出现，按不确定分支处理）；
  - 全程无踪迹且日志完整可查 → `{ fate: "neverDispatched" }`；
  - 会话已结束/日志不可查（daemon 重启、events.clear、session 整段删除）→ `{ fate: "logUnavailable", reason }`。
  扫描起点用 `issuing.lastSequence`（该 callId 的 `actionIntent` 必然晚于此水位），避免全量拉取。
- `reconcile(callId)` 的旧单参形式随收编评审废止；`| resume 且 frontier 有 pendingIntent |` 行的调用形同步为 `reconcile(callId, issuing)`。

---

## 6. 错误契约：归一化与 retryable 语义

### 6.1 三层错误面

DeviceRail 侧有三层错误来源，归一入口各不相同：

1. **动作终态内的 `ErrorInfo`**（`ActionOutcome.failed/cancelled/timedOut.error`）——不是异常，是数据；runner 在 `settling` 读取并按 §6.2 表归类。
2. **RPC 信封错误**（`RpcRemoteError`，即 JSON-RPC error response）——execute 之外的方法（observe、verdict.record……）的失败面；也覆盖 execute 的「请求没进入动作执行」情形。
3. **客户端/transport 错误**（TS 参考实现 `@devicerail/client` 的 `DeviceRailClientError` 家族，`code: ClientErrorCode`；Rust 侧 `devicerail-client` 将定义等价错误类型，名称以 client crate 落地为准，§9.6 注）——连接层失败面。

### 6.2 归一规则（完整映射表见 §10.6）

优先级自上而下，第一条命中即定类：

| # | 判定条件 | ErrorClass |
|---|---|---|
| 1 | `FeatureNotNegotiatedError`；attestation 与 `lockfileDigest` 不符 | `capability_drift` |
| 2 | wire 码 `invalid_arguments`；本地 inputSchema 校验失败 | `bind_arguments_invalid` |
| 3 | outcome `timedOut`（wire 码 `action_timeout`） | `action_timed_out` |
| 4 | outcome `cancelled`（wire 码 `action_cancelled`）；本地 AbortSignal 触发的 `request_aborted` | `action_cancelled` |
| 5 | wire 码 `session_degraded` | `session_degraded` |
| 6 | 定位类失败：`UiNodeRef.documentEpoch` 过期、`findElement`/`waitForElement` 未命中（`waitForElementResult.matched === false` 不是错误，见下注） | `target_stale` |
| 7 | `TransportClosedError`、`protocol_violation`、NDJSON 帧错误、daemon 进程退出 | `transport_lost` |
| 8 | outcome `failed` 且 `ErrorInfo.retryable === true`（如 `device_unavailable`） | `action_failed_retryable` |
| 9 | outcome `failed` 且 `retryable === false`；一切无法归类的开放集合错误码按其 retryable 位落入 8/9 | `action_failed_final` |

注：`waitForElement` 超时未满足条件时按协议返回 `waitForElementResult { matched: false, condition }`，outcome 仍是 `succeeded`——这不是错误也不是 fail，**是观测数据**：绑定为 act-chain 一环时由 runner 判 `target_stale`（推进 attempt 链或重试），绑定为 `wait_for` 动词步时由该步断言判 fail。provider 不做任何加工。已经源码实证（DeviceRail `crates/ios-webdriver/src/appium_driver.rs` 的 `wait_for_element` 在等待窗口耗尽时返回 `Ok(WaitOutcome::NotMatched)`——成功终态而非错误）。

### 6.3 retryable 语义（契约条款）

- **信任申报，保留裁决**：`ErrorInfo.retryable` 是 daemon 的申报，Pointlock 信任它做**分类**（retryable → `action_failed_retryable`），但**是否重试由 runner 的 `RetryPolicy.retryOn` 决定**——分类是事实，重试是策略。
- **provider 永不自行重试**。包括「看起来无害」的 observe/fetchEvidence：重试意味着时间点位移，Observation 的时点语义会被污染。runner 需要重试会再次调用。
- **retryable 出处入账**：`ProviderError.retryableSource` 记录该 retryable 判断来自 daemon 申报（`"daemon"`）还是 Pointlock 分类器兜底（`"classifier"`），进 RunLog 供审计——防止分类器的保守猜测被误当成基座事实。
- `action_timed_out` 的特殊纪律（骨架 §5）：只有 `idempotent: true` 的步才允许自动重试；否则先 `reconcile`/observe 确认效果，确认不了走 unknown 路径。超时 ≠ 没发生。

## 7. 超时与取消

### 7.1 AbortSignal 贯穿

SPI 中一切可能长时间运行的方法（`execute`、`observe`）接受 `AbortSignal`。契约：

- signal abort 时 provider 必须**尽快向基座传达取消意图**，而不是仅本地放弃等待。DeviceRail：`request.cancel { requestId }`（feature `request.control.v1` 门控——本 provider 已将其列为 required 基建 feature（§9.2 ★ 行），握手成功即必然可用；TS 参考实现 `@devicerail/client` 在该 feature 协商成功时自动走此路径，`devicerail-client`（Rust）须提供等价行为）。
- 取消是**请求**不是**保证**：daemon 的 durable terminal event finalization 有 shield，取消不会留下半开 Action——最终要么 `cancelled` 终态、要么动作已完成产生 `succeeded/failed/timedOut` 终态。provider 把实际终态如实返回；若 abort 后连终态都拿不到（transport 同时断裂），抛 ProviderError，runner 走 reconcile。
- signal 已 aborted 时调用 → 立即抛 `action_cancelled` 类 ProviderError，不发 wire 请求（此时 WAL 意图已写，reconcile 会得到 `neverDispatched`，安全）。

### 7.2 谁持有哪个预算

三级预算，所有权分明（数值推导见 §10.7）：

| 预算 | 所有者 | 载体 | 到期语义 |
|---|---|---|---|
| step `timeoutMs` | runner | 状态机 watchdog → AbortSignal | 整步流水线（preflight→act→observe→assert）预算；到期 abort 当前 provider 调用，step 走 `action_timed_out`/unknown 路径 |
| 请求信封 `timeoutMs` | provider | JSON-RPC 请求顶层字段（`RequestTimeoutMs`，正整数 ms）——**仅** `device.connect/disconnect/capabilities/observe/execute` 与 `media.stream.capture` 接受，且由 `request.control.v1` 门控（本 provider required 基建，§9.2）；其余方法不接受该字段（§9.7 末段） | daemon 侧 request-scoped device-operation 预算；到期得 RPC 信封错误，**动作是否已执行不确定** → reconcile |
| `actionTimeoutMs` | runner（经 `BoundActionCall`） | `device.execute` params 字段 | 仅约束 Driver 动作本体；到期得**确定的** `timedOut` 终态（有落盘记录） |

设计决策：**runner 只显式管理两端（step 预算、action 预算），信封预算由 provider 从 action 预算推导**——信封超时的结果是不确定性，而 action 超时是确定终态；把信封预算设得比 action 预算宽裕就能把绝大多数超时收敛到「确定」一侧。这是把不确定性最小化的杠杆。

---

## 8. 一致性测试套件与 FakeProvider

`pointlock-provider-kit` 随 crate 发布 conformance suite，任何 provider 实现必须全绿才能被 CLI 装配。核心断言组：

- **终态忠实**：四分终态原样上抛；omission 不抛错；`waitForElementResult.matched:false` 不加工。
- **callId 纪律**：`params.id === call.callId`；拒绝重复 callId；reconcile 能按 callId 找回终态。
- **fail-closed**：未 attested 的 actionName 拒发；attestation 不符拒开 session；oversized verdict 拒写。
- **取消语义**：abort 后要么终态要么 ProviderError，绝不静默成功；pre-aborted signal 不发请求。
- **无自主重试**：注入一次 retryable 失败，断言 wire 上恰好一次请求。

FakeProvider 是套件的参照实现（内存世界模型 + 可编程故障注入），同时供 runner 单测使用。

（M1 收编）装配裁决：v0.1 provider 名恒为 `devicerail`；测试用 FakeProvider 经装配层以该名注册（manifest 名与注册名分离是装配层职责）。

---

## 9. DeviceRail Provider：`pointlock-provider-devicerail`

以下各节把 SPI 的每个契约点落到 DeviceRail Protocol 1.5 的具体 wire 行为。

**客户端绑定（R12）**：本 crate 依赖新的 `devicerail-client` Rust crate——在 DeviceRail 仓库实现、经 path/git 依赖消费（DeviceRail workspace 全部 `publish=false`），**待实现，是 M1 的前置依赖**（M1 估算 +1~2 周）。`@devicerail/client`（TS：`DeviceRailClient`、`DeviceRailEventStream` 等）、`@devicerail/protocol`（类型 only）与 python-client 定位降级为**协议稳定性佐证与参考实现**。本文凡引用 TS 客户端行为与类型名（spawn 管道装配、stderr tail、自动 `request.cancel`、错误类等），均为「参考实现已验证该协议行为可落地」的佐证；`devicerail-client` 须提供等价行为，具体 API 与错误类型名以该 crate 落地为准（骨架 §4.2 / A.8）。

**响应校验（R12）**：协议 DTO 走 serde 严格反序列化（wire schema 的 `additionalProperties: false` 语义在类型层兑现）；动态 schema（如 `ActionDefinition.inputSchema` 的实参校验）走 jsonschema crate（Draft 2020-12）。

### 9.1 进程管理

`OpenSessionOptions.endpoint` 两种形态：

```ts
type DeviceRailEndpoint =
  | { spawn: SpawnSpec }    // Pointlock 拥有 daemon 生命周期（默认、推荐）
  | { attach: AttachSpec }; // 连接既有 daemon（调试/共享设备场景）

interface SpawnSpec {
  command: string;          // 默认 "devicerail-daemon"（PATH 解析）
  args?: string[];          // 默认 []：daemon 无参启动即 stdio NDJSON 服务
  env?: Record<string, string>;
  cwd?: string;
  shutdownGraceMs?: number; // 默认 5000：见下方退出协议
}

interface AttachSpec {
  transport: "stdio-pipe" | "socket";  // v0.1 仅覆盖 client 支持的 transport 形态
  // ... 连接参数以 devicerail-client（Rust）的对应 transport 类型落地为准（TS 参考实现：@devicerail/client 的 ClientTransport）
}
```

落地要点（全部有 client/daemon 源码支撑）：

- **spawn 路径**：`DeviceRailClient.spawn({ command, args, ... })`——client 负责 child process 的 stdio NDJSON 管道装配与 stderr tail（`stderr-tail.ts`，daemon 诊断输出进 Pointlock 日志，不进协议流）。
- **退出协议**（spawn 模式，provider 拥有进程）：`end(outcome)` 成功返回后 → `device.disconnect` → 关闭 stdin（daemon 检测 EOF 自然退出）→ 等 `shutdownGraceMs` → 仍存活则 SIGTERM → 再宽限 → SIGKILL。attach 模式绝不杀进程，只断连接。
- **daemon 意外退出**：client 抛 `TransportClosedError` → 归一 `transport_lost` → runner run 级 suspend → 重新 `openSession`（新 spawn、新 DeviceRail session，旧 sessionId 入 `sessionLineage`）→ 从 checkpoint resume（骨架 §6.7）。**provider 不做自动重连**——重连即新会话，新会话必须走完整 attestation，这是 runner 的编排职责。
- **崩溃遗留**：spawn 模式下 Pointlock 进程自身崩溃时 child 随之收到管道关闭；resume 时一律重新 spawn，不找旧进程。daemon 的事件日志随进程消亡（stream epoch 语义即为此设计，见 §9.8），这正是 Evidence 必须本地化的原因之一。

### 9.2 `system.hello` 协商与 feature 门控

openSession 的第一步。请求参数（schema：`hello-params`，required: `client`, `protocol`）：

```jsonc
{
  "client":   { "name": "pointlock-provider-devicerail", "version": "<crate version>" },  // PeerInfo
  "protocol": { "ranges": [{ "major": 1, "minMinor": 5, "maxMinor": 5 }] },            // ProtocolOffer：显式区间数组（schema 实测：required: ranges，additionalProperties:false）
  "features": {
    "required": [ /* = FlowIR.requiredFeatures 全量 ∪ provider 基建三件（下表 ★ 行）： */
                  "device.routing.v1", "events.snapshot.v1", "request.control.v1" ],
    "optional": [ "events.stream.v1", "media.stream.v1", "session.export.page.v1" ]
  }
}
```

`protocol` 位是 `ProtocolOffer { ranges: ProtocolRange[] }`，**不是** `{ major, minor }` 对——那是响应侧 `protocol.selected` 的形状（`{ "selected": { "major": 1, "minor": 5 } }`）。Pointlock 依赖的语义 action / verdict / uiSnapshot 契约在 Protocol 1.5 引入，故 `minMinor` 取 5；若后续验证低 minor 可用可放宽 `minMinor`。

- **required 白送强制力**：协议语义保证 required 不满足则握手失败（`required_feature_unsupported`）。`FeatureOffer.required` = IR 的 `requiredFeatures` 全量（编译期承诺在握手瞬间获得运行期强制，骨架 §4.1）∪ **provider 基建 required 三件**（下表 ★ 行——它们与 flow 内容无关，是本 provider 执行模型本身的先决条件：openSession 的设备路由、reconcile 的事件日志、超时/取消的信封控制；并集口径待骨架 §4.1 措辞收编）。
- **optional 是运行质量增强**，缺了只影响 provider 内部策略，不影响正确性。

| feature | 门控的 wire 面 | offer 档位与缺失时行为 |
|---|---|---|
| ★ `device.routing.v1` | **`devices.list` 与 `device.select` 方法本身**（协议 1.2 引入；未协商 → `method_not_found`，client 侧 `FeatureNotNegotiatedError`） | **required（基建）**：§9.3 openSession 的 devices.list → device.select 步骤依赖它。v0.1 不消费的是**多设备并发路由**，不是这两个方法。协议另有单设备 legacy lazy routing（不选设备、仅恰好注册一台设备时可路由），本 provider 不采用——`opts.deviceId` 是显式契约，隐式路由与 attestation 的确定性相悖 |
| ★ `events.snapshot.v1` | `events.list` `events.clear` `session.export` `sessions.list`（未协商 → `method_not_found`） | **required（基建）**：§5 reconcile 与 §9.9 崩溃自检以 `events.list` 为原料，缺失则 effectively-once 语义落空；`session.export.page.v1` 也建立在其上（已收编骨架 A.8 feature 清单） |
| ★ `request.control.v1` | `request.cancel`，**以及**五个 device 方法（`device.connect/disconnect/capabilities/observe/execute`）+ `media.stream.capture` 的请求信封 `timeoutMs`（未协商发送 → `feature_not_negotiated`） | **required（基建）**：§9.7 预算不变量 1/2 依赖信封超时与 `request.cancel`，缺失则「超时收敛到确定终态」策略整级失效 |
| `device.semanticActions.v1` | 五件套 `findElement` `tapElement` `clearElement` `setElementValue` `waitForElement` | 随 IR `requiredFeatures` 进 required（flow 用到才要求）；编译期已按 lockfile 拒绝；运行期兜底：client 侧 `FeatureNotNegotiatedError` → `capability_drift` |
| `observation.uiSnapshot.v1` | `ui.snapshot.get` | 随 IR 进 required。`uiSnapshot()` 直接返回 `{ ok:false, reason:"driverUnsupported" }` 语义等价体？——**否**，fail-closed：抛 `capability_drift`（编译期已保证不会走到；走到即漂移） |
| `verdict.record.v1` | `verdict.record` | 随 IR 进 required。`recordVerdict` 抛 `capability_drift`（IR 有 verdict 的 flow 编译期已将其列入 requiredFeatures） |
| `events.stream.v1` | `events.stream.open` + 两个通知 | optional。**v0.1 协商但不消费**（§9.8 范围标注）：事件消费统一走 `events.list` 拉取；流消费增强落地后，缺失时降级为 `events.list` 轮询（§9.8.4），功能等价、时延更差 |
| `media.stream.v1` | `media.stream.start/capture/end` | optional。缺失时 `screenshot` 动词仅剩 Observation.screenshot 路径；media 流式取证不可用（v0.1 本就不消费，非目标 #17） |
| `session.export.page.v1` | `session.export` 分页参数 `{ limit, afterSequence }` | optional。缺失时 `session.export` 整段导出（该方法本身由 ★ `events.snapshot.v1` 门控，已在 required 基建内） |
| `action.protected.v1` | protected action 的能力暴露与执行 | optional，v0.1 不 offer 不消费（protected 在 bind 阶段拒绝，R6） |

（M1 收编）feature 握手耦合（daemon 实测）：`enabled` 含 `device.semanticActions.v1` 而无 `observation.uiSnapshot.v1` 时握手失败（`semantic_snapshot_dependency_unsatisfied`）；编译器归集 `requiredFeatures` 时必须捆绑两者（骨架 §8 bind 行同规则）。

- **attestation**：握手后 `device.capabilities` 取 `ActionDefinition[]`，连同 `FeatureSelection.enabled`、`protocolSelected`、server `PeerInfo` 构成 `CapabilityAttestation`，与 `OpenSessionOptions.lockfileDigest` 规范形比对。不一致 → `capability_drift`，**报告中列出 diff**（多了什么、少了什么、schema 变了什么），指引用户 `pointlock lock` 重固化或重编。

### 9.3 Pointlock run 与 DeviceRail session 的生命周期对应

**基数关系**：一次 run 在任一时刻至多绑定一条活跃 DeviceRail session；suspend/崩溃/transport 断裂后 resume 一律 `session.start` 开新会话，历代 sessionId 记入 `CheckpointView.binding.sessionLineage`。一条 session 绝不跨 run 复用。

openSession 完整 wire 时序：

```
DeviceRailClient.spawn / attach
  → system.hello        （§9.2 协商；requiredFeatures 强制）
  → devices.list        （校验 opts.deviceId 存在且可用）
  → device.select { deviceId }
  → device.connect
  → device.capabilities （→ attestation 比对，不符即拒）
  → session.start       （→ sessionId）
  →（可选增强，v0.1 不启用）events.stream.open { sessionId, originPolicy: { kind: "absent" } }
                        （仅 events.stream.v1 协商成功且事件流增强路径启用时才调用，见 §9.8 范围标注；
                          Node 客户端 originPolicy 必须 absent。v0.1 事件消费统一走 events.list 拉取）
```

Pointlock run 事件 ↔ wire 调用对应表：

| Pointlock（RunLog 事件 / 状态） | DeviceRail wire |
|---|---|
| `runStarted` / `runResumed` | 上述 openSession 全序列 |
| `actionIntent`（WAL fsync 后进入 `acting`） | `device.execute { id: callId, name, arguments, actionTimeoutMs }` |
| `settling` 结束 → `actionSettled` | `device.execute` 响应的 `ActionOutcome`（事件流增强启用时 `actionCompleted` 可能先到，以响应为准、事件为证） |
| `observing` → `observationRecorded` | `device.observe`；需要 UI 树时 `ui.snapshot.get { observationId }`；证据本地化按 `AssetRef.uri` 拉流 |
| `verdictRecorded` | `verdict.record { verdict }` |
| checkpoint 落盘 | `currentCursor()`（本地水位，无 wire 调用） |
| `runSuspended`（有意暂停/进程退出前） | `session.end("shutdown", "pointlock run suspended")` |
| `runFinished`（无论 flow verdict 是 pass/fail/unknown） | `session.end("completed")` |
| run aborted（handler/用户裁决 abort；`action_cancelled`） | `session.end("cancelled", reason)` |
| run 因不可恢复错误终止（如 `capability_drift` 于中途、`session_degraded` 不可续） | `session.end("failed", reason)` |

`SessionOutcome` 四值（`completed | failed | cancelled | shutdown`，schema 实测）的裁决原则：**outcome 描述「这次会话如何收场」，不描述「测试结果」**——flow verdict 是 fail 的 run 依然 `completed`（会话善终，判定入 verdict.record）；`shutdown` 专用于「Pointlock 主动收摊、工作未完待续」的 suspend 语义；daemon 侧崩溃则没有 `session.end` 可言，lineage 里留下的是无终笔会话，reconcile 按 `logUnavailable` 处理。

### 9.4 动作转换：通用层 → wire 层

#### 9.4.1 分层回顾（谁在何时转换）

骨架 R7/A.6 已裁决：**动词→原生 action 的转换发生在编译期 bind 阶段**（消费 `ProviderManifest.verbBindings` 的声明式字段映射），IR 里只剩原生 `actionName`，runner 与 provider 均无 verb switch。因此下表的「YAML 动词 → wire action」一列描述的是**编译期绑定**，「provider 路由」一列描述的是运行期 `execute()` 对 actionName 的分发。

需求侧曾用 `app.open` / `screen.tap` / `text.input` 等通用命名意图；骨架未采纳该命名空间（A.6：动词表只收「至少两个 provider 语义一致实现」的动作，driver 专有 action 一律走 `invoke` 逃逸门）。下表以「意图」列保留需求措辞供对照，**规范写法以 YAML 动词键 / `invoke` 为准**。

#### 9.4.2 精确转换表

Android driver 已核实 action 目录：`tap` `keyPress` `swipe` `scroll` `inputText` `launch` `terminate`（protection `standard`）+ `inputSecret`（protection `protected`）；五件套由 `device.semanticActions.v1` 门控。其它平台以各自 lockfile 的 `device.actions` 为准。

| 需求意图 | 规范 YAML 写法 | wire 承载 | wire arguments 形状（实测 schema） | 门控 / protection | effect |
|---|---|---|---|---|---|
| app.open | `invoke: { action: "launch" }` | `device.execute` name=`launch` | `{ packageName: string }`（Android driver） | lockfile 有此 action | mutating |
| app.close | `invoke: { action: "terminate" }` | `device.execute` name=`terminate` | `{ packageName: string }` | 同上 | mutating |
| screen.tap（元素语义） | 动词 `tap` | `device.execute` name=`tapElement` | `{ target: ElementTarget }` | `device.semanticActions.v1` | mutating |
| screen.tap（坐标兜底） | act-chain `locate_via` 含 `coordinate` + 静态 `coordinate` | `device.execute` name=`tap` | `{ x: uint, y: uint }`（Android driver） | lockfile 有此 action；attempt 必带静态坐标（编译期强制，原则 6） | mutating |
| text.input（元素语义） | 动词 `set_value` | `device.execute` name=`setElementValue` | `{ target: ElementTarget, value: string }` | `device.semanticActions.v1` | mutating |
| text.input（无目标全局输入） | `invoke: { action: "inputText" }` | `device.execute` name=`inputText` | `{ text: string }`（Android driver） | lockfile 有此 action | mutating |
| text.secret | **v0.1 编译拒绝**（bind 阶段 fail-closed，R6）；v0.2 路径：`invoke: { action: "inputSecret", protected: true }` + `secrets.*` 句柄 | `device.execute` name=`inputSecret` | `{ secret: string }`（Android driver；事件/日志侧 daemon 自动脱敏） | `action.protected.v1` + protection=`protected` | mutating |
| 清空输入 | 动词 `clear` | `device.execute` name=`clearElement` | `{ target: ElementTarget }` | `device.semanticActions.v1` | mutating |
| element.find | 动词 `find` | `device.execute` name=`findElement` | `{ selector: ElementSelector }` | `device.semanticActions.v1` | readonly；output=`findElementResult { element: UiNodeRef }` |
| element.wait | 动词 `wait_for` | `device.execute` name=`waitForElement` | `{ selector: ElementSelector, condition?: "present"\|"visible"\|"enabled"\|"absent" }` | `device.semanticActions.v1` | readonly；output=`waitForElementResult { matched, condition, element? }` |
| observe | 动词 `observe` | `device.observe`（无参）＋按需 `ui.snapshot.get { observationId }` | —（Observation 内容由 daemon 策略决定，omission 类型化） | uiSnapshot 部分需 `observation.uiSnapshot.v1` | readonly（provider-synthetic，§9.4.3） |
| evidence.capture / screenshot | 动词 `screenshot` | `device.observe` → 投影 `Observation.screenshot`（`AssetRef`）；连续取证走 `media.stream.start { streamId, kind } / media.stream.capture { streamId, frameIndex, durationMs? } / media.stream.end` | `AssetRef { id, mediaType, uri, sha256? }` 原样透传，经 `fetchEvidence` 本地化为 `EvidenceRef` | media 路径需 `media.stream.v1` | readonly（provider-synthetic） |
| verdict 写回 | （非 step；runner 判定后自动） | `verdict.record { verdict: { status, summary, evidence } }` | `VerdictStatus: pass\|fail\|unknown`；`summary ≤ 16384`；`evidence ≤ 64` | `verdict.record.v1` | — |
| 其它 driver 专有（`keyPress` `swipe` `scroll`、mock/desktop driver 的 `tap` `waitForIdle` 等） | `invoke: { action: "<原生名>" }` | `device.execute` 原样 | 按 lockfile 内该 action 的 `inputSchema` | lockfile 有此 action | 作者以 `effect` 键声明 |

`invoke` 的纪律（编译期，复述以防漂移）：actionName 必须 ∈ `lockfile.device.actions`；实参按其 `inputSchema` 静态校验；output 无声明即 `unknown` 类型，下游取字段必须 `expect_schema` 收窄；protection=`protected` 的 action v0.1 一律拒绝。

#### 9.4.3 provider-synthetic readonly actions（设计决策，待骨架收编）

`observe` 与 `screenshot` 两个动词没有对应的 driver action——它们映射到 RPC 方法 `device.observe`（及附属调用）。为维持骨架「IR 只有原生 actionName、runner 无 verb switch」的不变量，本文决定：

- `ProviderManifest.knownActions` 中声明两个 **provider-synthetic** 条目：`observe` 与 `screenshot`（protection `standard`，effect readonly，outputSchema 分别为 Observation 元数据投影与 `{ screenshot: AssetRef }`）。
- 适配器 `execute()` 内部路由：actionName ∈ {`observe`, `screenshot`} → `device.observe`（+ `ui.snapshot.get` / `media.stream.*`）；其余 → `device.execute`。对 runner 而言仍是统一的 `execute` 契约，四分终态语义不变（RPC 失败按 §6 归一）。
- **遮蔽规则**：若某设备的 `lockfile.device.actions` 出现同名 driver action（`observe`/`screenshot`），lockfile 优先、synthetic 条目对该 lockfile 禁用；此时这两个动词在 bind 阶段直接绑到 driver action。歧义即编译错，不静默二选一。
- synthetic actions 是 readonly，天然免除 reconcile 关切（reconcile 只为 mutating 悬挂意图服务；readonly 悬挂一律安全重放，骨架 §6.7-B）。

### 9.5 选择器映射：`ElementSelectorIR` ↔ wire `ElementSelector` / `ElementTarget`

`ElementSelectorIR` 与 wire `ElementSelector` **字段级同构**（骨架 §3 注释已声明），provider 零转换透传。wire schema 实测（`element-selector.schema.json`，`additionalProperties: false`）：

| 字段 | wire 约束 | 编译期附加规则 |
|---|---|---|
| `context` | `UiContextSelector { contextKind: "native"\|"web", contextId? }`——**只选上下文，不携带 documentEpoch**（schema 注释原话：context selection never carries a stale document epoch） | 缺省 = daemon 当前上下文 |
| `role` | string ≤ 256 | — |
| `name` | string ≤ 65536 | — |
| `identifier` | string ≤ 4096 | — |
| `text` | `TextMatch { value(1..65536), mode: "exact"\|"contains"(默认 exact), caseSensitive? }` | — |
| `value` | string ≤ 65536 | — |
| `css` | string ≤ 65536 | **仅 `context.contextKind === "web"` 时允许**（协议语义：native 用 accessibility 字段，web 可加 CSS）；违者编译错 |

长度上限全部在编译期 bind 阶段校验（能在离线拒绝的绝不留给运行期 `invalid_arguments`）。

**`ElementTarget`**（tapElement/setElementValue/clearElement 的 `target` 位，oneOf）：

```jsonc
{ "kind": "selector", "selector": ElementSelector }   // 常规：每次动作即时解析
{ "kind": "node",     "node": UiNodeRef }             // 链式：复用先前 findElement 的产物
```

编译器缺省产出 `kind: "selector"`。`kind: "node"` 仅当作者显式引用前步 `find` 的 output（`${{ steps.<id>.output.element }}`）时产出，且受 documentEpoch 时效纪律约束：`UiNodeRef { observationId, context: { contextKind, contextId, documentEpoch }, stableNodeId }` 绑定 epoch，跨导航/重连/resume 的引用在编译期被注入 revalidate（`findElement` 重定位）step（骨架 §6.7）；运行期 epoch 过期由 daemon 拒绝，归一 `target_stale`，重试前强制重新 observe/find。

act-chain 与 verify-chain 的通道对应：`dom` 通道 = `context.contextKind:"web"`（可用 css）；`uiTree` 通道 = `contextKind:"native"`（accessibility 字段）；`coordinate` 通道 = driver 坐标 action（如 Android `tap {x,y}`）+ 静态坐标；`vision` 永不出现在定位（类型层封死，原则 7）。

### 9.6 错误码映射表（完整）

DeviceRail 错误面（§6.1 三层）→ Pointlock `ErrorClass` 的穷尽映射。wire 错误码是开放集合，下表列出全部已核实项 + 兜底规则：

| 来源层 | wire 码 / 客户端错误 | ErrorClass | retryable 处理 |
|---|---|---|---|
| ActionOutcome | `action_timeout`（outcome `timedOut`） | `action_timed_out` | 仅 `idempotent: true` 自动重试（新 callId）；否则 reconcile/observe 确认，不能确认 → unknown 路径 |
| ActionOutcome | `action_cancelled`（outcome `cancelled`） | `action_cancelled` | 不重试；runner 主动取消 → run aborted → `session.end("cancelled")` |
| ActionOutcome | `device_unavailable`（retryable=true） | `action_failed_retryable` | 命中 `retryOn` 按 backoff 重试（新 callId、新 `actionIntent`） |
| ActionOutcome | `invalid_arguments`（retryable=false） | `bind_arguments_invalid` | 不重试；step fail + 编译器/表达式 bug 信号（本地 schema 校验本应先拦住） |
| ActionOutcome / RPC | `session_degraded` | `session_degraded` | 当前 step → unknown；触发 flow 级 `onError` handler |
| ActionOutcome | 其它开放集合码，retryable=true | `action_failed_retryable` | 按 `retryOn` 策略 |
| ActionOutcome | 其它开放集合码，retryable=false | `action_failed_final` | 不重试；attempt 链有后继则推进，否则 step fail |
| 事件流（增强路径，v0.1 不启用，§9.8 范围标注） | `stream_slow_consumer`（`events.stream.terminal` 终止） | **不映射 ErrorClass** | provider 内部：重开 stream 续传；反复发生 → 降级 `events.list` 轮询（§9.8.4）。绝不因取证通道拥塞而挂掉 step |
| 客户端 | `TransportClosedError`（code `transport_closed`）；daemon 进程退出 | `transport_lost` | run 级 suspend → 新 openSession（lineage）→ resume |
| 客户端 | `protocol_violation`、`invalid_ndjson_utf8`、`incomplete_ndjson_frame`、`ndjson_frame_too_large`、`write_frame_too_large` | `transport_lost` | 连接已不可信（协议毒化），同上；报告附诊断 |
| 客户端 | `FeatureNotNegotiatedError`（code `feature_not_negotiated`） | `capability_drift` | 拒绝启动/恢复；指引重 lock 或重编 |
| 客户端 | `request_aborted`（本地 AbortSignal 触发） | `action_cancelled` | runner 自己发起的取消；abort 后走 reconcile 确认实际终态 |
| 客户端 | `RpcRemoteError`（code `remote_rpc_error`，信封层错误响应） | 按其内嵌 error 再分类；无法归类且请求可能已达 → 视同悬挂：reconcile 后按 fate 定 | — |
| 客户端 | `handshake_state`、`duplicate_request_id` | provider 实现 bug：抛不可恢复 ProviderError（`action_failed_final`），一致性套件层面消灭 | 不重试 |
| 客户端 | `pending_request_limit`、`write_queue_overflow` | provider 内部背压，排队消化，**不上抛 runner**；持续溢出 → `transport_lost` | — |
| 客户端 | `event_stream_aborted/closed/cursor/queue_overflow/remote_termination` | 不映射；事件流通道内部处理（§9.8） | — |
| attestation | digest 不符 / action schema 漂移 | `capability_drift` | 拒跑 |

（wire 已核实错误码全集见骨架 A.8：`action_timeout` `action_cancelled` `device_unavailable` `invalid_arguments` `session_degraded` `stream_slow_consumer`；引用仅限已核实项，开放集合走兜底行。）

> **R12 注**：本表「客户端」行的错误类名（`TransportClosedError` / `FeatureNotNegotiatedError` / `RpcRemoteError` 及 `ClientErrorCode` 字符串码）保留为 DeviceRail 生态事实（TS 客户端 `@devicerail/client`）；Rust 侧 `devicerail-client` 将定义等价错误类型，名称以 client crate 落地为准。本表按**错误语义**（而非类名字面）映射，Rust 客户端落地后仅替换类名列、不改 ErrorClass 归一结论。

### 9.7 两级（实为三级）超时预算

wire 事实（`device-execute-params` / `device-execute-request` schema 注释原话）：action 字段刻意平铺在 params 上；**可选 `actionTimeoutMs` 只控制 Driver 动作，请求信封 `timeoutMs` 控制 request-scoped device-operation 预算**；终态落盘有 shield。

预算推导链（自外向内，逐级收窄）：

```
step.timeoutMs                     (IR，作者声明；缺省 flow 级默认，如 120_000)
  ≥ Σ(该步各 phase 预算)
      act 段预算 A = step 剩余预算 − observe/assert 预留 R (缺省 R = 10_000)
        actionTimeoutMs   = min(A − E, 作者经 timeout_ms 显式给 act 的值)
        请求信封 timeoutMs = actionTimeoutMs + E      (信封裕度 E 缺省 5_000)
```

三条不变量（provider 装配时断言，违反即 `bind_arguments_invalid`）：

1. `actionTimeoutMs < 信封 timeoutMs`——保证「动作超时」先于「信封超时」发生，把超时收敛到**确定终态**（`timedOut` 有落盘记录）而非**不确定的信封错误**（需 reconcile）。裕度 E 吸收 daemon 调度与终态落盘时间。
2. `信封 timeoutMs ≤ step 剩余预算`——step watchdog 永远最后触发；触发时走 AbortSignal → `request.cancel`（`request.control.v1`），得到的仍是确定的 `cancelled` 终态。
3. 两个 wire 字段均为 `RequestTimeoutMs`（正整数，≤ 2^53−1）；provider 对推导结果向下取整并夹紧。

超时事件的三种结局与处置对照：

| 谁先到期 | wire 表现 | Pointlock 处置 |
|---|---|---|
| `actionTimeoutMs` | 终态 `timedOut`，`ErrorInfo.code = "action_timeout"`，已落盘 | `action_timed_out`：幂等步可重试，否则 reconcile/observe 确认 → unknown 路径 |
| 信封 `timeoutMs` | RPC 信封错误（`RpcRemoteError`），动作是否执行**不确定** | 视同悬挂意图：`reconcile(callId)` 定 fate |
| step `timeoutMs`（watchdog） | AbortSignal → `request.cancel { requestId }` → 终态 `cancelled`（shield 保证不留半开） | `action_cancelled`（内部超时起因记入 StepRecord）；step 走 unknown/fail 由 handler 策略定 |

`observe` 走 `device.observe`，属于可带信封 `timeoutMs` 的五个 device 方法之一，与 execute 同享信封一级。**除五方法与 `media.stream.capture` 之外的请求（`ui.snapshot.get` / `verdict.record` / `session.*` / `events.list` / `devices.list` / `device.select` 等）没有信封超时一级**——schema 实测这些方法的请求顶层均无 `timeoutMs` 字段；带上即被拒（daemon 侧 `request_timeout_not_supported`，client 侧先一步抛 ProtocolViolationError）。这些方法的固定缺省（15_000）因此只能实现为 **provider 本地计时**：到期本地放弃等待、按 §6 归一处置（响应可能仍在途，视同悬挂面：mutating 语义的 `verdict.record` 失败按 §5「远端存证失败」条款处理，其余按各方法契约），不上 wire；并同样受 step watchdog 覆盖。

### 9.8 事件流消费与 checkpoint 游标

> **范围标注（与 08 篇 M1 边界及非目标 #17 对齐）**：本节的 `events.stream` 全套流消费（§9.8.1 订阅机制、§9.8.4 降级与背压）是 **optional 增强路径的前置设计，v0.1（M1/M2）不实现**——v0.1 事件消费统一用 `events.list { afterSequence }` 拉取（openSession 后不调 `events.stream.open`），`currentCursor()` 按 §5 用 `events.list` 最新一页推算。该增强随非目标 #17 的重估（M3 及以后）一并落地。§9.8.2 的关联键与 §9.8.3 的 ack-after-persist 水位纪律**对拉取与流式两种消费方式同样适用，v0.1 即生效**。事件通道对正确性零贡献的定位不因实现方式改变：驱动始终是 RPC 响应 + RunLog。

#### 9.8.1 wire 机制（实测；增强路径，v0.1 不启用）

- `events.stream.open { sessionId, originPolicy }` → `{ endpoint, streamEpoch, expiresAtMs }`。`endpoint` 是短时效 bearer URL；Node 客户端 `originPolicy` 必须 `{ kind: "absent" }`。
- 流上通知：`events.stream.event`（事件信封 `{ eventId, sessionId, sequence, requestId, deviceId, atMs, payload }`）与 `events.stream.terminal`（类型化终止，如 `stream_slow_consumer`）。
- 续传游标 `EventStreamCursor { streamEpoch, sessionId, sequence }`。**`streamEpoch` 标识一个 daemon 进程生命期；跨 epoch 的 cursor 绝不可作为续传位置**（schema 注释原话）。`sequence` 是 session 内一基单调序列。
- 补拉路径：`events.list { afterSequence }`（session 事件日志 append-only、只能整段删除）。

#### 9.8.2 事件 ↔ Pointlock step 的关联

DeviceRail 事件对 Pointlock 是**佐证流**而非驱动流（驱动始终是 RPC 响应），消费目的有三：reconcile 的原料、Evidence 侧写、报告时间线。关联键：

| 事件 payload 类型 | 关联键 | Pointlock 用途 |
|---|---|---|
| `actionStarted` | `payload.call.id === callId`（payload 为 `{ type, call }`；= RunLog `actionIntent.callId`） | reconcile 判 fate；attempt 时间线 |
| `actionCompleted` | `payload.callId === callId`（payload 为 `{ type, callId, outcome }`，**无 call 对象**；终态读 `payload.outcome`） | reconcile 判 fate；RPC 响应丢失时的终态旁证 |
| `observationCaptured` | observationId ↔ StepRecord.observations | Observation 产生的旁证与时间戳 |
| `verdictRecorded` | verdict 内容哈希 | `recordVerdict` 持久化的回执确认 |
| `sessionStarted` / `sessionEnded` | sessionId ∈ sessionLineage | lineage 审计 |
| `mediaStreamStarted` / `mediaFrameCaptured` / `mediaStreamEnded` | streamId | media 取证的帧账本 |
| `error` | requestId / sessionId | 诊断附件 |

事件消费循环把每个已消费事件的 `sequence` 连同其 Pointlock 侧派生记录（若有）写入 store，随后才推进水位（下一节）。

#### 9.8.3 sequence 入 checkpoint（ack-after-persist）

`CheckpointView.binding.eventCursor = { sessionId, lastSequence }`（骨架 §6.6 字段，逐字）。推进规则：

- **`lastSequence` 只在「该序列及之前全部事件的 Pointlock 派生记录已 fsync」后推进**——消费-先持久-后确认。崩溃后最坏重复消费（事件处理设计为幂等：按 `eventId` 去重），绝不丢失。
- **`streamEpoch` 刻意不进 checkpoint**：checkpoint 的寿命跨越 daemon 进程，而 epoch 不跨进程。resume 一律开新 session（骨架 §6.7 session 断代），新 session 的 sequence 从 1 重新起算，事件游标**跨 session 不假装连续**——旧 `{ sessionId, lastSequence }` 只用于对旧 session 的 `events.list { afterSequence }` 补扫（reconcile 场景），新 session 的游标从零建立。
- run 内断流重连（增强路径，v0.1 无此关切）：同 epoch → 用内存中的 `EventStreamCursor { streamEpoch, sessionId, sequence }` 重开续传（`DeviceRailEventStream` 封装）；epoch 变化（daemon 重启，此时 transport 也必然断过，session 已死）→ 不存在续传问题，走 resume 全流程。
- **`afterSequence` 初始水位（M1 收编）**：`EventSequence` 一基，`afterSequence: 0` 不可表达；初始水位（尚未消费任何事件）以省略该字段（None）表达，实现注意 off-by-one。

#### 9.8.4 降级与背压（增强路径，v0.1 不启用）

- `events.stream.terminal`（含 `stream_slow_consumer`）→ 以当前 cursor 重开 stream；一个 run 内第 3 次终止 → 降级 `events.list` 轮询（间隔 1_000ms，afterSequence 推进），并在报告标注取证通道降级。**事件流故障永不成为 step 错误**（它是佐证通道；正确性由 RPC 响应 + RunLog 保证）。
- `endpoint` 过期（`expiresAtMs`）→ 重新 `events.stream.open`；这是常规轮转，不算故障。

### 9.9 一个 action step 的完整 wire trace（示例）

Flow 片段（YAML 界面，编译后动词消失）：

```yaml
- id: tapLogin
  tap:
    element: { role: "button", text: { value: "登录", mode: "exact" } }
  effect: mutating
  timeout_ms: 30000
  retry: { max_attempts: 2, backoff_ms: 1000, retry_on: [target_stale] }
  expect:
    - element: { identifier: "home_banner" }
      state: visible
      verify_via: [uiTree, vision]
```

运行期时序（RunLog 事件 ⇄ wire）：

```
stepEntered(tapLogin)
resolvedInputs 快照（selector 求值）
actionIntent{ callId: "5e0c…", argsSnapshot } → fsync            ← WAL 先行
  → device.execute { id:"5e0c…", name:"tapElement",
                     arguments:{ target:{ kind:"selector", selector:{ role:"button",
                       text:{ value:"登录", mode:"exact" } } } },
                     actionTimeoutMs: 15000 }   (信封 timeoutMs: 20000)
  ← ActionOutcome succeeded; result.execution.mode = "nativeSemantic"
     （若 mode="coordinateFallback" 且不在 acceptExecutionModes：
       degraded_by_provider → 不重试、记 Evidence、强制全量验证，验证不了 → unknown）
actionSettled{ outcome:"succeeded" }
  → device.observe            ← Observation{ id:"obs-9", screenshot: AssetRef, uiSnapshot: ref }
  → ui.snapshot.get { observationId:"obs-9" }   ← UiSnapshot（立即本地化）
  → fetchEvidence(screenshot AssetRef)          ← 边拉边验 sha256 → EvidenceRef
observationRecorded / (evidence 落盘)
asserting：纯计算，verify-chain uiTree 命中 home_banner visible → pass
assertionEvaluated{ result:"pass", channel:"uiTree" }
verdictRecorded{ status:"pass", degraded:false }
  → verdict.record { verdict:{ status:"pass", summary:"…", evidence:[…] } }
stepExited(tapLogin)
（checkpoint：currentCursor() → eventCursor{ sessionId, lastSequence:47 } 入库）
```

崩溃注入点自检：若进程死在 `device.execute` dispatch 前后任意瞬间，resume 时 frontier 带 `pendingIntent{ callId:"5e0c…" }`，`reconcile` 经 `events.list { afterSequence:<checkpoint 水位> }` 双式扫描——`actionStarted` 查 `payload.call.id === "5e0c…"`，`actionCompleted` 查 `payload.callId === "5e0c…"`（§9.8.2）：有 `actionCompleted` → 采认 `payload.outcome` 终态从 `observing` 续跑；无踪迹 → 安全重放；日志不可查 → 本步非幂等 mutating → `onResumeDrift` 升级人机（骨架 §6.7-B）。

---

## 10. 工程落地清单（provider-devicerail 的验收面）

1. conformance suite 全绿（§8 断言组）。
2. `pointlock lock` 端到端：spawn daemon → hello → capabilities → lockfile 固化 → digest 稳定（同一 daemon 双跑 digest 相同）。
3. 故障注入矩阵：daemon SIGKILL（execute 中/后）、信封超时、attestation 漂移、epoch 过期 selector——每项对应本文一个契约条款，均有确定处置。`stream_slow_consumer` 注入随事件流增强路径（§9.8 范围标注，非目标 #17 重估后）启用时再加入矩阵，v0.1 验收面不含。
4. 五件套 + Android driver 8 个原生 action 的 arguments/结果 round-trip 通过 wire schema 校验（`additionalProperties: false` 意味着多一个字段都会被拒，序列化层必须精确）。
