# 06 · Human 交互子系统与交互式安全验证 Handler 家族

> 本文是 Pointlock 设计文档系列第 6 篇（文件编号 06），骨架见 `00-architecture-spine.md`；覆盖需求产出 9（Human-in-the-loop 接口）与产出 11（交互式验证 handler 家族）。凡与骨架 Canonical Vocabulary 冲突之处，以骨架为准；本文新增且骨架未收编的命名均已显式标注「待骨架收编」并汇总于 §8。

---

## 1. 定位：Human 不是 Provider，是 step kind + 通道插件

需求侧把这一块称作「Human provider」。本文第一个设计决定是：**human 交互不实现 Provider SPI**，理由有三，全部源自骨架已锁定的契约：

1. **Provider 的本质是 capability-bound 的外部执行基座**：静态 manifest → `pointlock lock` 固化 lockfile → 运行期 attestation 复核（骨架 §4.1）。人的能力无法 lockfile 化，也无法 attest——「今天值班的人会不会解滑块」不是可固化断言。把 human 塞进 Provider SPI 会迫使编译器对不可证实的能力做原则 5 的绑定，属于范畴错误。
2. **human step 的 durable 语义是 runner 状态机的固有转移**：`awaitingHuman → suspended → resume` 与 checkpoint/RunLog 深度耦合（骨架 §6.2、§6.8），不该隔着 SPI 边界实现。
3. **骨架 crate 表已经这么裁决了**：`pointlock-human-cli` 依赖 `pointlock-store` 而非 `pointlock-provider-kit`（骨架 §1.2）。

因此本文的分工是：

| 层 | 承载物 | 归属 crate（R12：均为 Rust crate，骨架 §1.2/A.1） |
|---|---|---|
| 语义层 | `HumanStepIR`（step kind `human`，四 mode） | `pointlock-ir` |
| 执行层 | `awaitingHuman` 状态、超时结算、verdict 映射 | `pointlock-runner` |
| 持久层 | `humanRequested` / `humanResponded` RunLog 事件、`CheckpointView.humanPending`、响应仲裁 API | `pointlock-store` |
| 呈现层 | 通知通道（CLI / Web UI / webhook） | `pointlock-human-cli`（Rust crate，v0.1 唯一交互实现） |

需求中的三个接口名与骨架封闭枚举 `human mode` 的对应关系（**不新增 YAML 关键字**，YAML 表面永远是 `human:` + 子键 `mode prompt presents decisions on_timeout`）：

| 需求接口名 | 骨架 mode | 一句话语义 |
|---|---|---|
| `human.confirm` | `confirm` | 是/否 + 备注：人确认一个事实或授权继续 |
| `human.input` | `provideInput` | 结构化输入表单：人提供数据，按 schema 校验后进入数据流 |
| `human.pause` | `repairWorld` | 暂停，人**直接在设备上**操作，完成后宣告 done/cannotRepair |
| （骨架第四 mode） | `judge` | 人对呈示证据做三值终审，人的判定即 verdict |

`human.pause` 映射到 `repairWorld` 而非新造 mode：二者语义完全同构——runner 挂起、人对世界施加带外操作、人宣告结果、机器复核。骨架 §6.7-B 的悬挂意图裁决（adopt / redo / abort）也复用同一 mode。

---

## 2. HumanStepIR 四 mode 的完整契约

骨架 §3 已定义 `HumanStepIR` 的形状（`mode / prompt / presents / decisions? / outputSchema? / timeoutMs / onTimeout: "unknown"`）。本节钉死每个 mode 的响应 payload、verdict 映射与 output 契约。

### 2.1 响应 payload（`humanResponded` 事件的类型化载荷）

```ts
// pointlock-store —— humanRequested / humanResponded 的物化类型（待骨架收编：HumanRequest / HumanResponse）
// TS 记法为规范记法，真相源为 pointlock-ir/pointlock-store 侧的 Rust 类型（R12，骨架 §3 引言）。
// R13：两事件 payload 均携带判别子 purpose: "step" | "supervision"（骨架 §6.1/A.4）。
// 本节其余字段按 purpose = "step"（human step 问答）描述；purpose = "supervision"（监督问答）
// 复用同一事件对与仲裁管道，语义差异见 §5.5。
export interface HumanRequest {
  requestId: string;                 // uuid，runner 生成；humanRequested 事件主键
  runId: string;
  runPath: RunPath;                  // 骨架 §9
  purpose: "step" | "supervision";   // R13 判别子（骨架 A.4 封闭枚举）
  mode?: "confirm" | "judge" | "provideInput" | "repairWorld";  // purpose="step" 时必填；purpose="supervision" 无 mode
  prompt: string;
  presents: PresentedItem[];         // ready 时物化：值 或 已本地化的 EvidenceRef
                                     // （supervision：含 runPath / actionName / resolvedInputs 摘要，§5.5）
  decisions?: string[];              // confirm / judge
  outputSchema?: unknown;            // provideInput：JSON Schema Draft 2020-12
  requestedAt: string;               // ISO 8601
  deadlineAt?: string;               // = requestedAt + timeoutMs，绝对时刻，落盘（§4.4 lazy timeout 的依据）；
                                     // purpose="step" 时必有；supervision 默认无超时，此字段缺省（§5.5）
}

export type PresentedItem =
  | { kind: "value";    label?: string; value: unknown }
  | { kind: "evidence"; label?: string; evidence: EvidenceRef };

export interface HumanResponse {
  requestId: string;
  purpose: "step" | "supervision";   // R13 判别子，与对应 HumanRequest 一致
  payload: HumanResponsePayload;     // purpose="step" 的四 mode 变体 + purpose="supervision"
                                     // 变体并入同一 union（R13 收编，原 §8-Q8① 关闭；supervision
                                     // 的 decision 封闭枚举 proceed | abort | suspend，§5.5）
  actor: HumanActor;                 // 谁（§6）
  at: string;                        // 何时（ISO 8601，store 收到时刻，超时仲裁依据）
}

export type HumanResponsePayload =
  | { mode: "confirm";      decision: "yes" | "no";                 note?: string }
  | { mode: "judge";        decision: "pass" | "fail" | "unknown";  note?: string }
  | { mode: "provideInput"; decision: "provided" | "declined";
      input?: unknown;               // decision="provided" 时必填，已过 outputSchema 校验
      note?: string }
  | { mode: "repairWorld";  decision: "done" | "cannotRepair";      note?: string }
  | { decision: "proceed" | "abort" | "suspend";  note?: string };
      // ↑ purpose="supervision" 变体（R13 收编）：无 mode，判别靠外层 purpose；落账细则见骨架 §6.9

export interface HumanActor {
  channel: "cli" | "webUi" | "webhook";   // 通道封闭枚举（§4）
  principal: string;                      // v0.1: "os:<user>@<host>"（本机 OS 身份）
}
```

### 2.2 verdict 映射与 output 契约（钉死）

| mode | 人的回应 | step verdict | `steps.<id>.output.*` |
|---|---|---|---|
| `confirm` | `yes` | `pass` | `decision`、`note` |
| `confirm` | `no` | `fail` | 同上 |
| `judge` | `pass` / `fail` / `unknown` | 原样即 verdict（骨架 §6.3：人的判定即 verdict） | `decision`、`note` |
| `provideInput` | `provided` + 合法 `input` | `pass`（含义：「人已提供且过 schema」这一事实成立） | `input` 的各字段（按 `outputSchema` 展开）、`note` |
| `provideInput` | `declined` | `fail` | `note` |
| `repairWorld` | `done` | `pass` | `decision`、`note` |
| `repairWorld` | `cannotRepair` | `fail` | 同上 |
| 任意 mode | 超时（无响应过 `deadlineAt`） | `unknown`（骨架固定 `onTimeout: "unknown"`，绝不默认 pass/fail） | 无 output；下游引用其 output 的 step 进入 `blocked` |

三条补充规则：

- **`confirm` 的 decisions 定制**：作者可自定义两个标签（如 `["批准", "驳回"]`），但必须**恰好两项**，第一项映射 `pass`、第二项映射 `fail`，编译期 `check` 阶段校验。`judge` 的 decisions 固定为 `["pass","fail","unknown"]` 的非空子集，不可换名——三值判定的词汇不容别名（防「approve 算 pass 还是 unknown」式漂移）。
- **`repairWorld` 的 pass 是证词不是观测**：`done → pass` 表示「人宣告已修复」，其 evidence 是人类证词（§6），**不证明世界状态**。因此凡使用 `repairWorld` 的标准模式必须紧跟机器复核（assert step 或下一步的 `preflight`）；编译器对「`repairWorld` 之后无任何 assert/preflight 的 flow」发 lint 警告（不拒编——存在合法例外，如纯人工验收流）。§7.5 的恢复验证就是这条规则的制度化。
- **`provideInput` 的 schema 校验在 store 收口**：`submitHumanResponse`（§4.3）对 `input` 按 `outputSchema` 校验，不合法直接拒收（通道侧重新提示），不产生 `humanResponded` 事件。runner 收到的 `humanResponded` 恒为合法载荷——与 action 实参的 `bind_arguments_invalid` 前置校验同一哲学：坏数据不进账本。

### 2.3 presents 的物化

`presents: Expr[]` 在 step 进入 `ready` 时求值一次并随 `humanRequested` 落盘（与 `resolvedInputs` 快照同一纪律）。可引用的内容即表达式作用域封闭清单（`params.* / env.* / steps.<id>.output.* / steps.<id>.verdict / vars.* / iter.<as>`）——**不存在** `result.*` 之类的额外作用域根（02 §8.1 的 RefPath 文法封闭，编译期即拒）。要把某步的截图证据呈给人，走 observe 类 step 的标准输出投影：`steps.<id>.output.screenshot` / `.uiSnapshot` 即已本地化的 EvidenceRef（03 §1.3 已定义 `{ observationId, screenshot?, uiSnapshot? }`；本文提请为该投影增补类型化缺料原因字段，见 §8-Q5）。action step 若需自定义 `outputs` 投影，自引用遵循 02 §4.1.1 特例：仅在本步 `outputs` map 内部，`steps.<自身 id>.output.*` 指向原始 `ActionResult.output`。

物化后的 `presents` 是审计的一部分：**证明人当时看到了什么**。Evidence 项以 sha256 引用，通道呈现时按需从本地 evidence 库取字节——`humanRequested` 事件本身不内嵌二进制（与 DeviceRail「binary evidence is referenced, not embedded」同一纪律）。

---

## 3. Secret 输入的处理（v0.1 铁律与 v0.2 路径）

### 3.1 问题与裁决

`human.input`（`provideInput`）的表单里能不能有密码类字段？人输入的密码是「人直接在设备上输」还是「经 Pointlock 转发给 provider」？

**v0.1 铁律：secret 值绝不经过 Pointlock 进程、RunLog、Evidence、Checkpoint 中的任何一个。** 需要人输入密码/PIN/支付口令的场景，一律走 `human.pause`（`repairWorld`）：人直接在设备上输入，值从不进入 Pointlock 的任何数据面。`provideInput` 的 `outputSchema` 若声明了 secret 语义字段（`format: "password"` 或 `sensitive: true`，见 §3.3），编译期 `bind` 阶段**拒绝**——与骨架 R6「protected action 在 bind 阶段 fail-closed 拒绝」同一条裁决的自然延伸。

### 3.2 理由：DeviceRail 的脱敏约定是下界，Pointlock 不能做得更差

以下 DeviceRail wire 层事实全部已核实（拼写逐字）：

- feature `action.protected.v1` 门控 protected Action；`ActionProtection` 枚举 `standard | protected`。
- **`RecordedActionCall`**（Action 调用的 durable 表示）：protected 与 unknown 调用只保留关联字段，`arguments` 序列化为 `null` 并带显式 `argumentsRedacted` 标记。即：**基座自己的 append-only 事件日志都拒绝持久化 protected 实参明文**。
- **`ManualActionArguments`** 双态：`{ kind: "captured", value }`（可安全持久化）与 `{ kind: "protected", secretRef }`——protected 实参是「host-resolved 的不透明引用而非 durable 值」（协议 README 原文语义），`secretRef` 上限 128 字符。
- `ScreenshotOmissionReason` 含 `protectedAction`，`UiSnapshotOmissionReason` 含 `protectedAction`：protected 动作执行期间基座连截图和 UI 树都会脱敏缺料。

推论链：如果 Pointlock 让密码走 `provideInput` → 值会进入 `humanResponded` 事件、`StepRecord.output`、下游消费步的 `resolvedInputs` 快照、act 前 WAL 的 `actionIntent.argsSnapshot`——**四处明文落盘**，而基座那端 `argumentsRedacted` 脱敏得干干净净。Pointlock 会成为整条链上唯一的明文泄露点。这不是实现难度问题，是骨架的持久化纪律（resolvedInputs 快照化、actionIntent 先 fsync 再 dispatch）与 secret 保密性**结构性冲突**——v0.1 唯一诚实解是不让 secret 进来。

### 3.3 v0.2 承诺路径（词汇表已预留，本文不实现）

骨架 R6 预留了 `secrets.*` 作用域与 `protected` YAML 关键字。v0.2 的形状（此处仅锚定方向，防 8 份文档各自发明）：secret 由外部 secret provider 以**不透明句柄**注入 `secrets.*`，只能整体出现在 protected action 的实参位，禁止参与运算、禁止出现在 Evidence；wire 层先例即 `ManualActionArguments` 的 `{ kind: "protected", secretRef }`。届时 `human.input` 依然不承载 secret——人提供的长期凭据应进 secret provider，而不是进对话框。

### 3.4 短信验证码（OTP）：敏感但非 secret 的特例

OTP 是短命凭据：低熵、一次性、分钟级过期。默认路径仍是 `repairWorld`（人直接在设备上输码）；§7.7 的半自动回填路径（人把码贴给 Pointlock，由 `set_value` 代输）**默认关闭**，开启后受以下持久化脱敏规则约束：

```ts
// pointlock-store（待骨架收编：RedactedValue 信封 + outputSchema 扩展键 sensitive）
export type RedactedValue = {
  redacted: true;
  kind: "otp" | "generic";
  length: number;        // 只存长度。刻意不存哈希：OTP 熵空间 ~10^6，
                         // 任何形式的 digest（含加盐）都可瞬时穷举，存哈希 = 存明文
};
```

规则（钉死）：

1. `outputSchema` 中标 `sensitive: true` 的字段，其值在**一切持久化点**（`humanResponded.payload`、`StepRecord.output`、消费步 `resolvedInputs`、`actionIntent.argsSnapshot`、Evidence 文档）替换为 `RedactedValue` 信封；明文只存在于 runner 进程内存，仅用于 dispatch。
2. **崩溃重放规则**：resume 时若悬挂意图的 `argsSnapshot` 含 `RedactedValue`，则该意图**不可重放**（明文已随进程消亡）。`reconcile(callId)` 返回 `completed` → 照常采认；返回 `neverDispatched` / `logUnavailable` → 不重放，判定「产生该敏感值的 human step 及其消费步」全部不可采认，resume 点回退至该 human step **重新请求**。这与现实语义严丝合缝——崩溃恢复时 OTP 早已过期，本来就该重发。
3. 同理，含 `RedactedValue` 的 `StepRecord.output` 在 §6.7 修复对齐中视同不可复用：任何引用它的下游 `judgeDirty` 离线重判若需要该明文 → verdict `unknown`（缺料即 unknown，原则 4）。

---

## 4. 通知通道抽象

### 4.1 契约：runner 只认账本，通道是旁观者

runner 与「人」之间**唯一**的契约是两个 RunLog 事件：`humanRequested`（runner 写）与 `humanResponded`（store 仲裁后写）。通道不是 runner 的依赖，而是账本的订阅者与响应的搬运工。这带来两个硬性质：

- runner 进程可以在 `awaitingHuman` 后退出（detached 模式），通道进程/适配器独立存活或事后启动，互不阻塞。
- 任意多个通道可以并发呈现同一 `HumanRequest`，响应由 store 的单写者仲裁收敛（§4.3）——不存在「CLI 和 Web UI 同时点了不同答案」的竞态歧义。

```ts
// pointlock-human-cli 及后续通道实现的公共接口（待骨架收编：HumanChannel）
export interface HumanChannel {
  readonly id: "cli" | "webUi" | "webhook";
  readonly capabilities: {
    notify: boolean;     // 能把 HumanRequest 送达人
    collect: boolean;    // 能把 HumanResponse 收回来
  };
  notify(req: HumanRequest): Promise<void>;   // 幂等（以 requestId 去重）；失败可重试
  // collect 能力的通道通过 store API 提交响应（§4.3），不经 runner
}
```

### 4.2 v0.1 范围（三通道的能力分级）

| 通道 id | notify | collect | v0.1 交付 |
|---|---|---|---|
| `cli` | 是 | 是 | **完整交付**（`pointlock-human-cli`，Rust crate） |
| `webhook` | 是 | 否 | **notify-only 交付**：向配置 URL POST `HumanRequest` 的 JSON 投影（不内嵌 evidence 字节，附本地取证路径与恢复提示），可配 `X-Pointlock-Signature: HMAC-SHA256` 头。**不接收入站响应**——v0.1 单进程本地架构（原则 10）没有可鉴权的入站 HTTP 面，收响应等于开一个无认证的写账本端口，拒绝 |
| `webUi` | — | — | **不交付**，接口占位。前置依赖是响应端 authn/authz，v0.2 与远程部署一起做 |

**R14 注记（收件箱条目的呈现形状 = 投影 DTO）**：pending 请求在一切渲染器中的收件箱条目形状统一为投影 DTO `HumanInboxEntry`——human step 与 supervision 请求的统一条目，含 `purpose` 判别子（骨架 §10.1）；一切渲染器只经投影协议消费，不触碰 store 内部（骨架 §10）。`webUi` 通道「不交付，接口占位」的既有裁决不变——投影协议是只读呈现契约，不改变响应端 authn/authz 的前置依赖。

**R13 注记（LLM 修复提议循环的人审批门）**：骨架 §6.9 收编 3 的 LLM 修复提议循环（失败 → `pointlock locate --format json` 卷宗 → 起草器提议 YAML patch（diff 形态）→ 人审批门 → 批准 → resume）的审批门复用本表的通道裁决：CLI 形态属 M2；UI 审批表单属 M3a，且**必须遵守本节既有裁决**——v0.1 `webUi` 不收响应，UI 只呈现 patch diff 与 align-preview 的 `alignmentReport` 预览（哪些历史保留、哪些重跑、哪些需确认），批准动作经 `pointlock-human-cli` / CLI 等价通道完成。

CLI 通道的两种形态（不突破骨架 7 命令封闭表）：

- **附着模式**：`pointlock run` 在交互 TTY 中运行时，`humanRequested` 直接内联渲染为终端提示（呈现 prompt、presents 摘要、decisions 选单或 schema 表单），人当场回应，run 不落盘挂起。
- **分离模式**：无 TTY（CI）或人未及时响应，runner 写 `runSuspended` 后退出（退出码标识 awaiting-human）。`pointlock inspect <runId>` 可查看 pending 请求全文；`pointlock resume <runId>` 检测到 `CheckpointView.humanPending` 时**先进入交互回应流程**（呈现→收集→经 store 仲裁写入 `humanResponded`），再继续执行。响应入口收敛在 resume 上，不新增 CLI 命令。

### 4.3 响应仲裁：store 的单写者语义

```ts
// pointlock-store（待骨架收编：submitHumanResponse）
submitHumanResponse(resp: HumanResponse):
  | { accepted: true }
  | { accepted: false;
      reason: "unknownRequest" | "alreadyResponded" | "deadlineExceeded" | "schemaViolation" };
```

仲裁规则（原子，SQLite 事务内）：

1. `requestId` 必须存在且未被响应（**first response wins**；后到者收 `alreadyResponded`，通道自行向人解释）。
2. `resp.at`（store 收到时刻）> `deadlineAt` → `deadlineExceeded` 拒收。**超时判定的唯一裁判是 store**，不是通道钟也不是 runner 钟。
3. `provideInput` 的 payload 过 `outputSchema` 校验，不合法 → `schemaViolation` 拒收（§2.2）。
4. 通过则原子追加 `humanResponded{requestId, purpose, payload(脱敏后), actor, at}`。

R13 注记：监督问答（`purpose="supervision"`，§5.5）的响应走**同一** `submitHumanResponse` 仲裁与同一收件箱，不新建第二套管道——规则 1（first response wins）与规则 4 原样适用；监督请求默认无 `deadlineAt`，规则 2 对其不适用；规则 3 仅涉 `provideInput`，亦不适用。

R14 注记：仲裁前 pending 请求的呈现面即投影 DTO `HumanInboxEntry`（骨架 §10.1，§4.2 R14 注记）——投影只读，与本节写侧仲裁互不相扰，响应提交仍唯一经 `submitHumanResponse` 收口。

### 4.4 actor 鉴别与信任边界

v0.1 的信任边界 = 本机（原则 10：单进程、本地存储）。`actor.principal` 记 `"os:<user>@<host>"`，由 CLI 通道从 OS 身份取得——这是**归因**（attribution）不是**认证**（authentication）：能碰到这台机器和这个 store 文件的人本来就拥有整个 run。诚实地把这一点写进报告，比伪造一个本地口令层更有价值。webUi 通道被推迟到 v0.2 正是因为跨出本机边界后归因必须升级为认证。

---

## 5. 等待语义：挂起、落盘、超时、升级

### 5.1 进入等待（WAL 纪律对齐 actionIntent）

human step 的执行序列（钉死顺序）：

```
ready（presents 求值并快照）
  → 追加 humanRequested{requestId, purpose:"step", mode, prompt, presents, decisions?, outputSchema?, requestedAt, deadlineAt} 并 fsync
  → 状态转 awaitingHuman，checkpoint 更新 humanPending
  → 而后才 notify 各通道（best-effort，失败重试并留痕，不影响 run 正确性）
```

先落盘后通知，与 `actionIntent` 先 fsync 再 dispatch 同一纪律：崩溃后 resume 凭账本就能重新 notify（`notify` 以 requestId 幂等），不会出现「人收到了通知但账本不知道问过」。

### 5.2 checkpoint 落盘与进程挂起

骨架 `CheckpointView.humanPending: { runPath, requestId, purpose, prompt }`（R13 增补 `purpose` 判别子）已覆盖恢复所需最小闭包（`deadlineAt` 可由 `humanRequested` 事件重建，是否冗余进 humanPending 见 §8-Q4）。`awaitingHuman` 是合法 suspend 点：runner 写 `runSuspended` 后进程可退出；DeviceRail 侧按需 `session.end(outcome: "shutdown")` 释放设备（resume 一律开新 session、记 `sessionLineage`，骨架 §6.7 已定，无需为等人白占设备）。要不要立即挂起由装配层策略决定（附着模式先等一个可配置的 grace 窗口，超过才 suspend）。

### 5.3 超时：绝对 deadline + lazy 结算

- `timeoutMs` 必填（骨架 `HumanStepIR.timeoutMs: number`），换算为绝对 `deadlineAt` 落盘。
- **进程在场**：runner 内存定时器到点即结算：verdict `unknown`（`onTimeout` 固定），走 `onUnknown` handler 链。
- **进程不在场（已 suspend）**：没人执行超时——超时在下一次「响应到达」或「resume」时**惰性结算**：store 仲裁拒收过期响应（§4.3 规则 2）；resume 时 runner 发现 `now > deadlineAt` 且无响应 → 结算 unknown。账本上 verdict 的记录时刻可以远晚于 deadlineAt，但判定依据只有 deadlineAt 与响应的 `at`，**结算结果与结算时刻无关**——确定性由此保住。
- 提醒/催办（重发通知）是通道自身的职责（webhook 可配 reminder 策略），不是 runner 语义；runner 只认一个 deadline。

### 5.4 Escalation：超时后的升级链

超时 → `unknown` → 由 `onUnknown` handler 承接，标准形态是 `{ kind: "escalate", human: HumanStepIR }`：换一个 prompt（例如从「请操作者确认」升级为「请值班负责人裁决」）、通常换更长的 `timeoutMs` 或更醒目的通道配置，产生**新的** requestId 走同一等待语义。链深由 `maxTriggers` 封顶（骨架 R10 防循环），耗尽后该 step 以 unknown 定格，flow 级折叠自然传染（unknown 弱于 fail、强于 pass）。分级超时（如 10 分钟提醒、30 分钟升级、2 小时放弃）在 YAML 里就是「human step + on_unknown escalate 链」，不需要新机制。

**默认升级姿态（R13，骨架 §6.9 收编 1）**：「拿不准就问人」自 R13 起是默认而非 opt-in——标准 authoring 模板自带 flow 级 `handlers: on_unknown → escalate`（`max_triggers: 1`），编译器对「flow 对 unknown 无任何处置」发 lint 级警告（RF3xxx 段、warning 非 error）；模板形状与诊断规则由 03 篇细化，本节的 escalation 语义即其运行期承接面。§7.6 示例末尾的 flow 级 `on_unknown: escalate` 兜底就是该默认姿态的手写形态。

### 5.5 监督模式与 human 子系统的关系（R13，骨架 §6.9；里程碑 M2）

`pointlock run` / `pointlock resume` 的 `--supervise <mutating|all>` 是**运行级**监督策略：与 `verdictPolicy` 同层级，但属于 run 而非 IR——不影响 `irHash`、不进任何哈希域，记入本段起始事件（`run` 段为 `runStarted`、resume 段为 `runResumed`）payload 的 `supervisePolicy` 字段供审计（值封闭枚举 `mutating | all`，骨架 A.4）。**逐段生效、不跨段隐式继承（骨架 §6.9）**：每个执行段以启动该段的命令旗标为准，resume 传入 `--supervise` 即覆盖此前策略、未传即本段无监督，payload 显式记 `null`。监督问答**不是 human step**，但完整复用本文的 human 管道：同一 `humanRequested` / `humanResponded` 事件对（以 `purpose="supervision"` 判别，§2.1）、同一 store 单写者仲裁（§4.3）、同一通知通道（`pointlock-human-cli`）、同一收件箱（附着/分离模式的 pending 呈现，§4.2；条目形状 = 投影 DTO `HumanInboxEntry`，含 `purpose` 判别子，骨架 §10.1）——**不新建第二套管道**。

与 human step 的语义差异（钉死，均引骨架 §6.9）：

- **门控点与 WAL 顺序**：门控在受监督 step 进入 `acting` 之前（此时 `resolvedInputs` 快照已可呈现）。顺序：`humanRequested(purpose="supervision", presents 含 runPath / actionName / resolvedInputs 摘要)` 先 fsync → 通知 → `humanResponded(decision)` → `decision = proceed` 才写 `actionIntent` → dispatch。先落盘后通知与 §5.1 同一纪律；监督门整体位于 `actionIntent` WAL 之前，故被拒绝的动作在账本上从未有过意图。
- **decision 封闭枚举**：`proceed | abort | suspend`（骨架 A.4）。v0.1 **刻意不提供 `skip`**——跳过 mutating 步破坏数据依赖，改动一律走修复路径。`abort` / `suspend` 的落账细则已由骨架 §6.9 钉死（R13 细化）：`abort` 不触发任何 handler（人的直接裁决优先于策略路由），step 记 `aborted`、run 走既有 aborted 终局（`runFinished`），`humanResponded` 事件即审计痕；`suspend` 写 `runSuspended`，supervision 请求保持 pending，下一段 resume 后该 step 仍处 `awaitingHuman` 照常等待回应（惰性结算，§5.3）；`proceed` 的 WAL 后续见上。
- **不产生 verdict**：监督问答是运行级门控而非 step 判定，§2.2 的 verdict 映射对其不适用；§6 的证据文档物化机制存在的理由是「Verdict 只允许引用 Evidence」，对不产 verdict 的监督问答不强制，其审计痕即事件对本身（是否同样物化证据文档待骨架裁决，§8-Q8）。
- **默认无超时**：无 `timeoutMs` / `deadlineAt`（§2.1 的 `deadlineAt` 对 supervision 缺省），§4.3 规则 2（`deadlineExceeded`）不适用；人可随时回应 `suspend`，等待期间 run 照 §5.2 挂起、进程可退出。
- **崩溃语义**：崩溃发生在问答中间时，重启后 supervision 请求仍 pending，惰性结算语义与 human step 同款（§5.3 的惰性纪律；因无 deadline，resume 后重新 notify——`notify` 以 requestId 幂等——并继续等待回应）。

---

## 6. Human 响应即 Evidence：谁、何时、决定了什么

`humanResponded` 已经是 RunLog 事件，但骨架规定 **Verdict 只允许引用 Evidence**（概念 11/12）。因此每个 human step 响应（`purpose="step"`，含超时结算）在结算时刻**物化为一份规范化 JSON 证据文档**，进入本地内容寻址库（监督问答不产 verdict，不在本节强制范围内，见 §5.5 与 §8-Q8）：

```jsonc
// mediaType: application/json；内容规范化（键排序、无空白）后取 sha256 入库
{
  "pointlockEvidence": "humanResponse/1",
  "requestId": "0f8c…",
  "runId": "run_20260716_0930",
  "runPath": "checkout@a1f3/purchase/hook:onUnknown#1/humanSlider",   // 规范串，结构化 RunPath 另存
  "mode": "repairWorld",
  "promptSha256": "sha256:…",                  // prompt 全文另存，文档内以哈希锚定
  "presented": [                                // 人当时看到了什么（哈希锚定，可回放）
    { "kind": "evidence", "sha256": "sha256:…", "mediaType": "image/png" }
  ],
  "decision": "done",
  "input": null,                                // provideInput 时为脱敏后 payload（sensitive 字段是 RedactedValue）
  "note": "滑块拖了两次才过",
  "actor": { "channel": "cli", "principal": "os:dengfengwang@mbp" },
  "requestedAt": "2026-07-16T09:30:12Z",
  "deadlineAt":  "2026-07-16T09:40:12Z",
  "respondedAt": "2026-07-16T09:33:47Z",        // 超时结算时此字段为 null，另有 "settledAs": "timeout"
  "responseLatencyMs": 215000
}
```

要点：

- **谁**：`actor`（通道 + principal）；**何时**：`respondedAt` 与 `responseLatencyMs`；**决定了什么**：`decision` / `input`（脱敏后）/ `note`；**在什么信息下决定**：`presented` 的哈希清单——这份文档使「人的判定」达到与机器 assertion 同级的可审计性。
- 文档入库后铸造 `EvidenceRef`，并合成 AssetRef 形状 `{ id, mediaType: "application/json", uri（本地库路径）, sha256 }`，从而可以进入 `provider.recordVerdict` 的 `evidence` 数组回写 DeviceRail（`verdict.record` 只校验持久化；注意 schema 上限 summary ≤ 16384 字符、evidence ≤ 64 条，折叠时 human evidence 与机器 evidence 同池竞争名额，超限按「verdict 直接依据优先」裁剪并在报告注记）。
- human step 的 verdict（§2.2 映射）**必须**引用这份证据文档；`judge` mode 下它就是 verdict 的全部依据。
- 超时结算同样出文档（`settledAs: "timeout"`，无 actor）——「没人来」也是要留痕的历史事实。

---

## 7. 交互式验证 handler 家族：`handle_interactive_verification`

### 7.1 问题域与定性

图形鉴权、滑块、图片验证码、短信验证码、安全键盘——统称**交互式验证态**（interactive verification challenge）：目标系统主动插入的、以「区分人机」或「保护敏感输入」为目的的拦截层。它们的共同结构是：

1. 出现时机不完全可预测（登录后、敏感操作前、风控触发时、resume 回来时）；
2. 出现的表现是**主流程断言失败或缺料**（预期元素被遮挡 → fail；安全键盘触发 `protectedAction` omission → unknown）；
3. 解除它需要人。

因此它天然是 **handler 问题**而不是主流程问题：主流程 YAML 不应被「万一出验证码」污染。`handle_interactive_verification` 是 Pointlock 标准库提供的一个 **handler 配置模式 + 一个标准修复 subflow**，作者以一行 handler 绑定接入。

### 7.2 与 Handler / Subflow / Macro 的关系（严格按骨架）

| 骨架概念 | 在本家族中的角色 |
|---|---|
| **Handler** | 接入点：宿主 step（或 flow 级）的 `on_fail` / `on_unknown` / `on_error` / `on_resume_drift` 绑定，`action: { kind: "repair", flowRef }` 指向修复流，`maxTriggers` 封顶；YAML 表面形状遵循 03 §1.8 的 Disposition head-key 语法（`repair: <路径>` + `max_triggers`）。注意骨架 §3 的 repair 变体**只有 `flowRef`、没有 `inputs`**——handler 不能向修复流传参，按目标应用注入参数走 §7.5 的绑定流模式（IR 层扩展提案见 §8-Q1）。骨架 R10：handler 无数据输出、只产处置决定、执行留 `hook:` 帧审计痕——**这正是本家族安全性的结构保证**：验证码处理过程中产生的一切数据（包括 OTP）被封闭在修复 subflow 内部，结构上不可能渗入主数据流 |
| **Subflow** | 承载体：`interactive_verification_repair` 是一个独立编译、按 `irHash` 锁定、可独立测试的一等公民 Flow。检测确认、分类、存证、人机节点、恢复验证全部在其 body 内；其 flow verdict（pass/fail/unknown）就是「修复成功与否」的判定，宿主 handler 据此路由（骨架 §3 HandlerAction repair 语义：跑完后重探 `onResumeDrift` 或重入 `onFail`） |
| **Macro** | 打包糖：`handle_interactive_verification` 宏把「敏感 step 的 preflight 探针 + 三个 hook 的 handler 绑定（`repair` 指向作者提供的绑定流路径，作宏参传入，§7.5）」一次性展开到宿主 step 上，编译期蒸发、origin trace 进 sourceMap。宏只是省抄写——手写等效的 handler 绑定完全合法（§7.6 示例即手写形态；宏调用语法属 YAML 界面文档管辖，见 §8-Q6） |

### 7.3 统一模式：六阶段

```
检测 detect → 分类 classify → 存证 snapshot → 人机 human → 恢复验证 re-verify → 路由 route
   (宿主侧)         (修复 subflow 内) ──────────────────────────────────────────┘
```

**阶段 1 · 检测（宿主侧，三条入口，全部是骨架既有钩子）**：

| 入口 | 触发形态 | hook |
|---|---|---|
| 断言否定 | 预期元素被验证层遮挡，`expect` 求值完成且不成立 | `onFail` |
| 断言缺料 | 安全键盘/protected 场景，`uiSnapshotOmission: protectedAction` 或 `screenshotOmission: protectedAction` 致 verify-chain 耗尽 → unknown | `onUnknown` |
| 动作错误 | `setElementValue` 等被验证层拦截而 `action_failed_final` | `onError`（`errorClasses: [action_failed_final]`） |
| resume 漂移 | 暂停期间设备被风控拦下，resume 探针（`preflight`）不过 | `onResumeDrift` |

检测**不做**图像识别式的「主动扫描验证码」：宿主侧只依赖既有断言/探针的否定与缺料信号。是不是验证态、是哪种验证态，交给修复 subflow 去确认——handler 触发本身允许假阳性（subflow 分类阶段发现根本不是验证态时，以 fail 收场并把入口证据留给人判）。

**阶段 2 · 分类（subflow 内，机器探针，不产 verdict）**：用 `wait_for`（原生 `waitForElement`，短超时）逐类探测标志元素，分支依据是协议输出 `waitForElementResult` 的 `matched` 布尔——探针是 readonly action step 且**不带 expect，故无 verdict**（骨架 R4：无断言 step 只有执行状态），探不到不会把 subflow 折叠成 fail。分类清单（标准 subflow 内部词汇，不进骨架枚举）：`graphicalAuth`（图形鉴权/手势）、`slider`（滑块）、`captcha`（图片/字符验证码）、`smsOtp`（短信验证码）、`secureKeyboard`（安全键盘）、`unknownChallenge`（兜底）。`secureKeyboard` 的分类特征是**缺料本身**：`observe` 回来 `uiSnapshotOmission`/`screenshotOmission` 为 `protectedAction`——omission 是类型化原因不是错误（骨架 §5），在这里反转为最强的分类信号。

**阶段 3 · 存证（进入时刻的世界快照）**：subflow 第一步就是 `observe`（wants: screenshot + uiSnapshot），Evidence 立即本地化（`ui.snapshot.get` 仅本 Session 活跃期可读，等人期间 session 可能断代，不本地化就永久丢证）。这份「进入验证态时长什么样」的证据随后呈给人（`presents`）、进 human 响应证据文档的 `presented` 清单、也进最终 verdict 的 evidence——**无论修复成败，验证态出现过这件事永远可审计**。

**阶段 4 · 人机（绝不自动绕过，见 §7.4）**：按分类路由到 human step。默认全部走 `repairWorld`（人直接在设备上完成滑块/图形/输码），`smsOtp` 在显式开启时可走 `provideInput` 半自动回填（§7.7）。

**阶段 5 · 恢复验证（人宣告 ≠ 世界已复原）**：human step pass 后，subflow 以 assert step 复核「验证态标志已离开」——检测谓词的否定（`state: absent`，`verify_via: [uiTree, vision]`，vision 依原则 7 只能垫链尾）。骨架 R5 在此生效：复核断言完成求值且否定（验证层还在）→ **fail 即终局**，不许降级链翻案。这一步就是 §2.2「repairWorld 必须紧跟机器复核」规则的落点。

**阶段 6 · 路由**：

```
subflow verdict pass    → 宿主 handler 处置完成：onFail 入口 → 宿主 step 从 acting 重入（新 callId）；
                          onResumeDrift 入口 → 重探 preflight
subflow verdict fail    → 本次 trigger 失败；宿主 handler maxTriggers 未耗尽 → 允许再触发（验证态可能再弹）
subflow verdict unknown → 同 fail 路由（不确定 ≠ 已修复），但报告分开计数
maxTriggers 耗尽        → 兜底 escalate：human judge（呈全部轮次证据，人对宿主 step 作三值终审）
                          或按作者配置 abort
human 超时              → 该 human step unknown → subflow verdict unknown → 上述 unknown 路由
```

### 7.4 为什么绝不自动打码（设计声明与边界）

「接打码平台/视觉模型自动解验证码」被**永久排除**在 Pointlock 能力之外，不是 v0.1 的省事，是四条结构性理由：

1. **类型层已经封死**：自动打码的本质是 vision 驱动 act（看图 → 点坐标/出答案）。骨架把 `Channel` 钉死为 vision 仅 verify-chain、coordinate 仅 act-chain 且必须静态坐标（原则 6/7），vision→act 的通路在 IR 类型上不存在。为打码开洞等于拆掉原则 7 的结构保证。
2. **verdict 诚实性（原则 3/4）**：验证码是目标系统行使「区分人机」权利的表达。绕过之后跑出来的 pass 验证的是「被绕过的系统」，不是真实系统——这样的 verdict 建立在被污染的前提上。Pointlock 的全部架构（evidence 锚定、双哈希、离线重判）都在为「verdict 可信」服务，自动打码从根上腐蚀这个目标。
3. **capability 语义不成立（原则 5）**：能力必须可 lockfile 化、可 attest。打码是与风控系统的对抗性军备竞赛，成功率随对方改版漂移，不存在可固化、可复核的能力声明——它连成为一个合法 capability 的资格都没有。
4. **合规与滥用面**：自动绕过人机验证在多数目标系统的服务条款下违约，且使 Pointlock 可被直接改造为攻击工具。设计上不提供这个部件，是对工具边界的声明。

**边界（什么不算打码，Pointlock 不拦）**：

- 目标系统提供的**正式测试后门**（测试环境固定 OTP、bypass token、白名单参数）：那是普通的 param / action 实参，走正常数据流。正确的工程解法从来是「测试环境关闭风控」，不是「生产手法绕过风控」。
- **人主导的半自动**（§7.7 OTP 回填）：读码、判断、决定回填的是人，Pointlock 只做搬运。它不是自动绕过——但因为它把敏感值引入 Pointlock 进程与持久层（即便有 §3.4 的脱敏），扩大了攻击面，所以默认关闭。
- 人在设备上解验证码（`repairWorld`）：这就是验证码的设计用途——由人来解。

### 7.5 修复 subflow：`interactive_verification_repair` 的契约

```
flowId:   interactive_verification_repair        （标准库发布，作者按 irHash 锁定引用）
params:   challenge_markers  object   各类验证态的标志元素选择器（经绑定流注入，见下）
          allow_otp_relay    boolean  默认 false（§7.7）
          human_timeout_ms   integer  各 human 节点的默认预算（默认 600000）
outputs:  无 —— handler 修复流无数据输出（骨架 R10 在契约层的体现）
verdict:  pass = 验证态已确认离开；fail = 未离开或人宣告不能修复；unknown = 人超时/复核缺料
```

**接线约束（IR 类型层的硬事实）**：骨架 §3 的 `HandlerAction` repair 变体是 `{ kind: "repair"; flowRef }`——**没有 `inputs` 字段**，handler 不能向修复流传参（与 R10「handler 无数据输出」互为对偶：现状是无入参也无出参）。而 `challenge_markers` 无合理默认值（标志元素选择器只能按目标应用给出），直接让 repair 指向标准流会因缺必填 param 在编译期被拒。因此 v0.1 的标准接线是**绑定流（binding flow）模式**：

- 作者为目标应用写一个**零必填参数**的薄包装 flow，body 唯一一步是 `call` 标准流，以字面量（或包装流自身带默认值的 params）注入 `challenge_markers` 等实参——`CallStepIR.inputs` 是骨架既有能力，编译期完全合法；
- repair handler 的 `flowRef` 指向这个绑定流；绑定流的 flow verdict = call step verdict = 标准流 verdict（骨架 §6.3），§7.3 阶段 6 的路由语义不变；
- 绑定流同样按 `irHash` 锁定，`challenge_markers` 因此成为锁定内容的一部分——选择器变更可审计，这是该模式相对「handler 直接传参」的额外收益。

`HandlerAction.repair` 是否扩展 `inputs?: Record<string, Expr>` 以省去绑定流样板，作为 IR 变更提案上报骨架（§8-Q1）。

### 7.6 完整示例

宿主 flow（手写 handler 绑定形态，形状遵循 03 §1.8 的 Disposition head-key 语法：处置动作作 head key，`repair` 的值 = subflow 路径，同 `call` 按 `irHash` 锁定）：

```yaml
flow: transfer_smoke
provider: devicerail
params:
  - name: amount
    schema: { type: string }

steps:
  - id: tapTransfer
    tap:
      element: { text: { value: "确认转账", mode: exact } }
    effect: mutating
    # 检测入口之一：resume/前置探针 —— 回来时若已被风控拦截，走 on_resume_drift
    preflight:
      - element: { identifier: "verificationOverlay" }
        state: absent
        verify_via: [uiTree]
    expect:
      - element: { identifier: "transferSuccessBanner" }
        state: visible
        verify_via: [uiTree, vision]
        visual: "转账成功横幅完整可见"
    on_fail:            # 断言否定：预期横幅被验证层遮挡
      repair: ./flows/bankapp-verification-repair.flow.yaml   # 绑定流（见下），编译期按 irHash 锁定
      max_triggers: 2
    on_unknown:         # 断言缺料：protectedAction omission 等
      repair: ./flows/bankapp-verification-repair.flow.yaml
      max_triggers: 2
    on_resume_drift:
      repair: ./flows/bankapp-verification-repair.flow.yaml
      max_triggers: 1

handlers:               # flow 级兜底：轮次耗尽后人来终审
  on_unknown:
    - escalate:         # Disposition: escalate → 升级人机节点（值 = human 子键，03 §1.8）
        mode: judge
        prompt: "自动修复轮次已耗尽。请审阅全部轮次证据，对本步作出终审。"
        presents:
          - ${{ steps.tapTransfer.verdict }}
        decisions: [pass, fail, unknown]
        timeout_ms: 7200000
        on_timeout: unknown
      max_triggers: 1
```

绑定流（§7.5 模式：作者按目标应用维护，零必填参数，repair handler 的实际指向）：

```yaml
flow: bankapp_verification_repair
provider: devicerail
steps:
  - id: runRepair
    call: ./interactive-verification-repair.flow.yaml   # 标准库流，编译期按 irHash 锁定
    inputs:                                             # CallStepIR.inputs：骨架既有能力
      challenge_markers:
        slider:      { identifier: "captcha_slider" }
        captcha:     { identifier: "captcha_image" }
        otp_input:   { identifier: "sms_code_input" }
        otp_submit:  { identifier: "sms_code_submit" }
        any_overlay: { identifier: "verificationOverlay" }
```

标准修复 subflow（示意全文；observe 输出投影的缺料原因字段见 §8-Q5）：

```yaml
flow: interactive_verification_repair
provider: devicerail
verdict_policy: strict
params:
  - name: challenge_markers
    schema: { type: object }
  - name: allow_otp_relay
    schema: { type: boolean }
    default: false
  - name: human_timeout_ms
    schema: { type: integer }
    default: 600000

steps:
  # ── 阶段 3：进入时刻存证（Evidence 立即本地化） ──────────────────
  - id: snapEntry
    observe: [screenshot, uiSnapshot]   # 03 §1.3 列表形；标准输出投影 = { observationId,
    effect: readonly                    #   screenshot?: EvidenceRef, uiSnapshot?: EvidenceRef,
                                        #   uiSnapshotOmission?, screenshotOmission? }（缺料原因字段收编中，§8-Q5）

  # ── 阶段 2：分类探针（readonly、无 expect ⇒ 无 verdict，不污染折叠） ──
  - id: probeSlider
    wait_for:
      element: ${{ jsonPath(params.challenge_markers, "$.slider") }}
      state: present
    timeout_ms: 2000
    effect: readonly
  - id: probeCaptcha
    wait_for:
      element: ${{ jsonPath(params.challenge_markers, "$.captcha") }}
      state: present
    timeout_ms: 2000
    effect: readonly
  - id: probeOtp
    wait_for:
      element: ${{ jsonPath(params.challenge_markers, "$.otp_input") }}
      state: present
    timeout_ms: 2000
    effect: readonly
  - id: classifySecureKeyboard          # 缺料即信号：protectedAction omission（字段收编见 §8-Q5）
    let:
      is_secure_keyboard: ${{ eq(steps.snapEntry.output.uiSnapshotOmission, "protectedAction") }}

  # ── 阶段 4：人机路由（绝不自动绕过） ─────────────────────────────
  - id: routeSlider
    if: ${{ steps.probeSlider.output.matched }}
    then:
      - id: humanSlider
        human:
          mode: repairWorld
          prompt: "检测到滑块验证。请在设备上完成滑动后确认。"
          presents:
            - ${{ steps.snapEntry.output.screenshot }}
          on_timeout: unknown
        timeout_ms: ${{ params.human_timeout_ms }}

  - id: routeCaptchaOrSecureKb
    if: ${{ or(steps.probeCaptcha.output.matched, vars.is_secure_keyboard) }}
    then:
      - id: humanSolveOnDevice
        human:
          mode: repairWorld
          prompt: "检测到验证码/安全键盘。请直接在设备上完成输入后确认。（安全输入不经 Pointlock 转发）"
          presents:
            - ${{ steps.snapEntry.output.screenshot }}
          on_timeout: unknown
        timeout_ms: ${{ params.human_timeout_ms }}

  - id: routeOtp
    if: ${{ steps.probeOtp.output.matched }}
    then:
      - id: otpPath
        if: ${{ params.allow_otp_relay }}
        then:                                        # §7.7 半自动路径（默认关闭）
          - id: askOtp
            human:
              mode: provideInput
              prompt: "已向绑定手机发送验证码。请查收短信并回填（4-8 位数字）。"
              presents:
                - ${{ steps.snapEntry.output.screenshot }}
              on_timeout: unknown
            expect_schema:                           # provideInput 的输入契约（关键字复用见 §8-Q2）
              type: object
              properties:
                code: { type: string, pattern: "^[0-9]{4,8}$", sensitive: true }
              required: [code]
            timeout_ms: 300000                       # OTP 生命周期短，预算独立收紧
          - id: fillOtp
            set_value:
              element: ${{ jsonPath(params.challenge_markers, "$.otp_input") }}
              value: ${{ steps.askOtp.output.code }} # 持久化点全部是 RedactedValue（§3.4）
            effect: mutating
          - id: submitOtp
            tap:
              element: ${{ jsonPath(params.challenge_markers, "$.otp_submit") }}
            effect: mutating
        else:                                        # 默认：人直接在设备上输码
          - id: humanOtpOnDevice
            human:
              mode: repairWorld
              prompt: "检测到短信验证码。请在设备上直接输入收到的验证码并提交，完成后确认。"
              presents:
                - ${{ steps.snapEntry.output.screenshot }}
              on_timeout: unknown
            timeout_ms: ${{ params.human_timeout_ms }}

  - id: routeUnknownChallenge                        # 兜底：探针全落空但 handler 被触发
    if: ${{ and(and(not(steps.probeSlider.output.matched), not(steps.probeCaptcha.output.matched)),
                and(not(steps.probeOtp.output.matched), not(vars.is_secure_keyboard))) }}
    then:
      - id: humanUnknown
        human:
          mode: repairWorld
          prompt: "触发了修复流程但未识别出已知验证类型。请查看设备与截图，将界面恢复到可继续状态后确认；若并非验证拦截请选择不能修复。"
          presents:
            - ${{ steps.snapEntry.output.screenshot }}
          on_timeout: unknown
        timeout_ms: ${{ params.human_timeout_ms }}

  # ── 阶段 5：恢复验证（人宣告 ≠ 已复原；fail 即终局，R5） ─────────
  - id: confirmChallengeGone
    expect:
      - element: ${{ jsonPath(params.challenge_markers, "$.any_overlay") }}
        state: absent
        verify_via: [uiTree, vision]                 # vision 只垫链尾（原则 7）
        visual: "屏幕上不存在任何验证/风控遮罩层"      # 链尾含 vision 时必填（03 §1.4 规则 5）
      - element: ${{ jsonPath(params.challenge_markers, "$.otp_input") }}
        state: absent
        verify_via: [uiTree]
```

折叠核算：分类探针与 `let`/`if` 骨架无 verdict；有 verdict 的是命中的 human 节点与 `confirmChallengeGone`。人 done（pass）+ 复核 pass → subflow pass；复核 fail → subflow fail（验证态未离开）；人超时 → unknown 传染 → subflow unknown。`verdict_policy: strict` 确保复核若靠 vision 降级通过（degraded pass）折叠为 unknown——「验证态是否离开」这种安全判定不接受降级通道的绿灯。

### 7.7 短信验证码的半自动路径（设计完成，默认关闭）

上例 `allow_otp_relay` 分支即完整设计，要点收拢：

- **人主导**：短信到达的是人的手机（或人可见的收件渠道），读码、决定回填的是人；Pointlock 只做「人 → 输入框」的搬运。通知渠道（webhook payload / CLI 提示）会注明「请查收短信并经 respond 回填」。
- **默认 `false` 的理由**：它把 OTP 引入 Pointlock 进程内存与持久层路径。§3.4 的 `RedactedValue` 规则保证持久化面零明文（含 `humanResponded`、`StepRecord.output`、`fillOtp` 的 `resolvedInputs` 与 `actionIntent.argsSnapshot`），但进程内存中明文短暂存在，攻击面客观大于 `repairWorld`。默认安全、显式弃权（opt-in），并且开启动作本身进报告注记。
- **崩溃语义**：`fillOtp` 悬挂意图含 `RedactedValue` → 不可重放；`reconcile` 非 `completed` 时回退至 `askOtp` 重新请求（§3.4 规则 2）——与「OTP 已过期、理应重发」的现实语义一致。
- **超时预算独立**：OTP 的 `timeoutMs` 建议 300000（5 分钟）而非默认 10 分钟——超过短信有效期的等待没有意义，早点 unknown、早点走重发轮次。
- **不做的事**：Pointlock 不读短信（不接短信网关、不读设备通知栏做自动提取）。自动提取 = 无人参与 = 落入 §7.4 的自动绕过禁区；且设备通知栏内容属 protected 语义高危区。

---

## 8. 开放问题（提请骨架收编 / 上游文档裁决）

| # | 事项 | 本文暂用形态 |
|---|---|---|
| Q1 | **IR 类型缺口（非 YAML 拼写问题）**：骨架 §3 `HandlerAction` repair 变体只有 `flowRef`、无 `inputs`，handler 无法向修复 subflow 传参；带必填 param 的标准修复流按现行类型无法被 repair 直接引用（编译期缺参即拒）。提请骨架裁决是否扩展为 `{ kind: "repair"; flowRef; inputs?: Record<string, Expr> }`；若扩展，需同步钉死：① 求值时机 = handler 触发时求值一次并快照（纪律同 `resolvedInputs`），② 哈希域归属（骨架 §3 双哈希规则目前不覆盖 handler 配置，需明示 repair.inputs 是否入 `effectHash`）。handler 的 YAML 表面（head key `repair`/`escalate`、`max_triggers`、`error_classes`）03 §1.8 已定义、已列 03 §5-1 收编项，本文直接沿用，不另立形状 | §7.5 绑定流模式（现行类型内的合法接线），§7.6 |
| Q2 | `provideInput` 的输入契约在 YAML 层复用断言关键字 `expect_schema`（避免新增关键字），需骨架确认这次复用 | §7.6 `askOtp` |
| Q3 | `HumanRequest` / `HumanResponse` / `HumanActor` / `HumanChannel` / `PresentedItem` / `RedactedValue` 类型名与 `submitHumanResponse` store API、outputSchema 扩展键 `sensitive` | §2.1、§3.4、§4.1 |
| Q4 | `CheckpointView.humanPending` 建议增补 `deadlineAt`（惰性超时结算免回读事件流） | §5.2 |
| Q5 | observe 类 step 的标准输出投影（03 §1.3 已定义 `{ observationId, screenshot?: EvidenceRef, uiSnapshot?: EvidenceRef }`，其收编即 03 §5-11）提请增补类型化缺料原因字段：`uiSnapshotOmission?: UiSnapshotOmissionReason`、`screenshotOmission?: ScreenshotOmissionReason`（枚举与 DeviceRail wire 逐字对齐：`driverUnsupported\|policy\|protectedAction` / `policy\|protectedAction`）。这是 §7.3 阶段 2 `secureKeyboard`「缺料即信号」分类的唯一合法数据通路——02 §8.1 RefPath 文法封闭、02 §4.1.1 自引用特例只达 `ActionResult.output`，均不提供 Observation omission 的访问面；本文先前的 `result.*` 占位已废弃。该字段收编前，`classifySecureKeyboard` 分支不可编译，家族对 secureKeyboard 的覆盖 blocked on 此裁决 | §7.6 `snapEntry` / `classifySecureKeyboard` |
| Q6 | 宏调用语法（`handle_interactive_verification` 的展开触发写法）与标准库 subflow/宏的分发机制，属 YAML 界面文档（01/02）管辖 | §7.2 以手写等效形态给例 |
| Q7 | `confirm` 双标签定制（恰好两项、位置映射 pass/fail）与 `judge` 固定三值的编译期校验规则，需 YAML/编译文档同步 | §2.2 |
| Q8 | 监督问答（R13，骨架 §6.9）的类型化收编缺口：**①③ 已收口（R13 细化）**——① supervision 响应并入 `HumanResponsePayload` union（无 mode 变体，decision 封闭枚举 `proceed \| abort \| suspend`，§2.1）；③ `decision = abort / suspend` 的落账细则由骨架 §6.9 钉死（abort 不触发 handler、step 记 aborted、run 走既有 aborted 终局；suspend 写 runSuspended、请求保持 pending）。**仍开放**：② 监督问答是否与 human step 同样物化 §6 证据文档（其不产 verdict，故非强制） | §2.1、§5.5 |

---

*本文引用的 DeviceRail wire 层事实（`action.protected.v1`、`RecordedActionCall.argumentsRedacted`、`ManualActionArguments{kind: captured|protected, secretRef}`、`ScreenshotOmissionReason: policy|protectedAction`、`UiSnapshotOmissionReason: driverUnsupported|policy|protectedAction`、`waitForElementResult{matched, condition, element?}`、`session.end` 四值 outcome、`verdict.record` 的 summary ≤ 16384 / evidence ≤ 64 上限）均已逐字对照 DeviceRail 源仓库 protocol schema 与 README 核实。*
