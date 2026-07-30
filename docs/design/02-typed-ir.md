# Pointlock 设计文档 02：Typed IR v0.1 —— Schema 与语义

> 本文是 Pointlock 设计文档系列第 2 篇，骨架见 [00-architecture-spine.md](./00-architecture-spine.md)。本文是全系列的**锚文档**。
>
> **类型真相源（R12，同骨架 §3 引言块）**：本文与全系列的 TS 类型记法保留为**规范记法**；类型唯一真相源是 `pointlock-ir` 的 Rust DTO（serde + schemars），CI 由 DTO 生成 JSON Schema（Draft 2020-12）、`@pointlock/ir-types` 与 golden fixtures（管线细则见 §1.1）。仓库根 [`schema/flow-ir.v0.1.schema.json`](../../schema/flow-ir.v0.1.schema.json)（`$id: urn:pointlock:schema:ir:v0.1:flow-ir`，Draft 2020-12）自 R12 起降级为**验收基线**：M0 的生成 schema 必须与之 diff 评审、语义等价后基线随生成物滚动。`irVersion` 不 bump——IR 形状不变，仅真相源与实现语言变化。在 M0 生成物落地前，本文与验收基线 schema 冲突时以 schema 为准；schema 与骨架冲突时以骨架为准。

---

## 1. 地位与总纲

Typed IR 是 Pointlock 内部唯一的执行契约：

- `pointlock-compiler` 的 `seal` 阶段产出 `FlowIR`，是 YAML 的**终点**——阶段 1（`parse`）之后 YAML 不复存在（原则 1）。
- `pointlock-runner` 的入口签名只接受 `FlowIR`，不接受字符串、不接受路径、不接受任何"顺便帮你编译"的糖（原则 2 的结构性保证）。
- `pointlock-store` 里的 RunLog、Checkpoint、alignmentReport 全部以 IR 的 `stepId` + `effectHash`/`judgeHash` 为坐标系。

因此这份 schema 同时是三份契约：编译器的输出契约、runner 的输入契约、checkpoint 对齐的坐标系定义。

### 1.1 代码生成管线与验收基线（R12）

类型真相源自 R12 反转：`pointlock-ir` 的 Rust DTO（serde + schemars）是唯一真相源，本文的 TS 记法保留为规范记法。CI 管线复制 DeviceRail 已验证的做法：

```
pointlock-ir Rust DTO（serde + schemars）
  ├─→ JSON Schema（Draft 2020-12，schema/ 下）
  ├─→ @pointlock/ir-types（type-only npm 包，角色镜像 @devicerail/protocol）
  └─→ golden fixtures（schema/ 下，Rust 侧与 TS 侧消费方的一致性锚点）
```

- 现有 `schema/flow-ir.v0.1.schema.json` 降级为**验收基线**：M0 验收项要求该管线打通，且生成 schema 与基线**行为等价**；此后基线随生成物滚动（骨架 B.1）。
- **「语义等价」判据（2026-07-17 裁决，原 openQuestion 关闭）= 行为等价**：生成 schema 与基线 schema 对完整 golden fixture 语料（正例 + 全部反例组，含本节验证声明所列 17 组反例）的**接受/拒绝判定逐一完全一致**即为等价；文本 diff 仅供人读，不作判据。fixture 语料因此是 schema 契约的**可执行规范**：M0 起入库（`schema/` 下，与生成物同处），随 IR 演进**只增不删**。
- 本文 §2 的四条结构性约定与 §12 的规范化规则对生成物同样有约束力——行为等价验收（辅以人读 diff）即检查生成物是否兑现它们。
- `irVersion` 不 bump：IR 形状不变，仅真相源与实现语言变化（§11 判据下不构成语义变化）。
- **同款管线复用于投影 DTO（R14）**：投影协议五族 DTO 同走 schemars 管线，生成 JSON Schema + type-only TS 包 `@pointlock/projection-types` + golden fixtures；其真相源在 `pointlock-store` 的 projection 模块，**非本篇范围**（骨架 00 §10.2）。`projectionVersion` 与 `irVersion` 相互独立（§11、00 §10.3）。`@pointlock/projection-types` 是 M3a 前置产物，M0 不要求交付。

**验证声明**：交付的 schema 已通过 Draft 2020-12 元 schema 校验；本文 §13 的完整示例实例通过 schema 校验；17 组反例（act-chain 出现 vision、verify-chain 出现 coordinate、`protection: protected`、越界 ref 作用域、`errorClasses` 挂错 hook、PureFn 元数错误、step 上的未知字段、`onMissingInput` 非 unknown、visual 谓词配非 vision 链、expr 谓词配观测通道、`provideInput` 缺 outputSchema、空 attempts、human `onTimeout` 非 unknown、缺 `checkpoint`、非法哈希格式、`irVersion: 2`、assert step 空断言）全部被 schema 拒绝。

---

## 2. Schema 组织的四条结构性约定

### 2.1 单文件、`$defs`、命名一一对应

全部类型收在一个文件的 `$defs` 里，键名与骨架 A.3 的 TS 类型名逐字相同（`ActionStepIR`、`BoundAttempt`、`AssertionIR`、`ElementSelectorIR`……）。工程上这保证 `pointlock-ir` 的 Rust DTO 类型名、生成 schema 的 `$defs` 键与 `@pointlock/ir-types` 的 TS 类型名三者逐字对应而无需名字映射表（R12 真相源反转后生成方向变为 DTO → schema/ir-types，对应关系不变）。

### 2.2 默认封闭，三类显式豁免

所有 object 都是 `additionalProperties: false`。豁免恰好三类，每类在 schema 内注明理由：

| 豁免 | 位置 | 理由 |
|---|---|---|
| `JsonSchemaDocument` | `ParamDecl.schema`、`OutputDecl.schema`、`outputSchema` | 内嵌的是一份 JSON Schema 文档，其内部文法归 JSON Schema 元 schema 管，不归本 schema 管 |
| 标识符键 map | `ExprMap`（args/inputs/outputs/bindings）、`FlowIR.subflows` | 键是数据不是结构；用 `propertyNames` 约束键的文法，值仍强类型 |
| `StepBase` | `$defs/StepBase` | 被 7 个 step 变体经 `allOf` 组合；封闭责任移交给每个变体的 `unevaluatedProperties: false` |

### 2.3 判别式联合：`kind` const + `oneOf` + `unevaluatedProperties`

`StepIR` 是 7 个变体的 `oneOf`，每个变体：

```json
{ "allOf": [{ "$ref": "#/$defs/StepBase" }],
  "properties": { "kind": { "const": "action" }, ... },
  "unevaluatedProperties": false }
```

这是 Draft 2020-12 下"继承 + 封闭"的唯一干净写法：`additionalProperties: false` 看不见 `allOf` 引入的属性，`unevaluatedProperties: false` 看得见。代价是校验器必须支持 2020-12（R12 工具链：Rust 侧参考校验器为 `jsonschema` crate，支持 Draft 2020-12；`pointlock-ir` 的 DTO 装载本身走 serde 严格反序列化，二者语义一致性由 golden fixtures 锚定，§1.1）。

### 2.4 默认值物化 + absence-by-omission（哈希单一表示原则）

同一语义只允许一种字节表示，否则内容寻址（§12）失效。两条推论：

1. **有默认值的字段在 sealed IR 中必须物化为显式值**，schema 相应把它们设为 required：`StepBase.checkpoint`（默认 true，宏展开体内默认 false）、`ActionStepIR.idempotent`（默认 false）、`TextMatchIR.mode`（默认 `exact`）与 `caseSensitive`（默认 false）。填默认值是 `normalize` 阶段的职责。
2. **缺席一律用"字段不出现"表达，禁止 null**。DeviceRail 的 `ElementSelector` 用 `string | null` 联合，`ElementSelectorIR` 与之字段同名、上限同值，但把 null 表示收敛为 omission——这是 IR 与 wire 类型唯一的形状差异，`pointlock-provider-devicerail` 在出 wire 前补 null 即可。
3. 无默认值的可选字段（`retry`、`timeoutMs`、`preflight`、`handlers`、`verb`、`outputs`……）缺席即语义（"没有预算上限"、"没有探针"），不物化。

---

## 3. Step 通用结构（`StepBase`）

```json
{ "stepId":  "...",         // 必填：作者提供、flow 内全局唯一（含嵌套体）、稳定
  "effectHash": "sha256:…", // 必填：这一步"对世界做什么"的规范化哈希（§12.3）
  "judgeHash":  "sha256:…", // 必填：这一步"如何被判定"的规范化哈希（§12.3）
  "preflight": [AssertionIR, …],  // 可选：前置/resume 世界探针
  "retry": RetryPolicy,     // 可选：只作用于 act 阶段
  "timeoutMs": 15000,       // 可选：step 预算
  "handlers": [HandlerBinding, …],// 可选：step 级钩子，覆盖 flow 级
  "checkpoint": true }      // 必填（物化默认）：是否在本步边界物化 checkpoint
```

**`stepId` 是身份，哈希是内容**——这是整个 resume 对齐机制（骨架 §6.7）的支点：修复 YAML 时不改 id 就保住历史，对齐器按 id 匹配新旧 StepRecord，再比双哈希分类为 `reusable | judgeDirty | effectDirty | new | orphaned`。因此 stepId 刻意**不**参与 effectHash/judgeHash 的计算（改名 = 新身份 = `new` + `orphaned`，不需要哈希帮忙）。schema 中 stepId 的文法分两层：作者可写的只有首段 `[A-Za-z_][A-Za-z0-9_-]*`；`:` 分段保留给编译器合成 id——handler 内嵌 human step（如 `flow:onUnknown:escalate`）与 macro 卫生展开的前缀 id（如 `set_ssid:field`，§7）共用这一段位。合成 id 永不出现在 ref 路径里（`RefPath` 的 `StepIdRef` 文法不含 `:`）：handler 无输出本就引不到；宏展开步的输出没有 ref 表面——宏是文本便利，不是数据流单位（§7，与 03 §1.7 规则 2 一致）。

**`preflight` vs `expect`（YAML 层）的钉死辨析**再重复一次：`preflight` 编译为 `StepBase.preflight`，是**进入本步之前**对世界的探针，兼任 resume 漂移检测（骨架 §6.7-C）；`expect` 编译为 action/assert step 的 `assertions`，是**动作完成之后**的后置断言。二者共享 `AssertionIR` 类型但语义位置永不混用。

**IR 里没有的东西**：任务清单里的"输出值、evidence 引用、verdict、错误"都不在 IR 里——它们是运行期产物，落在 `pointlock-store` 的 `StepRecord`（骨架 §6.6）：`resolvedInputs`（ready 时的表达式快照）、`attempts[]`（每次 callId/outcome/errorClass/execution.mode）、`observations[]`、`evidence[]`（EvidenceRef 本地内容寻址）、`assertionOutcomes[]`、`verdict`。IR 只放**声明**：输入是 `args`/`inputs` 里的 `Expr`，输出是 `outputs` 投影声明 + `outputSchema` 契约，断言是 `AssertionIR`，错误处置是 `retry` + `handlers`。这条 IR/StepRecord 分界线就是"Action / Assertion / Verdict 分离"（原则 3）在存储层的投影。同理，监督模式（`--supervise <mutating|all>`，R13，骨架 §6.9）是 run 级策略而非 IR 的一部分：不进 IR、不进任何哈希域，只记录于 `runStarted` 事件 payload 的 `supervisePolicy` 字段供审计。

---

## 4. Step kind 清单（7 种，封闭）

### 4.1 `action` —— 固定流水线 `preflight? → act → observe → assert`

| 字段 | 必填 | 语义 |
|---|---|---|
| `verb` | 否 | `CanonicalVerb` 之一，**纯元数据**（报告/binding report 用）。runner 没有 verb switch：执行只看 `binding.attempts[].actionName` |
| `effect` | 是 | `mutating \| readonly`。`EffectClass` 的 `pure` 被 schema 排除（`EffectClassAction`）：纯计算不过 Provider，属于 `let`。effect 决定 reconcile 不确定分支的重放许可（readonly 恒可重放） |
| `idempotent` | 是（物化） | 作者声明。生效处恰两个：`action_timed_out` 的自动重试许可；resume 时 `reconcile` 返回 `logUnavailable/startedNoTerminal` 的重放许可 |
| `binding` | 是 | 编译期完全绑定的 act-chain（§5.1） |
| `assertions` | 是（可为空数组） | 后置断言。**空数组 = 本步不产生 verdict**，报告层标 `unverified`（骨架 R4：action 成功 ≠ 语义通过，宁可诚实地不判，也不发弱 pass） |
| `outputs` | 否 | 输出投影：`Record<name, Expr>`，从 `ActionResult.output` / Observation 元数据抽取（§4.1.1） |
| `outputSchema` | 否 | 下游静态类型检查的依据；`invoke` 逃逸门的输出不写 schema 即为 `unknown` 类型，下游取字段必须先 `expect_schema` 收窄 |

**4.1.1 outputs 投影与 self-ref 的作用域约定（设计决策，待骨架收编）**。`outputs` 的表达式需要引用**本步自己的原始结果**，而封闭作用域清单（`params/env/vars/iter/steps`）没有 "self" 根。本章不新增作用域根，而是钉死 self-ref 特例——**合法位置恰两处，语义各异**：

1. **本步 `outputs` map 内部**：`steps.<自身 id>.output.*` 合法，指向**原始** `ActionResult.output`（投影的输入）。
2. **本步 `assertions` 的 `expr` 谓词内部**：`steps.<自身 id>.output.*` 合法，指向**投影后**的输出（断言检查的是本步对外承诺的数据契约）。无鸡生蛋问题：断言在 `asserting` 阶段求值（§8.3），彼时 action 已执行、投影已完成。

**其余一切位置 self-ref 非法**（`check` 拒绝）：`args`/`inputs`/`cond`/`items`/`bindings`/`presents` 在 `ready` 时求值（§8.3），彼时本步输出尚不存在；`preflight` 在 `probing` 阶段求值，同理。下游 step 引用 `steps.<id>.output.*`（非 self-ref）恒指投影后输出。`outputs` 缺席时投影为恒等：`steps.<id>.output ≡ ActionResult.output`。这保住了 ref 文法的封闭性，代价是两条必须写进 `check` 阶段的特例规则。

### 4.2 `assert` —— 无副作用的观察与判定

```json
{ "kind": "assert",
  "observe": "fresh" | { "fromStep": StepId, "which": "after" | "before" },
  "assertions": [AssertionIR, …] }   // minItems 1
```

`observe: "fresh"` 触发一次 `ProviderSession.observe`（readonly，重放安全）；`fromStep` 形式复用指定 action step 的 `ActionResult.before/after` Observation——不碰设备，纯离线可重判。assert step 的 `assertions` 至少一条（没有断言的 assert step 无意义，schema 拒绝）。

### 4.3 `call` —— 见 §6。

### 4.4 `human` —— 人机协作正式节点（原则 8）

```json
{ "kind": "human",
  "mode": "confirm" | "judge" | "provideInput" | "repairWorld",
  "prompt": "…", "presents": [Expr, …],
  "decisions": ["confirmed", "rejected"],   // 可选；judge/confirm 的枚举选项
  "outputSchema": { … },                    // provideInput 时必填（schema 强制）
  "timeoutMs": 600000,                      // human 步必填：无界等待用 runSuspended 表达，不用悬空挂着表达
  "onTimeout": "unknown" }                  // const：超时绝不默认 pass/fail（原则 4）
```

`presents` 是呈给人的证据/值的表达式列表；人的回应经 `humanResponded` 事件入 RunLog，`mode: "judge"` 的判定即本步 verdict，`mode: "provideInput"` 的回应即本步输出（按 `outputSchema` 校验）。`repairWorld` 模式的输出即其决定（`{decision, note?}`，06 §2.2），决定封闭为 `done | cannotRepair`（06 §2.1；2026-07-28 统一裁决）——`done` 折为本步 verdict pass、`cannotRepair` 折为 fail（06 §2.2，绝不静默变 run abort）；`onResumeDrift` 的 reconcile 裁决（骨架 §6.7-B / 07 §4.4）复用同一 human 通道但**自带声明词表** `adopt / redo / abort`，按声明仲裁，不落入基座词表。

### 4.5 `if` / 4.6 `foreach` / 4.7 `let`

- `if`：`cond: Expr` + `then: StepIR[]`（≥1）+ 可选 `else`。未选中分支的 step 状态为 `skipped`。
- `foreach`：`items: Expr` + `as: Identifier` + `body: StepIR[]`。迭代变量经 `iter.<as>` 引用；RunPath 用 `{ kind: "iteration", index }` 帧区分各轮，因此 body 内 stepId 不需要（也不允许）按轮次变化。
- `let`：`bindings: ExprMap`（≥1 项），产出 `vars.*`，SSA 单赋值——同名重绑是 `check` 阶段错误。

三者都是完整的 step（有 stepId、双哈希、可 checkpoint），但**容器步的哈希不含子树**（§12.3）：子 step 自持身份与哈希，对齐在子步粒度进行。

---

## 5. Fallback 双链与 capability binding 在 IR 中的形态

### 5.1 act-chain：`ActionBinding.attempts`

```json
"binding": { "attempts": [
  { "channel": "uiTree",
    "actionName": "tapElement",
    "args": { "target": { "lit": { "kind": "selector",
                                   "selector": { "identifier": "wifi_row" } } } },
    "requiresFeature": "device.semanticActions.v1",
    "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
    "protection": "standard" },
  { "channel": "coordinate",
    "actionName": "tap",
    "args": { "x": { "lit": 512 }, "y": { "lit": 384 } },
    "acceptExecutionModes": ["nativeSemantic", "webSemantic", "coordinateFallback"],
    "protection": "standard" } ] }
```

要点，逐条对应设计原则：

1. **有序、封闭**。YAML 没写 `locate_via` 就恰好一个 attempt——runner 绝不即兴降级（原则 6）。attempt 推进规则由骨架 §6.2 钉死：前一 attempt 以 `action_failed_final` 终止才试下一个。
2. **`channel` 的类型是 `ActChannel = dom | uiTree | coordinate`**。vision 不在枚举里——"视觉不能定位/行动"（原则 7）不是 lint 规则，是类型系统事实。对偶地，`verifyVia` 的类型是 `VerifyChannel = dom | uiTree | vision`，coordinate 进不去（坐标验证不了任何事）。
3. **`actionName` 恒为 provider 原生名**（骨架 R7/A.6）：五件套 `findElement | tapElement | clearElement | setElementValue | waitForElement`，或 lockfile 暴露的 driver 专有 action（上例 coordinate attempt 绑定 mock/desktop driver 的 `tap`）。动词在编译后消失，`verb` 字段只是尸检标签。schema 的 `ActionName` 文法拒绝 `<provider>:<action>` 限定形式（多 provider 预留，v0.1 裸原生名）。
4. **`args` 的值是 `Expr`**：选择器等静态结构以 `{ "lit": … }` 包裹整个 wire 形状（上例 `target` 是 DeviceRail `tapElement` 实参 schema 的 `ElementTarget`，逐字 `{ kind: "selector", selector: ElementSelector }`）；动态值用 `{ "ref": … }`。编译期 `bind` 已按 lockfile 中该 action 的 `inputSchema` 做过形状校验，运行期求值后**二次**校验，失败 → `bind_arguments_invalid`。
5. **coordinate attempt 必须携带字面量静态坐标**（YAML 关键字 `coordinate` 必填）。schema 表达不了"args 里必须有 lit 坐标"这种跨字段语义，归 `bind` 阶段拒绝清单（§14）。
6. **`acceptExecutionModes` 是 daemon 内部降级的白名单**（骨架 §6.4 R-degrade），**逐 attempt 推导，且只由该 attempt 自身的 `channel` 决定**：语义 attempt（`dom`/`uiTree`）恒为 `["nativeSemantic","webSemantic"]`；`coordinate` attempt 恒为 `["nativeSemantic","webSemantic","coordinateFallback"]`。作者在 `locate_via` 写入 `coordinate`，授权的是链上**显式的坐标 attempt**——它有自己的静态坐标实参、自己的 attempt 记录与 Evidence；这**不等于**授权语义 attempt 在 daemon 内部静默降级为坐标（后者只留下一个事后申报的 `execution.mode`，证据形态完全不同）。因此语义 attempt 的白名单永不含 `coordinateFallback`，无论链上是否声明了坐标兜底。运行期 `succeeded` 但 `execution.mode` 不在白名单 → 不算成功也不试下一 attempt（动作已发生），强制 verify-chain 全量确认，确认不了 → `unknown`。本节开头的 JSON 与 §13.2 的 `open_wifi_settings` 都是该规则的实例：`locate_via` 含 `coordinate`，uiTree attempt 的白名单仍不含 `coordinateFallback`。
7. **`protection` 是 const `"standard"`**：schema 直接把 v0.1 的 R6 裁决（protected action 在 bind 拒绝）铸进类型；v0.2 引入 `secrets.*` / `protected: true` 时此处变枚举，属于 breaking change，走 §11 的版本化流程。

### 5.2 capability binding 的完整拼图

IR 中承载 capability 绑定的字段共五处，合起来构成"capability-bound 编译"（原则 5）的证据链：

| 字段 | 位置 | 谁消费 | 语义 |
|---|---|---|---|
| `lockfileDigest` | FlowIR | `openSession` attestation | 编译所依据的 `CapabilityLockfile.digest`；运行期 attestation 不符 → `capability_drift`，拒跑 |
| `requiredFeatures` | FlowIR | `openSession` → `FeatureOffer.required` | 全流程 feature 并集；协议语义保证 required 不满足则 `system.hello` 握手失败——免费强制力 |
| `actionName` | BoundAttempt | `ProviderSession.execute` | 已对 lockfile.device.actions 验存在、验 inputSchema |
| `requiresFeature` | BoundAttempt | runner（attempt 可用性） | 如五件套 → `device.semanticActions.v1` |
| `verifyVia` 各通道 | AssertionIR | runner（verify 可用性） | 如 `uiTree` → `observation.uiSnapshot.v1`（manifest.channels 声明） |

**observe / screenshot 动词的表示（待骨架收编）**：这两个动词的 DeviceRail 落点是 RPC 方法 `device.observe`（及 `media.stream.*`），不是 `device.execute` 的 action，因此没有天然的 `actionName`。本章 schema 不为此开新结构（`ActionBinding` 保持骨架原样）；建议的收编方案是让 `pointlock-provider-devicerail` 的 manifest 在 `knownActions` 中声明合成条目（如 `observe`，adapter 内部路由到 `device.observe`），使其与五件套走完全相同的 bind/execute 通道。在骨架裁决前，`observe`/`screenshot` 动词的编译规则由 Provider 契约文档细化，本 schema 的 `ActionName` 文法已兼容该方案。

### 5.3 verify-chain：`AssertionIR.verifyVia`

```json
{ "assertId": "wifi_screen_open",
  "predicate": { "type": "elementState",
                 "selector": { "identifier": "wifi_toggle" }, "state": "visible" },
  "verifyVia": ["uiTree", "vision"],
  "visionPrompt": "Wi-Fi 设置页已打开，Wi-Fi 开关控件在屏幕上可见",
  "onMissingInput": "unknown" }
```

- `verifyVia` 是显式 verify-chain：`[dom, uiTree, vision]` 的子序列。**vision 只准出现在链尾**——顺序约束 schema 表达不了，归 `bind`；但"coordinate 不得出现"已由 `VerifyChannel` 枚举封死。
- 谓词类型与链的耦合由 schema 强制到能强制的程度：`visual` 谓词 → `verifyVia` 恒等于 `["vision"]`（视觉谓词只有视觉能答）；`expr` 谓词 → `verifyVia` 恒为 `[]`（纯输出断言不消费观测通道——骨架把 `verifyVia` 声明为 `Channel[]` 未规定空链，本章将空链定义为"无观测需求"，待骨架确认）；`elementState`/`elementText` 谓词 → 至少一个通道。
- **`visionPrompt` 是 vision 降级提示词的 IR 落点**（03 §1.4 规则 5 / E3 的类型化）：`elementState`/`elementText` 断言的 `verifyVia` 含 `vision` ⟺ `visionPrompt` 在场（YAML 表面键 `visual`，逐字交 `pointlock-vision` 的 VisionVerifier）——编译器绝不自动生成视觉提示（原则 6）。该充要条件由 schema 条件子句直接强制；`visual` 谓词的提示词在谓词自身的 `prompt` 里，`visionPrompt` 对 `visual`/`expr` 谓词断言禁止在场（单一表示，服务哈希）。`visionPrompt` 属断言全字段之一，随 `assertions` 进 `judgeHash` 域（§12.3）——改提示词 = `judgeDirty`，离线重判，不重跑设备。
- `onMissingInput` 是 const `"unknown"`（原则 4 的类型化）：某通道因 omission（`uiSnapshotOmission: driverUnsupported|policy|protectedAction`、`screenshotOmission: policy|protectedAction`）或缺料无法完成求值时，换下一通道；链耗尽即 `unknown`。**完成求值的否定是终局**：某通道明确给出 fail，后续通道不再尝试（骨架 R5——降级链解决"看不到"，不解决"不喜欢答案"）。

---

## 6. Subflow 调用与参数

```json
{ "kind": "call",
  "stepId": "ensure_session",
  "flowRef": { "flowId": "ensure_logged_in",
               "irHash": "sha256:bbbb…" },
  "inputs": { "username": { "ref": "params.username" } } }
```

配套的 flow 级注册表：

```json
"subflows": { "ensure_logged_in": { "flowId": "ensure_logged_in", "irHash": "sha256:bbbb…" } }
```

设计决策与理由：

1. **引用，不内联**（骨架 `FlowIR.subflows` 注释原文："引用不内联"）。callee 是独立编译、独立版本化的 `FlowIR` 工件，caller 只按 `irHash` 锁定。理由：(a) 同一 callee 被多处调用不膨胀 caller；(b) callee 可独立测试、独立 resume 分析；(c) caller 的 `irHash` 计算覆盖 `subflows` 里的 callee irHash，形成**联编闭包**——callee 变 → caller 的 irHash 必变，杜绝"子流程悄悄换了"。
2. **按内容锁定，不按版本号**。`flowRef.irHash` 是 `normalize` 阶段解析引用后钉死的；runner 装载 callee 时校验 irHash 相等，不等即拒（等价于依赖 lockfile 的 integrity 字段）。
3. **call-by-value，硬作用域边界**（原则 9 + 骨架 §7）：`inputs` 在 caller 作用域求值、快照后成为 callee 的 `params`；callee 只见显式 inputs，caller 只见 callee 声明的 `outputs`（经 `steps.<callStepId>.output.*` 引用）。没有闭包、没有共享可变量。
4. **verdict 聚合**：call step 的 verdict = callee 的 flow verdict（骨架 §6.3）；RunPath 加 `{ kind: "call", … }` 帧，checkpoint 有 CallFrame 栈。
5. **编译期完整性**：`bind` 校验每个 `flowRef` 在 `subflows` 有条目且 irHash 一致、callee 的 `requiredFeatures` 并入 caller、callee 的 `provider` 与 caller 相同（v0.1 单 provider）。循环调用在 `normalize` 拒绝（调用图必须是 DAG）。

## 7. Macro：展开，不引用

任务问"macro 展开后在 IR 中的形态——展开还是引用？"答案是**彻底展开**（骨架概念 4 已裁决，本节给可落地细则）：

- `normalize` 阶段做卫生展开（hygiene：宏体内 stepId 重命名为 `<调用点 id>:<体内 id>`——`:` 分段是 schema `StepId` 留给编译器合成 id 的段位（§3），防撞由前缀保证；合成 id 不入 ref 文法，故宏体内跨步 `steps.*` 引用与宏外引用宏体步输出都是编译错误，需要体内数据流 → 用 subflow），展开后宏在 IR 中**没有任何结构性存在**——没有 MacroStepIR、没有宏帧、没有宏级 verdict。判断依据是骨架 §2.1 三问：运行期无身份、无数据流签名、不可独立判定的东西不配进 IR。
- 唯一残留是 `sourceMap[].origin`：宏展开链（`MacroOriginFrame[]`，最内层在前），把展开出的 IR 节点映射回 YAML 里的宏定义与调用点。`pointlock locate` 靠它把失败坐标译回作者写的那一行。
- 展开体内的 step `checkpoint` 物化为 `false`（骨架 StepBase 注释：宏展开体内默认 false）——宏是文本便利，不是 checkpoint 边界；作者在宏体内显式写 `checkpoint: true` 可覆盖。
- 禁递归（`normalize` 拒绝），展开深度有上限（与 `parse` 的 byte/depth 上限同属 fail-closed 资源约束）。

与 subflow 的对照记忆：**macro 在编译期蒸发，subflow 在编译期链接**。需要运行期身份（独立 verdict、断点、复用已验证工件）用 subflow；只是少打字用 macro。

---

## 8. 表达式的精确文法

IR 表达式是数据（JSON AST），不是字符串——YAML 里的 `${{ … }}` 在 `parse`/`normalize` 就编译成如下 AST，运行期没有解析器、没有 eval。

### 8.1 文法（EBNF）

```ebnf
Expr     ::= LitExpr | RefExpr | FnExpr
LitExpr  ::= '{' '"lit"' ':' JSON '}'                       (* 任意 JSON 值，含 null *)
RefExpr  ::= '{' '"ref"' ':' '"' RefPath '"' '}'
FnExpr   ::= '{' '"fn"' ':' PureFn ',' '"args"' ':' '[' Expr* ']' '}'

RefPath  ::= 'params.' Ident ('.' Ident)*                   (* run 输入，只读 *)
           | 'env.'    Ident                                 (* binding 注入的只读环境 *)
           | 'vars.'   Ident                                 (* let 产物，SSA *)
           | 'iter.'   Ident                                 (* foreach 迭代变量 *)
           | 'steps.'  StepIdRef '.' 'output' ('.' Ident)*   (* 上游输出（投影后） *)
           | 'steps.'  StepIdRef '.' 'verdict'               (* 上游 verdict status *)
Ident     ::= [A-Za-z_][A-Za-z0-9_]*
StepIdRef ::= [A-Za-z_][A-Za-z0-9_-]*                        (* 作者层 stepId；合成 id 不可引用 *)
```

作用域清单封闭（骨架 §7），`secrets.*` 预留 v0.2——schema 的 `RefPath` 正则今天就拒绝它，防挪用。**深访问收窄为点分标识符**：数组下标、通配、含特殊字符的键一律不进 ref 文法，统一走 `jsonPath` 纯函数。理由：ref 文法保持正则可判定，使"引用了哪些 step"这个问题（数据依赖图、下游失效计算的输入）可以纯语法地回答。

### 8.2 PureFn 白名单（元数由 schema 强制，类型由 `check` 强制）

| fn | 元数 | 签名 | 静态约束 |
|---|---|---|---|
| `eq` / `ne` | 2 | `(T, T) → boolean` | 两参类型可比 |
| `not` | 1 | `(boolean) → boolean` | |
| `and` / `or` | ≥2 | `(boolean…) → boolean` | 无短路可观测差异（纯函数、无副作用） |
| `concat` | ≥1 | `(string…) → string` | |
| `len` | 1 | `(string \| array) → number` | |
| `coalesce` | ≥2 | `(T?, …, T) → T` | 首个非缺席值 |
| `jsonPath` | 2 | `(any, string) → any` | **args[1] 必须是 LitExpr 字符串**（路径静态可查）；结果类型 `unknown`，取用需 `expect_schema` 收窄 |
| `regexMatch` | 2–3 | `(string, string[, string]) → boolean` | **pattern 与 flags 必须是 LitExpr**，编译期预编译并拒绝灾难性回溯类 pattern |

非图灵完备：无循环、无自定义函数、无 I/O、无时钟。这不是省事——是让 `asserting` 阶段成为纯计算，从而使离线重判（`judgeDirty` 对齐）在数学上成立。

### 8.3 求值时机（两个时机，钉死）

表达式按所处位置分属两个求值时机，各自一次性、确定性：

1. **实参求值（`ready`）**：`args`（action）、`inputs`（call）、`bindings`（let）、`cond`（if）、`items`（foreach）、`presents`（human）在 step 进入 `ready` 时求值**一次**，快照入 `StepRecord.resolvedInputs`；resume 不重算（骨架 §6.6 刻意选择：杜绝重判后下游漂移）。此时本步 action 尚未执行，因此这些位置引用 `steps.<自身 id>.*` 是 `check` 阶段错误（§4.1.1）。求值后的实参按目标 action 的 `inputSchema` 二次校验，失败 → `bind_arguments_invalid`（不重试——这是编译器或表达式的 bug 信号，不是环境噪声）。
2. **探针与断言求值（`probing` / `asserting`）**：`preflight` 在 `probing` 阶段求值；`outputs` 投影在 `observing` 之后对原始 `ActionResult.output` 求值，产出 `StepRecord.output`；`assertions`（含 `expr` 谓词内的表达式）在 `asserting` 阶段求值，输入 = ActionResult.output（经 `outputs` 投影）+ before/after Observation + 本步 observe 产物（骨架 §6.2）。因此本步 `assertions` 内的 self-ref（§4.1.1）无鸡生蛋问题；其结果入 `assertionOutcomes` 与 verdict，**不进** `resolvedInputs` 快照。断言求值是纯计算、输入全部已存档——离线重判（`judgeDirty`，§12.3）在数学上成立，靠的正是这个时机划分。

骨架 §7 的「求值时机」条款描述的是第 1 类（实参快照）；第 2 类由骨架 §6.2 对 `asserting` 阶段「纯计算」的定义给出。本节把两者并列钉死，防止「进 ready 时一次性求值」被误读到断言头上。

---

## 9. 断言、Verdict 枚举与判定规则

### 9.1 IR 侧：只有问题，没有答案

IR 中与判定有关的只有 `AssertionIR`（谓词四型：`elementState | elementText | expr | visual`，见 §5.3）和 flow 级 `verdictPolicy: "standard" | "strict"`。**Verdict 类型本身不在 IR schema 里**——verdict 是运行期产物（`{ status: pass|fail|unknown, degraded, summary, evidence[] }`），住在 StepRecord 与 RunLog 的 `verdictRecorded` 事件里，经 `ProviderSession.recordVerdict`（wire：`verdict.record`，上限 summary ≤ 16384 字符、evidence ≤ 64 条）回写 DeviceRail 存证。把答案排除在 IR 之外是原则 3 的 schema 级表达。

`elementState` 的取值 `present | visible | enabled | absent` 与 DeviceRail `WaitForElementCondition` 逐字相等；`elementText` 的 `match` 是 `TextMatchIR`（`mode: exact|contains`）；`ElementSelectorIR` 与 wire `ElementSelector` 字段同名同上限（§2.4 注明的 null 收敛除外）。

### 9.2 判定规则（复述骨架 §6.3，IR 视角）

- 单条 assertion 沿 `verifyVia` 求值：完成且成立 → pass（非首选通道 → 记 `degradedVerify`）；完成且不成立 → **fail 终局**；无法完成 → 下一通道，链耗尽 → unknown。
- Step 折叠：`any fail → fail`，`else any unknown → unknown`，全 pass 无降级 → `pass(degraded=false)`；全 pass 但有 `degradedVerify` 或未授权 `coordinateFallback` → `pass(degraded=true)`，且 `verdictPolicy: "strict"` 时折叠为 unknown——示例（§13）选 strict 就是为了演示这一档。
- `assertions: []` 的 mutating action step 不产生 verdict；报告注记 `unverified` **不是** VerdictStatus 的第四个值（骨架 R4）。
- human step 的 verdict 由人产生（`mode: judge`），超时 const unknown；call step 的 verdict = callee flow verdict；flow verdict = 有 verdict 的 step 同规则折叠。

折叠是确定性纯函数，输入全部在 RunLog 里——所以 verdictPolicy 或断言修复后可以**离线重折叠/重判**而不碰设备，这正是双哈希（§12.3）存在的原因。

---

## 10. Handler 与 Retry 在 IR 中

```json
{ "hook": "onUnknown",
  "action": { "kind": "escalate", "human": HumanStepIR },
  "maxTriggers": 1 }
```

- `hook` 封闭四值 `onFail | onUnknown | onError | onResumeDrift`；`errorClasses`（`ErrorClass` 子集）**仅** `onError` 可写——schema 用 if/else 强制，挂在 `onFail` 上直接校验失败。
- `action.kind` 即 `Disposition` 枚举：`retry | continue | escalate | abort | repair`。`escalate` 内嵌完整 `HumanStepIR`（合成 stepId，§3）；`repair` 引用修复 subflow 的 `flowRef`——无数据输出，跑完重探或重入（骨架 HandlerAction 注释）。
- Handler **没有输出**（骨架 R10）：schema 里 HandlerAction 各变体没有任何 outputs 字段可写，错误路径进不了数据流。审计走 RunLog 的 `handlerTriggered` + RunPath `hook` 帧。
- 重试挂载点恰两处（骨架 §6.5）：`StepBase.retry`（act 阶段内，同 attempt 重发，新 callId）与 handler `{ kind: "retry" }`（整 step 从 acting 重入，预算独立）。schema 里 `RetryPolicy` 只出现在这两个位置——没有断言重试、没有 flow 级重试，结构上不存在第三处。
- `RetryPolicy.retryOn` 类型为 `ErrorClass[]`；语义上有意义的只有 `action_failed_retryable`、`target_stale`（重试前强制重 observe）与 idempotent 步的 `action_timed_out`，`check` 阶段对其余取值告警。

---

## 11. IR 版本化与向后兼容策略

- **`irVersion` 是语义世代号**，v0.1 schema 钉死 `const: 1`。它宣告的不只是形状，还有：closed vocabulary 的取值集、哈希域划分（§12.3）、规范化规则。三者任一变化都是语义变化。
- **bump 规则**：新增可选字段且不进入任何哈希域、不改变任何既有实例的执行语义 → schema 修订（`$id` 升 `v0.1.x`），`irVersion` 不变；其余一切——枚举增删值、required 变化、哈希域调整、规范化规则调整、`protection` 开放 `protected`、`secrets.*` 落地——**一律 bump `irVersion`**。判据只有一条：旧 runner 拿到新 IR，或新 runner 拿到旧 IR 的 RunLog，是否可能**静默地**得出不同行为？可能，就 bump。
- **R12 案例（不 bump）**：真相源从 TS 反转为 Rust DTO（§1.1）不改变形状、closed vocabulary、哈希域或规范化规则中的任何一项，按上述判据 `irVersion` 不 bump——仅真相源与实现语言变化。
- **runner 精确匹配**：runner 只接受 `irVersion` 恰等于其支持值的 IR，其余 fail-closed 拒绝（不是"尽力执行"）。`pointlock inspect` 负责报告版本与 schema 校验结果。
- **跨版本 resume 不承诺**（v0.x）：`irVersion` 参与所有哈希的 domain tag（§12.2），版本变 → 哈希全变 → 对齐自然全部 `effectDirty`。这是刻意的：跨语义世代复用历史记录需要显式 migration 工具，而不是碰巧哈希相等。
- **Store 侧**：RunLog/Checkpoint 记录产生它的 `irVersion` 与 `irHash`；`pointlock report` 对历史 run 永远用当时的语义解读。
- **`projectionVersion` 与 `irVersion` 相互独立（R14）**：投影协议 DTO 携带的 `projectionVersion`（骨架 00 §10.3）是**读侧契约**的版本号，不影响 IR、checkpoint 或本文任何哈希域——投影演进（additive-only，breaking 需 bump `projectionVersion`）不触发本节的 `irVersion` bump 判据，反之亦然。

---

## 12. 规范化与内容寻址（irHash / effectHash / judgeHash）

### 12.1 规范形（canonical form）

哈希的输入是 IR 子树的**规范 JSON 序列化**，规则五条：

1. 序列化用 **RFC 8785（JCS）**：键按 UTF-16 码元升序、无空白、数字按 ES 最短表示。
2. 字符串在 `seal` 前统一 **Unicode NFC**（JCS 不管规范化，编译器管）。
3. 默认值已物化、缺席即字段不存在、无 null 表示缺席（§2.4）——同语义单一字节形。
4. 数字约束：计数/预算/行列号等整数域必须是整数；坐标与 backoff 允许有限 double。`NaN`/`Infinity` 不是 JSON，天然出局。
5. 表达式已是 AST，无字符串歧义；`args`/`inputs` 等 map 由 JCS 排序，与作者书写顺序无关。**数组顺序有语义**（attempts、verifyVia、assertions、body），原样参与哈希。

### 12.2 三个哈希的定义

统一构造：`sha256( utf8(domainTag + "\n" + JCS(субtree)) )`，输出 `"sha256:<hex>"`。domain tag 做域分离，防跨域碰撞与跨版本误认：

| 哈希 | domainTag | 输入子树 |
|---|---|---|
| `irHash` | `pointlock-ir/1/irHash` | 整份 FlowIR，**剔除两个字段**：`irHash` 自身（自指）与 `sourceMap`（纯诊断——注释挪行、宏调用点变化不该使 resume 历史失效）。每步的 `effectHash`/`judgeHash` 保留在树中（确定性派生值，保留使单文件可自洽复核：`pointlock inspect` 重算比对） |
| `effectHash` | `pointlock-ir/1/effectHash/<kind>` | 该步"对世界做什么"域（12.3 表） |
| `judgeHash` | `pointlock-ir/1/judgeHash/<kind>` | 该步"如何被判定"域（12.3 表） |

`irHash` 经 `subflows.*.irHash` 传递覆盖全部 callee——联编闭包（§6）。`lockfileDigest` 同理是 lockfile 规范形的 domain-tagged sha256（`domain_hash("pointlock-lockfile/1", 规范形)`，`pointlock lock` 计算；骨架 §4.1，M1 收编）。

### 12.3 双哈希的域划分（宪法条款的可执行化）

骨架 §3 给了原则（effectHash = binding/attempts + 实参规范形 + effect + call 的 flowRef/inputs；judgeHash = preflight + observe + assertions + verifyVia——`onTimeout` 在 v0.1 是 const `"unknown"`，不入哈希域），本节钉死逐 kind 字段清单：

| kind | effectHash 域 | judgeHash 域 |
|---|---|---|
| `action` | `kind` `effect` `idempotent` `binding`（attempts 全字段）`outputs` `outputSchema` | `preflight` `assertions`（全字段，含 verifyVia/visionPrompt/onMissingInput） |
| `assert` | `kind` | `preflight` `observe` `assertions`（全字段，含 verifyVia/visionPrompt/onMissingInput） |
| `call` | `kind` `flowRef` `inputs` | `preflight` |
| `human` | `kind` `mode` `prompt` `presents` `decisions` `outputSchema` | `preflight` |
| `if` | `kind` `cond` | `preflight` |
| `foreach` | `kind` `items` `as` | `preflight` |
| `let` | `kind` `bindings` | `preflight` |

六条裁决及理由：

1. **`stepId` 不进任何一侧**：身份与内容分离是对齐机制的支点（§3）。
2. **`retry` / `timeoutMs` / `checkpoint` / `handlers` / `verb` 不进任何一侧**：预算、断点粒度、错误策略、报告标签都不改变"已成功记录的那次执行"的效力——改重试策略不应作废历史（它们仍进 `irHash`，改了它们新旧 IR 当然不是同一份 flow，但逐步对齐不受累）。
3. **`outputs`/`outputSchema` 进 effectHash（保守裁决）**：输出投影是本步的对外数据契约，下游 `resolvedInputs` 依赖它。投影理论上可像断言一样离线重算（ActionResult 已存档），但重算会级联触发下游快照漂移分析，v0.1 不背这个复杂度——改投影 = `effectDirty` = 从该步重跑。放宽（"projectionDirty 离线重投影"）留给 v0.2 评估。
4. **human step 的问题域整体进 effectHash**：人的回答绑定所提的问题——prompt、presents、decisions、outputSchema 任一变化，旧回答对新问题无效，必须重问。human 的 judgeHash 域因此只剩 `preflight`，其 `judgeDirty` 必为 preflight-only 变更——按裁决 6 采认旧回答，不重问。
5. **容器步（if/foreach）哈希不含子树**：子 step 自持哈希、以自己的 stepId 对齐；容器只对"控制决策"（cond/items/as）负责。cond 变 → 容器 `effectDirty`，其子树按骨架 §6.7 的下游失效规则处理。
6. **preflight-only 变更不作废历史（全 kind 统一）**：`preflight` 探针问的是「进入本步之前的世界」，而已完成步的进入时刻在历史上已经发生——当时要么实际通过了旧探针，要么本就不处于 probing 条件（探针只在声明了 preflight 或作为 resume 首步时执行，骨架 §6.2/§6.7-C），且不存在与之对应的「入场观测」存档可供新探针离线求值。因此对齐器对每个 `judgeDirty` 先做**子域比对**（新旧 IR 均按 irHash 存档可得）：judgeHash 变化**仅由 `preflight` 子域引起**（judgeHash 域其余部分逐字相等）→ 该步按 `reusable` 采认（verdict/output/evidence 全复用、不重判、不重问），`alignmentReport` 条目的 `reason` 标注 `preflightChanged`；新 preflight 只在该步成为 resume 首个待执行步时才实际生效（probing 恰只发生在那里）。对 `call`/`human`/`if`/`foreach`/`let`（judgeHash 域只有 preflight）这是其 `judgeDirty` 的唯一情形；对 `action`/`assert`，preflight 与 assertions/observe 同时变化时走常规离线重判——重判只针对 assertions/observe 子域，preflight 部分仍按本条不影响历史效力。执行细则同步落在 07 §5.3。

**双哈希如何供 checkpoint 对齐用**（呼应任务清单）：resume 时对齐器按 stepId 匹配新 IR 与旧 StepRecord，`effectHash` 同 + `judgeHash` 同 → `reusable`；`effectHash` 同 + `judgeHash` 变 → `judgeDirty`，对存档 Observation/Evidence 离线重跑新断言（§8 的纯函数性在此兑现），产出 `supersedes` 新 verdict——变化仅在 `preflight` 子域时例外，按裁决 6 采认为 `reusable`（标 `preflightChanged`），不重判；`effectHash` 变 → `effectDirty`，该步及数据依赖下游失效。这就是"修复只改断言 → 不重跑设备"的全部机制。

---

## 13. 完整示例

### 13.1 YAML 源（符合 03 的 surface 规范文法；文法的规范定义在 03）

```yaml
flow: wifi_toggle
provider: devicerail
verdict_policy: strict

params:
  ssid:     { schema: { type: string, minLength: 1 }, required: true }
  username: { schema: { type: string }, required: false, default: qa-bot }

outputs:
  wifi_verdict:
    schema: { enum: [pass, fail, unknown] }
    from: ${{ steps.wifi_on_visible.verdict }}

macros:
  fill_field:                            # 编译期蒸发（§7）；params 命名与 03 §1.7 的同名宏一致
    params: [element, value]
    steps:
      - id: field                        # 展开为 set_ssid:field（hygiene 合成前缀，§7）
        set_value: { element: "${{ macro.element }}", value: "${{ macro.value }}" }   # flow 集合内表达式须引号（03 §1.9；M2 勘正）
        idempotent: true                 # 刻意不写 expect：展开步 assertions: [] → unverified（R4）

handlers:
  on_unknown:
    - escalate:
        mode: judge
        prompt: 机器无法判定本步结果，请依据证据人工裁决
        decisions: [pass, fail, unknown]
        timeout_ms: 3600000
        on_timeout: unknown
      max_triggers: 1

steps:
  - id: open_wifi_settings
    preflight:
      - element: { identifier: settings_root }
        state: visible
        verify_via: [uiTree]
    tap: { element: { identifier: wifi_row } }
    locate_via: [uiTree, coordinate]
    coordinate: { x: 512, y: 384 }
    effect: mutating
    idempotent: true
    timeout_ms: 15000
    retry:
      max_attempts: 2
      backoff_ms: { initial: 500, factor: 2, max: 4000 }
      retry_on: [action_failed_retryable, target_stale]
    expect:
      - element: { identifier: wifi_toggle }
        state: visible
        verify_via: [uiTree, vision]
        visual: "Wi-Fi 设置页已打开，Wi-Fi 开关控件在屏幕上可见"   # 链尾含 vision 时必填（03 §1.4 规则 5）

  - id: set_ssid                         # macro 调用：head key = 宏名（03 §1.2 分派第 4 支）
    fill_field:
      element: { identifier: ssid_field }
      value: ${{ params.ssid }}

  - id: wifi_on_visible
    expect:
      - element: { identifier: wifi_toggle }
        state: enabled
        verify_via: [uiTree, vision]
        visual: "Wi-Fi 开关处于打开（enabled）状态"
      - element: { identifier: current_network_label }
        text: { value: Pointlock-Lab, mode: contains }
        verify_via: [uiTree]

  - id: ensure_session
    call: ./flows/ensure-logged-in.flow.yaml   # 相对路径；normalize 独立编译并按 irHash 锁定（03 §1.6）
    inputs: { username: "${{ params.username }}" }

  - id: wait_if_passed
    if: ${{ eq(steps.wifi_on_visible.verdict, 'pass') }}
    then:
      - id: wait_connected
        wait_for: { element: { identifier: connected_banner }, state: visible }
        timeout_ms: 30000
        expect:
          - expr: ${{ eq(steps.wait_connected.output.matched, true) }}   # 本步 assertions 内 self-ref（§4.1.1）

  - id: confirm_wifi
    human:
      mode: confirm
      prompt: 确认设备已连接到目标 Wi-Fi 网络
      presents: ["${{ steps.wifi_on_visible.verdict }}", "${{ params.ssid }}"]
      decisions: [confirmed, rejected]
      on_timeout: unknown
    timeout_ms: 600000

  - id: label
    let: { report_label: "${{ concat('wifi:', params.ssid) }}" }
```

### 13.2 编译产物 FlowIR（已通过 schema 校验；哈希为演示占位，实际由 `seal` 按 §12 计算）

> **勘注（M2 勘正，以实现为准；下方 IR 示例本体保持原样，供对照）**：与 §13.1 及实现存在三处已知出入。
> (a) `wait_connected` 的 `outputs`/`outputSchema` 在 §13.1 无 authoring 表面——step 级 `outputs` 语法属未实现的糖，M2 编译器对 action 步不产 `outputs` 投影。
> (b) 本节 `assertId` 均为演示值；实现按谓词内容哈希派生（`expr-<hex8>`）或由作者显式命名。
> (c) `requiredFeatures` 含 `verdict.record.v1`；实现裁决该 feature 为 optional（provider 未协商时 verdict 回写 no-op），不进编译器归集。

```json
{
  "irVersion": 1,
  "flowId": "wifi_toggle",
  "irHash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "provider": { "name": "devicerail", "version": "0.4.2" },
  "requiredFeatures": [
    "device.semanticActions.v1",
    "observation.uiSnapshot.v1",
    "verdict.record.v1"
  ],
  "lockfileDigest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  "params": [
    { "name": "ssid", "schema": { "type": "string", "minLength": 1 }, "required": true },
    { "name": "username", "schema": { "type": "string" }, "required": false, "default": "qa-bot" }
  ],
  "outputs": [
    { "name": "wifi_verdict",
      "schema": { "enum": ["pass", "fail", "unknown"] },
      "from": { "ref": "steps.wifi_on_visible.verdict" } }
  ],
  "verdictPolicy": "strict",
  "body": [
    {
      "kind": "action",
      "stepId": "open_wifi_settings",
      "effectHash": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
      "judgeHash": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
      "checkpoint": true,
      "verb": "tap",
      "effect": "mutating",
      "idempotent": true,
      "timeoutMs": 15000,
      "preflight": [
        { "assertId": "settings_root_visible",
          "predicate": { "type": "elementState",
                         "selector": { "identifier": "settings_root" }, "state": "visible" },
          "verifyVia": ["uiTree"],
          "onMissingInput": "unknown" }
      ],
      "retry": {
        "maxAttempts": 2,
        "backoffMs": { "initial": 500, "factor": 2, "max": 4000 },
        "retryOn": ["action_failed_retryable", "target_stale"]
      },
      "binding": {
        "attempts": [
          { "channel": "uiTree",
            "actionName": "tapElement",
            "args": { "target": { "lit": { "kind": "selector",
                                           "selector": { "identifier": "wifi_row" } } } },
            "requiresFeature": "device.semanticActions.v1",
            "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
            "protection": "standard" },
          { "channel": "coordinate",
            "actionName": "tap",
            "args": { "x": { "lit": 512 }, "y": { "lit": 384 } },
            "acceptExecutionModes": ["nativeSemantic", "webSemantic", "coordinateFallback"],
            "protection": "standard" }
        ]
      },
      "assertions": [
        { "assertId": "wifi_screen_open",
          "predicate": { "type": "elementState",
                         "selector": { "identifier": "wifi_toggle" }, "state": "visible" },
          "verifyVia": ["uiTree", "vision"],
          "visionPrompt": "Wi-Fi 设置页已打开，Wi-Fi 开关控件在屏幕上可见",
          "onMissingInput": "unknown" }
      ]
    },
    {
      "kind": "action",
      "stepId": "set_ssid:field",
      "effectHash": "sha256:3333333333333333333333333333333333333333333333333333333333333333",
      "judgeHash": "sha256:4444444444444444444444444444444444444444444444444444444444444444",
      "checkpoint": false,
      "verb": "set_value",
      "effect": "mutating",
      "idempotent": true,
      "binding": {
        "attempts": [
          { "channel": "uiTree",
            "actionName": "setElementValue",
            "args": {
              "target": { "lit": { "kind": "selector",
                                   "selector": { "identifier": "ssid_field" } } },
              "value": { "ref": "params.ssid" }
            },
            "requiresFeature": "device.semanticActions.v1",
            "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
            "protection": "standard" }
        ]
      },
      "assertions": []
    },
    {
      "kind": "assert",
      "stepId": "wifi_on_visible",
      "effectHash": "sha256:5555555555555555555555555555555555555555555555555555555555555555",
      "judgeHash": "sha256:6666666666666666666666666666666666666666666666666666666666666666",
      "checkpoint": true,
      "observe": "fresh",
      "assertions": [
        { "assertId": "toggle_enabled",
          "predicate": { "type": "elementState",
                         "selector": { "identifier": "wifi_toggle" }, "state": "enabled" },
          "verifyVia": ["uiTree", "vision"],
          "visionPrompt": "Wi-Fi 开关处于打开（enabled）状态",
          "onMissingInput": "unknown" },
        { "assertId": "ssid_listed",
          "predicate": { "type": "elementText",
                         "selector": { "identifier": "current_network_label" },
                         "match": { "value": "Pointlock-Lab", "mode": "contains", "caseSensitive": false } },
          "verifyVia": ["uiTree"],
          "onMissingInput": "unknown" }
      ]
    },
    {
      "kind": "call",
      "stepId": "ensure_session",
      "effectHash": "sha256:7777777777777777777777777777777777777777777777777777777777777777",
      "judgeHash": "sha256:8888888888888888888888888888888888888888888888888888888888888888",
      "checkpoint": true,
      "flowRef": { "flowId": "ensure_logged_in",
                   "irHash": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
      "inputs": { "username": { "ref": "params.username" } }
    },
    {
      "kind": "if",
      "stepId": "wait_if_passed",
      "effectHash": "sha256:9999999999999999999999999999999999999999999999999999999999999999",
      "judgeHash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
      "checkpoint": true,
      "cond": { "fn": "eq",
                "args": [ { "ref": "steps.wifi_on_visible.verdict" }, { "lit": "pass" } ] },
      "then": [
        {
          "kind": "action",
          "stepId": "wait_connected",
          "effectHash": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "judgeHash": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
          "checkpoint": true,
          "verb": "wait_for",
          "effect": "readonly",
          "idempotent": true,
          "timeoutMs": 30000,
          "binding": {
            "attempts": [
              { "channel": "uiTree",
                "actionName": "waitForElement",
                "args": { "selector": { "lit": { "identifier": "connected_banner" } },
                          "condition": { "lit": "visible" } },
                "requiresFeature": "device.semanticActions.v1",
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard" }
            ]
          },
          "outputs": { "matched": { "ref": "steps.wait_connected.output.matched" } },
          "outputSchema": { "type": "object",
                            "properties": { "matched": { "type": "boolean" } },
                            "required": ["matched"] },
          "assertions": [
            { "assertId": "banner_matched",
              "predicate": { "type": "expr",
                             "expr": { "fn": "eq",
                                       "args": [ { "ref": "steps.wait_connected.output.matched" },
                                                 { "lit": true } ] } },
              "verifyVia": [],
              "onMissingInput": "unknown" }
          ]
        }
      ]
    },
    {
      "kind": "human",
      "stepId": "confirm_wifi",
      "effectHash": "sha256:abababababababababababababababababababababababababababababababab",
      "judgeHash": "sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
      "checkpoint": true,
      "mode": "confirm",
      "prompt": "确认设备已连接到目标 Wi-Fi 网络",
      "presents": [ { "ref": "steps.wifi_on_visible.verdict" }, { "ref": "params.ssid" } ],
      "decisions": ["confirmed", "rejected"],
      "timeoutMs": 600000,
      "onTimeout": "unknown"
    },
    {
      "kind": "let",
      "stepId": "label",
      "effectHash": "sha256:efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef",
      "judgeHash": "sha256:1212121212121212121212121212121212121212121212121212121212121212",
      "checkpoint": true,
      "bindings": {
        "report_label": { "fn": "concat",
                          "args": [ { "lit": "wifi:" }, { "ref": "params.ssid" } ] }
      }
    }
  ],
  "handlers": [
    { "hook": "onUnknown",
      "action": {
        "kind": "escalate",
        "human": {
          "kind": "human",
          "stepId": "flow:onUnknown:escalate",
          "effectHash": "sha256:3434343434343434343434343434343434343434343434343434343434343434",
          "judgeHash": "sha256:5656565656565656565656565656565656565656565656565656565656565656",
          "checkpoint": true,
          "mode": "judge",
          "prompt": "机器无法判定本步结果，请依据证据人工裁决",
          "presents": [],
          "decisions": ["pass", "fail", "unknown"],
          "timeoutMs": 3600000,
          "onTimeout": "unknown"
        }
      },
      "maxTriggers": 1 }
  ],
  "sourceMap": [
    { "irPath": "/body/0",
      "file": "flows/wifi_toggle.flow.yaml",
      "span": { "startLine": 33, "startCol": 3, "endLine": 52, "endCol": 62 } },
    { "irPath": "/body/1",
      "file": "flows/wifi_toggle.flow.yaml",
      "span": { "startLine": 18, "startCol": 7, "endLine": 20, "endCol": 24 },
      "origin": [
        { "macro": "fill_field",
          "file": "flows/wifi_toggle.flow.yaml",
          "span": { "startLine": 54, "startCol": 3, "endLine": 57, "endCol": 32 } }
      ] }
  ],
  "subflows": {
    "ensure_logged_in": {
      "flowId": "ensure_logged_in",
      "irHash": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    }
  }
}
```

读这份 IR 时值得指认的几个细节：`set_ssid:field` 是宏卫生展开的合成 stepId（`:` 前缀，§3/§7），其 `checkpoint: false` 是宏展开体内的物化默认（§7）、`assertions: []` 是 unverified 演示（R4）；`open_wifi_settings` 的双 attempt act-chain 与 coordinate 静态坐标——注意其 uiTree attempt 的白名单**不含** `coordinateFallback`（§5.1 规则 6：白名单只由 attempt 自身通道决定，作者声明坐标 attempt ≠ 授权语义 attempt 内部降级）；`tapElement`/`setElementValue`/`waitForElement` 的 `args` 逐字对应 DeviceRail 实参 schema（`target: ElementTarget`、`selector` + `condition`）；`wait_connected` 的 `outputs` self-ref（指原始输出）与 `banner_matched` 断言内的 self-ref（指投影后输出、`asserting` 阶段求值）分别是 §4.1.1 两处特例的实例；`wifi_screen_open`/`toggle_enabled` 的 `visionPrompt` 是 vision 降级提示词的 IR 落点（§5.3）；`banner_matched` 是 `verifyVia: []` 的 expr 谓词；`sourceMap[1].origin` 是宏 `fill_field` 的展开残留（span 指宏定义体，origin 指调用点）——IR 里已没有宏本身。

---

## 14. Schema 之外的语义校验清单（谁在哪个阶段拒绝）

JSON Schema 只能封住形状；下列约束由编译阶段承担，列出以防"schema 过了就算合法"的误解：

| 约束 | 阶段 |
|---|---|
| `stepId` flow 内全局唯一（含 if/foreach 嵌套体与 handler 合成 id） | `check` |
| ref 只引用同 flow 体内、拓扑在前的 step；数据依赖图无环 | `check` |
| `vars.*` SSA 单赋值；`iter.<as>` 仅 foreach 体内可见 | `check` |
| PureFn 参数类型；`jsonPath` 路径与 `regexMatch` pattern/flags 必须 LitExpr | `check` |
| `unknown` 类型（invoke 无 outputSchema、jsonPath 结果）取字段前必须 `expect_schema` 收窄 | `check` |
| self-ref 作用域（§4.1.1/§8.3）：本步 `outputs` 内合法（指原始输出）、本步 `assertions` 的 `expr` 谓词内合法（指投影后输出）；其余位置（`args`/`inputs`/`cond`/`items`/`bindings`/`presents`/`preflight`）self-ref 非法 | `check` |
| handler 环检测；宏递归 | `check` / `normalize` |
| `actionName` 存在于 lockfile.device.actions；args 过该 action 的 inputSchema | `bind` |
| vision 仅出现在 `verifyVia` 链尾；coordinate attempt 携带字面量静态坐标 | `bind` |
| protected action 拒绝（v0.1）；feature 归集入 `requiredFeatures`；`acceptExecutionModes` 推导 | `bind` |
| 每个 `flowRef` 在 `subflows` 有条目且 irHash 一致；callee provider/feature 兼容；调用图 DAG | `normalize` / `bind` |
| 跨 documentEpoch 的 `UiNodeRef` 引用注入 revalidate step | `bind` |
| `irHash`/`effectHash`/`judgeHash` 与内容一致（复核） | `seal` 产出；`pointlock inspect` 与 runner 装载时重算比对 |

runner 装载 IR 时执行的检查恰好三条：schema 校验、`irVersion` 精确匹配、三类哈希重算比对——全部通过才进入 attestation 与执行。其余一切信任编译器，这是"IR 是契约"的另一面：契约窄，才守得住。
