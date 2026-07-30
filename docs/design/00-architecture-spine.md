# Pointlock 架构骨架（Architecture Spine）

> **文档地位**：本文档是 Pointlock 后续全部细化文档（8 份）的唯一依据。任何细化文档与本文冲突时，以本文为准；修改本文需走架构评审。
>
> **事实来源声明**：原定必读输入《需求与目标》与《DeviceRail 真实接口事实报告》在编排时路径未注入（字面量 `undefined`）。本文中全部 DeviceRail 事实（方法名、action 名、feature 名、错误码、schema 字段、枚举值）已由评审人逐项对照 DeviceRail 源仓库 `/Users/dengfengwang/Codes/projects/device-rail` 核实：`protocol/README.md`（Protocol 1.5，24 个方法 + 2 个 stream 通知，174 个 schema）、`protocol/schema/v1/*.schema.json`、`crates/protocol/fixtures/`（golden fixtures：方法名、错误码、事件 payload 类型）、`packages/client/src/`（客户端错误类与导出）。
>
> **需求基线声明**：《需求与目标》已入库为 [`docs/requirements.md`](../requirements.md)（其 §2–§4 为需求方**逐字原文**，2026-07-16 回填，取代当日早先的「标题重建版」；见其 §7 变更记录），是全系列「产出 N / 原则 N」编号与需求措辞的**唯一权威基线**。需求条目与文档章节的双向追踪矩阵见本文**附录 B**；矩阵与各文档页眉的「覆盖需求产出 N」自报声明冲突时，以附录 B 为准。需求措辞的一切偏离必须登记在 `docs/requirements.md` §5 偏离登记表，禁止再以各文档就地自曝的方式分散记录。十条设计原则见 §1.0（原文在 requirements.md §3；§1.0 为架构化转述，措辞以原文为准）。

---

## 0. 评审记录：三方案裁决摘要

| 方案 | 视角 | 最强贡献 | 最大缺陷 | 裁决 |
|---|---|---|---|---|
| 方案 1 | schema-first | 过程诚实：在输入缺失时拒绝虚构（与原则 4/5 的精神一致），并准确诊断了编排脚本缺陷 | 零架构产出；未像另两方案那样以 DeviceRail 源仓库为地面真值完成任务 | 不采用（无内容可采）；其「fail-closed 面对缺失输入」的立场被吸收为编译器与 runner 的一贯态度 |
| 方案 2 | runtime-first | **执行语义宪法**：RunLog 事件溯源 + act 前 WAL 意图（`actionIntent`）+ 借 `device.execute` 的 caller-UUID 与 `events.list` 做崩溃后效果核对（reconcile）+ effectHash/judgeHash 双哈希使「改断言不重跑设备」成为一等操作 + resolvedInputs 快照 + Evidence 本地化（规避 `ui.snapshot.get` 仅限本 Session 读取的协议限制）+ resume 世界探针 | capability 层单薄：编译期能力绑定缺少离线快照机制（无 lockfile，编译隐含要求设备在线或来源不明的 profile）；fallback 模型只有单条 `degradeTo`，未区分定位链与验证链，对 DeviceRail 内部 `coordinateFallback` 的处置只有粗粒度 fail-closed | **主干（runner / IR 哈希 / checkpoint / resume）** |
| 方案 3 | capability-first | **能力宪法**：静态 manifest + capability lockfile + 运行期 attestation（编译不需设备在线，`FeatureOffer.required` 白送强制力）；通用动词表 = 协议五件套忠实投影 + `invoke` 逃逸门；act-chain / verify-chain 双链拆分并在类型层禁止 vision 定位；`acceptExecutionModes` 对 daemon 内部 `coordinateFallback` 的「未授权降级 → 强制全量验证 → 否则 unknown」处置；`documentEpoch` 失效在编译期注入 revalidate | 持久化与修复语义有正确性漏洞：checkpoint 无动作意图 WAL，崩溃在 `acting` 中段时无法判定 mutating 动作是否已发生；resume 要求 `contentHash` 严格相等——任何修复（哪怕只改断言）都使历史失效，直接违背「局部修复后从断点继续」的核心需求。另有小误：`SessionOutcome` 实为四值（漏 `shutdown`） | **主干（Provider 契约 / capability binding / fallback 双链）** |

**合成原则**：以方案 2 的运行时宪法为脊柱、方案 3 的能力宪法为脊柱，二者在 IR 处会合。关键冲突裁决：

| # | 冲突 | 裁决 | 理由 |
|---|---|---|---|
| R1 | resume 对齐：双哈希（方案 2） vs contentHash 严格相等（方案 3） | 采方案 2 的 effectHash/judgeHash 双哈希 + 五类对齐分类 | 严格相等使一切修复丢失历史，违背局部修复需求；断言是纯函数，离线重判是免费的 |
| R2 | 崩溃安全：WAL 意图 + reconcile（方案 2） vs 仅 settle 点 checkpoint（方案 3） | 采方案 2；checkpoint 中并存方案 3 的 `{sessionId, lastSequence}` 事件游标水位 | `device.execute` 携带 caller UUID 且终态事件有落盘 shield，是协议白送的 effectively-once，不用是浪费 |
| R3 | fallback 模型：单 degradeTo（方案 2） vs act/verify 双链（方案 3） | 采方案 3 双链；保留方案 2 的 `degraded` 标记与折叠规则（strict 策略下 degraded pass 折叠为 unknown） | 双链在类型层封死「vision 定位」，是原则 7 的结构性保证 |
| R4 | 无断言 mutating step 的 verdict：不产生 verdict（方案 2） vs 弱 pass `unverified`（方案 3） | 采方案 2：不产生 verdict；报告层借用方案 3 的 `unverified` 标注，但它是执行状态注记而非 verdict | 原则 3：action 成功 ≠ 语义通过；`unverified` 若是 verdict 会污染折叠 |
| R5 | 断言求值与降级链的关系 | 采方案 3：完成求值的否定即终局，降级链只救 unavailable 不救 fail | 防止 vision 被用于推翻结构化通道的明确否定（刷绿激励） |
| R6 | protected action | v0.1 在 bind 阶段 fail-closed 拒绝（方案 2）；`secrets.*` 不透明句柄 + `protected: true` 显式声明作为已承诺的 v0.2 路径（方案 3 设计原样保留，词汇表预留） | 小团队 v0.1 减面；但词汇表现在就锁定，防止 8 份文档各自发明 |
| R7 | 动词层：canonical verbs（方案 3） vs 直呼原生 action 名（方案 2） | 采方案 3 动词表（8 个动词），但 `lower()` 从「manifest 内代码」降级为**声明式字段映射**；编译后动词从 IR 消失，IR 只有原生 `actionName` | 五件套动词与原生 action 一一对应，无需可执行变换；编译器零代码执行 |
| R8 | 表达式定界符：`${...}`（方案 2） vs `${{ ... }}`（方案 3） | 采 `${{ ... }}` | 与普通字符串中的 `${}` 视觉冲突更小，作者群体熟悉（GH Actions 惯例） |
| R9 | 探针关键字：方案 2 的 `expects` 与方案 3 的 `expect`（断言）拼写相撞 | 后置断言 = `expect`；前置/resume 探针 = `preflight` | 一字母之差的两个关键字是文档漂移的温床，必须改名 |
| R10 | handler 有无数据输出 | 采方案 3：handler 不产生可被数据流引用的输出，只产生处置决定；采方案 2：handler 执行留 StepRecord 审计痕（路径带 `hook:` 帧） | 错误路径进数据流破坏 SSA 与 resume 确定性；但审计不可少 |
| R11 | `session.end` outcome 枚举 | 四值 `completed \| failed \| cancelled \| shutdown`（schema 实测） | 修正方案 3 的三值签名 |

### 0.1 修订记录（骨架定稿后的裁决追加）

| # | 修订 | 日期与状态 | 裁决理由（一句） |
|---|---|---|---|
| R12 | 技术栈修订：Rust 核心 + TS UI 混合 monorepo（`crates/` + `packages/` + `schema/`，布局镜像 DeviceRail）；类型真相源反转为 `pointlock-ir` 的 Rust DTO（§1.1/§1.2/§3/§4/§8/附录 A.1/A.8） | 2026-07-16，经需求方确认采纳 | 分发面（`pointlock` CLI 交付单一静态二进制）是选 Rust 的首要理由，且直接复用 DeviceRail 已验证的 schema 生成管线；性能不是理由——runner 是 I/O 密集型，语言速度非决策依据 |
| R13 | 监督式人机协作四项收编：supervised run（`--supervise`，§6.9）、humanRequested/humanResponded 的 `purpose` 判别子（§6.1/附录 A.4）、默认升级姿态（on_unknown → escalate 模板 + 编译器 lint 警告）、编译期问询（elicitation）与 LLM 修复提议循环 | 2026-07-16，经需求方确认采纳 | 「拿不准就问人」从 opt-in 变为默认姿态；监督/问询复用既有 human 管道（同一 store 仲裁、同一通知通道、同一收件箱）与 WAL 语义，不新建第二套机制 |
| R14 | 投影协议（Projection Protocol）成为 UI 与 runner/store 之间的正式对接面：五族渲染器无关只读投影 DTO（`FlowGraphView` `RunTimelineEntry` `StepDossierView` `HumanInboxEntry` `RunOverview`，§10）+ `projectionVersion` + 传输中立三形态；Rust 真相源落 `pointlock-store` projection 模块，CI 生成 `@pointlock/projection-types`（TS package 计数 3 → 4）；08 篇 React Flow node/edge type 字面量从协议命名降级为 `@pointlock/ui` 内部 adapter 实现细节（§1.1/§1.2/§10/附录 A.1/A.3/A.4/附录 B） | 2026-07-16，需求方提出并确认方向 | 界面层技术选型波动大于内核，协议先行使渲染器可替换 |
| — | 开放问题批量闭环（非新裁决）：wait_for 超时未命中 = `succeeded` + `matched:false` 源码实证（03 §6、04 §6.2）；schema 语义等价判据 = golden fixture 语料行为等价（02 §1.1、08 §6.1）；FlowGraphView `data` 边 v0.1 削除、v0.2 additive 候选（§10.1、08 §3.1）；`RunOverview` additive 扩 per-step 状态摘要（§10.1、08 §5） | 2026-07-17 | M0 开工前的最后收口，均为既登记开放问题的就地闭环 |
| — | M1 批量收编（条目 1–11）：运行时共享类型定义域归 `pointlock-ir`（§1.2/A.3）；`ReconcileResult::completed` 改携完整 `ActionOutcome`（§4.2、04 §5）；run 表补 `binding` 列（07 §3.3）；lockfile digest 域标签 `pointlock-lockfile/1`（§4.1）；`ProviderError` 收编（§4.2/A.3/A.4）；`requiresConfirmation.cause` 封闭集（A.4、07 §5.4）；CLI 退出码表（A.2）；测试 provider 装配裁决——FakeProvider 以 `devicerail` 名注册（04 §8）；`stepEntered`/`stepExited` payload 定形为 StepRecord 事件载体（§6.1、07 §3.3）；`afterSequence` 初始水位以省略表达（04 §9.8.3）；`device.semanticActions.v1` 与 `observation.uiSnapshot.v1` 归集捆绑（§8、04 §9.2） | 2026-07-17，M1 批量收编 | M0 实现与 devicerail-client 验收的反哺 |
| — | M3a 批量收编（实现反哺）：投影 host 形态裁决——原 08 §1「薄 Node host」与 R12 矛盾，裁决为 **Rust CLI 即投影 host**（`pointlock inspect --serve` 内建 tiny_http loopback host，`--ui <dist>` 托管纯静态 SPA 产物，数据面 token 门控，修复闭环 = 同一二进制自执行；Node ≥ 22 降为 `packages/` 构建期约束，运行时零 Node 依赖——08 §1 就地修订）；`pointlock-vision` 首个可用实现 = `AnthropicVisionVerifier`（raw HTTP Messages API，PASS/FAIL/UNKNOWN 单行应答协议，一切失败模式折叠为 `unknown`，CLI `--vision <off\|anthropic>` per-segment 注入，默认 `off`）；**登记开放问题（v0.2 评估）**：checkpoint 物化节奏——I1 的每事件物化使单次 append 成本 O(view)、全 run O(n²)（增量折叠缓存已消去重读重折项，剩余项是 canonical 序列化本身），10k+ 事件 run 生成分钟级；候选方向为步界物化或视图增量序列化，任何变更须过 I1 自检对照；`pointlock report` 收口交付（08 §6.2 原排 M1、§6.3 补 human/handler 维度原排 M2，实际于 M3a 收口一并清账：`unverified` 注记 R4、unknown 独立计数 06 §7.3、supersedes 谱系计数、逐段 supervise/对齐摘要；`--format json` 为 CLI 自有形 `pointlockReport: 1`，不入投影五族封闭清单）；`ObservationRecord.viewport` additive 补齐（W1 登记缺口关闭）——additive 为单向：旧读者对含新字段的账本 fail-closed 拒读（R12 严格 serde），二进制降级读新 store 不受支持；非有限 `scaleFactor` 在 SPI 摄入处隔离为 `observation_viewport_invalid` 终局失败（serde_json 将非有限 f64 写为 `null`，绝不许其进账本） | 2026-07-18，M3a 批量收编 |
| — | **收编评审四项裁决**（四路读档→提案→对抗核查后定形；全部为 §6.1 payload 字段追加 + 投影 additive 字段，无新事件类型、无 projectionVersion bump，兼容一律 viewport 先例的单向 additive）：① **providerStateSummary 收编**——07 §2.2 六字段形状进 `stepExited`/`runSuspended` payload（两处 Option 化偏离：`eventCursor` 采集失败缺席、`platform` 装配未供缺席——绝不冒充，原则 4）；写入判据内涵式（退出时生效 verdict fail/unknown + 每次 runSuspended 含 resume 前置阻塞；aborted 后置退出裁定不写）；`health()`+`currentCursor()` 共用采集期限，失败不阻塞挂起；消费面 = 卷宗 FrameEnvironment + RunOverview（字段名 `lastSuspensionProviderStateSummary` 语义，仅 suspended/awaitingHuman 态呈现）；**纯法证**——reconcile/fold/对齐永不消费。② **attempt 链位联接**——`actionIntent` 增 `chainIndex`（1 基）/`channel`/`actionName`；AttemptRecord 同名 additive 字段（fold 内存携带 callId 键控，PendingIntent 持久形不扩）；handler retry/repaired 再入裁定**从链头重启**（§6.5「从 acting」+ 07 §1.4 类推；binding.attempts 是作者偏好序非消耗品）；轮界 = 帐面 handlerTriggered（chainIndex 单调性不可判轮界）；**登记既存缺陷待修**：崩溃恢复 Adopt/Replay 现从枚举位 0 再入，中链 intent 的 Replay 会以错位实参派发——裁定按 07 §1.4「落回精确位置」以 `chainIndex-1` 再入（旧账本无字段回退现行为）；运行叠加经 RunOverview.steps.actChainMarks（08 §3.4 修订）。③ **结算/verdict 证据裁决**——「即取本地化、引用即落地、判定时清单」：`actionSettled` 终局记录不扩（WAL 锚是 actionIntent）；`verdictRecorded` 增 `localized: EvidenceRef[]` + `localizationGaps: EvidenceGap[]`（观测类除外）；fold 将 localized 并入 StepRecord.evidence（去重键 (sha256, assetId) 首现胜，两面同规）；离线重判清单恒空；卷宗增 evidence_gaps + AttemptView.evidence（声明面原样）+ VerdictRecordView.localized/gaps；report 如实渲染缺口行；**并修既存缺陷**：uiSnapshot 引用在本地化成功前无条件计入 cited（引而不落即静默缺口）——改为成功后计入。④ **sessionLineage 逐段**——`runResumed` 增 `eventCursor?`（新代重置水位，reconcile 决断后、落账前取值）；fold 收 lineage_known 规则（缺席置假不复原，此后 intent 折叠 `issuingCursor` 缺席 =「签发代不可知」，绝不 gen-1 冒充）；签发凭据实现落 **harvest 侧逐 intent 精确态**（FromBinding/Known/Unknown——后续携 cursor 的段自证其代，不受先前未知段拖累；持久 `PendingIntent` 形**不扩**，存量 store 的 I1 对照零风险——对原「PendingIntent.issuingCursor additive」表述的实现反哺修正）；SPI `reconcile(callId, issuing)` 签名修订（§4.2/04 §5）；devicerail 守卫修既存缺陷（今日扫新 session 日志可产假 neverDispatched → 签发≠当前且不可达 → logUnavailable）；07 §3.1 step-完成行 cursor 刷新缓期注记。 | 2026-07-18，收编评审 | 三个投影缺口 + sessionLineage 的裁决一次定形；对抗核查揪出并登记三处既存缺陷（uiSnapshot 引用泄漏、devicerail 跨代 reconcile 扫错日志、Replay 中链错位实参）随波修复 | M3a 四波（投影协议/serve host/UI SPA/修复闭环+vision）实现的反哺；「实现喂养设计」纪律的例行收口 |

### 1.0 十条设计原则（本文一切规则的上位约束；对应 `docs/requirements.md` §3 的 P1–P10 原文，此处为架构化转述，措辞以原文为准）

1. YAML 是界面，不是执行协议。
2. Runner 只执行 Typed IR。
3. Action / Assertion / Verdict 分离。
4. 无法确认就输出 unknown，绝不猜。
5. Capability-bound 编译：能力缺失是编译错误。
6. Fallback 必须显式声明，runner 绝不即兴降级。
7. 视觉只能是降级验证，不能做主验证或定位。
8. 人机协作是正式节点。
9. 子流程（Subflow）是一等公民。
10. 第一版轻量：单进程、本地存储，不上 Temporal / 队列。

---

## 1. 分层架构与包结构

### 1.1 分层图

```
┌─────────────────────────────────────────────────────────────────┐
│  pointlock-cli（单一静态二进制）                                     │
│      lock | compile | run | resume | inspect | locate | report   │
├───────────────────────────┬─────────────────────────────────────┤
│  编译面（离线，无设备）      │  运行面（在线，单进程）               │
│                           │                                     │
│  pointlock-compiler        │  pointlock-runner                    │
│   parse → normalize →     │   状态机 / 调度 / verdict 折叠 /      │
│   check → bind → seal     │   resume 对齐 / handler 引擎         │
│        │                  │        │              │             │
│        │ 消费              │        │ 读写          │ 经 SPI 调用  │
│        ▼                  │        ▼              ▼             │
│  ProviderManifest +       │  pointlock-store    pointlock-        │
│  CapabilityLockfile       │  RunLog/Checkpoint provider-kit(SPI) │
│  （digest 进 IR）          │  Evidence 内容寻址  ▲    ▲           │
├───────────────────────────┴────────────────────│────│───────────┤
│              共享内核：pointlock-ir  pointlock-expr                 │
├─────────────────────────────────────────────────│────│───────────┤
│  pointlock-provider-devicerail ──────────────────┘    │           │
│    （实现 SPI；依赖 devicerail-client Rust crate，待实现，M1 前置） │
│  pointlock-vision（VisionVerifier，仅 verify 角色）────┘           │
│  pointlock-human-cli（humanRequested 的呈现与回应通道）             │
└─────────────────────────────────────────────────────────────────┘
                         │ NDJSON / JSON-RPC 2.0
                         ▼
              DeviceRail daemon（Protocol 1.5）
```

以上全部为 Rust crate（`crates/`，Cargo workspace）。信任边界外的 TS 侧（`packages/`，pnpm workspace）见 §1.2 表末四行：`@pointlock/ir-types`（生成物）、`@pointlock/projection-types`（生成物，R14）、`@pointlock/ui`、`@pointlock/nl-drafter`。生成物与 golden fixtures 落 `schema/`——仓库三分布局镜像 DeviceRail（R12）。

### 1.2 crate / package 结构与依赖方向（单调向下指向 `pointlock-ir`）

| 单元（Rust crate，`crates/`） | 职责 | 依赖 |
|---|---|---|
| `pointlock-ir` | 全部 IR 类型（Rust DTO，serde + schemars，**类型唯一真相源**）、规范化、哈希（irHash/effectHash/judgeHash）、RunPath、sourceMap；CI 由此生成 JSON Schema（Draft 2020-12）、`@pointlock/ir-types`、golden fixtures。运行时共享类型（`ActionOutcome`/`Observation`/RunLog 事件/`StepRecord`/`CheckpointView` 等 wire 与持久化形状）同样定义于本 crate（类型真相源），由 provider-kit/store/runner 消费（M1 收编） | 零运行时依赖 |
| `pointlock-expr` | 表达式 AST、求值器、静态类型检查 | ir |
| `pointlock-provider-kit` | Provider SPI（Rust trait，in-process 权威形态；stdio JSON-RPC sidecar 适配形态预留 v0.2）、ProviderManifest / CapabilityLockfile 类型、一致性测试套件、FakeProvider | ir |
| `pointlock-compiler` | §8 编译五阶段；只读 manifest/lockfile，**不依赖任何具体 provider 运行时** | ir, expr, provider-kit |
| `pointlock-store` | RunLog + Checkpoint（SQLite WAL，rusqlite；WAL + `synchronous=FULL` 语义逐条不变）+ Evidence 内容寻址文件区 + projection 模块（R14：五族投影 DTO 的 Rust 定义与只读查询层——store 是读侧权威，`pointlock locate` 查询层同归属；schemars 生成 JSON Schema 与 `@pointlock/projection-types`，见 §10） | ir |
| `pointlock-runner` | 状态机、verdict 折叠、resume/对齐、handler 引擎；provider 由装配层注入 | ir, expr, store, provider-kit |
| `pointlock-provider-devicerail` | SPI 的 DeviceRail 实现 | provider-kit, `devicerail-client`（Rust crate，DeviceRail 仓库实现、经 path/git 依赖消费，**待实现，M1 前置**） |
| `pointlock-vision` | VisionVerifier 插件接口 + 默认实现（v0.1 可为返回 unknown 的 stub） | provider-kit |
| `pointlock-human-cli` | humanRequested 事件的 CLI 呈现/回应 | store |
| `pointlock-cli` | 装配一切；交付为单一静态二进制 | 以上全部 |

| 单元（TS package，`packages/`） | 职责 | 依赖 |
|---|---|---|
| `@pointlock/ir-types` | 由 `pointlock-ir` Rust DTO 经 CI 生成的 type-only 包（角色镜像 `@devicerail/protocol`），禁止手写修改 | 生成物，零依赖 |
| `@pointlock/projection-types` | 由 `pointlock-store` projection 模块的投影 DTO（§10，R14）经 CI 生成的 type-only 包（管线与 `@pointlock/ir-types` 同款），禁止手写修改；M3a 前置产物（M0 不要求） | 生成物，零依赖 |
| `@pointlock/ui` | web UI（08 篇；R12 正式收编）；与 store 之间只经投影协议（§10，R14），React Flow 映射是其内部 adapter | `@pointlock/projection-types`, `@pointlock/ir-types` |
| `@pointlock/nl-drafter` | NL 起草器：只产 YAML 草稿与结构化问询（§6.9 收编 2），在信任边界外，编译器是唯一执法者；选 TS 以用 LLM 生态 | `@pointlock/ir-types` |

硬规则：`compiler` 与 `runner` 互不依赖（共享物只有 ir）；runner 的入口签名只接受 `FlowIR`，不接受字符串（原则 1/2 的结构性保证）；具体 provider 只在 CLI 装配层出现。技术栈（R12）：混合 monorepo，布局镜像 DeviceRail——`crates/`（Cargo workspace，Rust）+ `packages/`（pnpm workspace，TS）+ `schema/`（生成物与 golden fixtures）。Node ≥ 22 约束只保留在 packages/ 一侧（ui、nl-drafter 及 ir-types / projection-types 消费方）；`pointlock` CLI 交付为单一静态二进制——**分发面是选 Rust 的首要理由，性能不是**（runner 是 I/O 密集型，语言速度非决策依据）。

---

## 2. 十三个核心概念的权威定义

| # | 概念 | 权威定义 | 存在时机 |
|---|---|---|---|
| 1 | **Flow** | 编译产物中的可执行单元：带 params/outputs 契约的 Step 有序结构（v1 为顺序 + 有限控制结构），有内容哈希 `irHash`。一次执行（Run）= 对一份 FlowIR 的一次留痕遍历，绑定恰好一台设备、一条活跃 DeviceRail Session（断代重开时形成 session lineage） | 编译期产生，运行期只读 |
| 2 | **Step** | 最小可调度、可 checkpoint 的执行单元，有唯一 `stepId`、生命周期状态机、至多一个 Verdict。action step 内部是固定流水线 `preflight? → act → observe → assert` | IR + 运行期 StepRecord |
| 3 | **Subflow** | 被当作一个 step 调用的 Flow，一等公民：显式 input/output 契约（call-by-value）、独立编译与版本化（按 `irHash` 锁定）、运行期有调用帧（call frame）、可独立测试、作用域封闭（callee 只见显式 inputs） | 编译期链接，运行期有身份 |
| 4 | **Macro** | 编译期模板：参数化 step 序列，`normalize` 阶段卫生展开（hygiene 重命名）后**彻底消失**——无运行期身份、无独立 verdict、无独立 checkpoint；只留 origin trace（展开链）供 sourceMap 把 IR 路径映射回 YAML 源。禁递归 | 仅编译期 |
| 5 | **Handler** | 挂在明确钩子（`onFail` / `onUnknown` / `onError` / `onResumeDrift`）上的显式策略，由 runner 状态机在特定转移上触发，**不出现在正常控制流**。产物是处置决定（disposition），**没有可被数据流引用的输出**；执行留 StepRecord 审计痕（RunPath 带 `hook` 帧）。`maxTriggers` 防循环 | IR 声明，运行期触发 |
| 6 | **Provider** | 把 Pointlock 抽象执行面绑定到外部基座的适配器，由两部分构成：**静态 manifest**（编译器消费）+ **运行时 adapter**（runner 经 SPI 消费），同包发布、必须一致。v0.1 唯一实现：`devicerail` | 编译期 + 运行期 |
| 7 | **Capability** | Provider 声明的可被编译器绑定的能力，三类：**feature**（透传 DeviceRail feature id，如 `device.semanticActions.v1`）、**action**（带 JSON Schema 的 `ActionDefinition`，`protection: standard \| protected`）、**channel**（定位/验证通道 `dom \| uiTree \| vision \| coordinate`）。编译期以 CapabilityLockfile 快照消费，运行期以 attestation 复核 | 编译期快照，运行期复核 |
| 8 | **Action** | 一次改变或探查世界的意图，经 Provider 执行，以调用方生成的 UUID（`callId`）为身份。对 DeviceRail 即一次 `device.execute { id, name, arguments, actionTimeoutMs }`。终态四分：`succeeded \| failed \| cancelled \| timedOut`，不折叠 | 运行期 |
| 9 | **Observation** | Provider 捕获的、**不含判断**的时点世界快照。DeviceRail `Observation`：`{ id, deviceId, capturedAtMs, viewport, screenshot?, screenshotOmission?, uiSnapshot?, uiSnapshotOmission?, metadata? }`。**可以合法缺料**（omission 是类型化原因，不是错误），缺料传导为断言输入的 unknown 条件。生命周期短：其中 `UiNodeRef` 绑定 `documentEpoch`，导航/重连后失效 | 运行期，进 RunLog |
| 10 | **Assertion** | 对 Observation / Action output 求值的**纯函数**谓词（编译产物 `AssertionIR`），产出 `pass \| fail \| unknown` + 理由。是「问题」不是「答案」；可离线对存档 Observation/Evidence 重新求值——这是修复对齐（§6.7）的基石 | IR 声明，运行期求值，可事后重求值 |
| 11 | **Evidence** | 内容寻址、可归档的证据实体：DeviceRail `AssetRef { id, mediaType, uri, sha256? }` 经本地化（按 sha256 拉入本地库）后的 `EvidenceRef`，以及 Pointlock 自产的求值 trace。Observation *引用* Evidence；Verdict 只允许引用 Evidence（审计时 Observation 已死，Evidence 永生）；Evidence 自身不含语义判断 | 运行期落盘 + 归档 |
| 12 | **Verdict** | 对一个 step 的三值终审：`{ status: pass\|fail\|unknown, degraded, summary, evidence[] }`，由 runner 的确定性折叠规则产生（human judge 节点则由人产生），经 `verdict.record` 回写 DeviceRail（daemon 只校验持久化，不运行断言、不推断升级）。一旦记录即历史事实；重判产生**新** Verdict 并标注 `supersedes`，旧的不删 | 运行期产出并持久化 |
| 13 | **Checkpoint** | RunLog 在安全点的确定性物化视图：足以让 resume 正确继续的最小闭包（§6.5）。永远可由 RunLog 重建；粒度 = step 边界 + act 前 WAL 意图点 | 运行期持续维护 |

### 2.1 三组易混概念的钉死辨析

**Macro vs Subflow vs Handler** —— 三问：
1. *运行期有没有身份？* Macro 无（编译期蒸发）；Subflow 有（栈帧、checkpoint 边界）；Handler 有（但被动触发，路径带 `hook` 帧）。
2. *有没有数据流签名？* Macro 只有编译期参数；Subflow 有完整 typed inputs/outputs；Handler **没有输出**，只产处置决定。
3. *能不能独立判定？* Macro 不能（展开出的 steps 各自判定）；Subflow 的 call step 有聚合 verdict；Handler 不产生 verdict，只影响宿主 step 的 verdict 走向。

**Assertion vs Verdict** —— Assertion 是函数（问题），可对同一谓词多次求值、可离线重判；Verdict 是判决书（答案），一次性终审、append-only、重判走 `supersedes`。单条 assertion 求值**未能完成**（缺料）≠ fail，= unknown。

**Observation vs Evidence** —— Observation 是「此刻世界长什么样」的结构化声明，短命（documentEpoch）；Evidence 是被 SHA-256 锚定的字节实体，耐久。Observation 派生零或多条 Evidence；Verdict 只引用 Evidence。

### 2.2 概念关系图

```
Flow ──contains──> Step ──preflight──> AssertionIR(探针)
  │                  │──act──> ActionBinding ──attempts──> Action(callId)
  │                  │            └─(经 Provider.execute)─> ActionOutcome
  │                  │                 └─ ActionResult{output, before/after Observation, evidence[], execution.mode}
  │                  │──observe──> Observation ──references──> Evidence
  │                  │──assert──> AssertionIR ──evaluates(沿 verify-chain)──> pass|fail|unknown
  │                  └──fold──> Verdict ──cites──> Evidence ──(verdict.record)─> DeviceRail Session
  │──calls──> Subflow(=Flow + contract, 按 irHash 锁定)
  │──(编译期已消化) Macro
  └──hooks──> Handler ──yields──> Disposition{retry|continue|escalate|abort|repair}
Provider ──declares──> ProviderManifest ──(pointlock lock)──> CapabilityLockfile ──binds──> 编译
Run ──appends──> RunLog ──materializes──> Checkpoint
```

---

## 3. Typed IR 核心类型（`pointlock-ir`）

> **类型真相源（R12）**：本节及全系列文档的 TS 记法保留为**规范记法**；类型唯一真相源是 `pointlock-ir` 的 Rust DTO（serde + schemars）。CI 由 DTO 生成 JSON Schema（Draft 2020-12）、`@pointlock/ir-types` 与 golden fixtures（复制 DeviceRail 已验证的管线）。现有 `schema/flow-ir.v0.1.schema.json` 降级为**验收基线**：M0 的生成物必须与之 diff 评审，语义等价后基线随生成物滚动。`irVersion` 不 bump——IR 形状不变，仅真相源与实现语言变化。

```ts
// ─── 基元 ────────────────────────────────────────────────────────
export type Hash = string;          // "sha256:<hex>"
export type StepId = string;        // 作者提供、flow 内唯一、稳定（修复时不改 id 即保身份）
export type EffectClass = "mutating" | "readonly" | "pure";
export type Channel = "dom" | "uiTree" | "vision" | "coordinate";
export type ExecutionMode = "nativeSemantic" | "webSemantic" | "coordinateFallback";

// ─── Flow ───────────────────────────────────────────────────────
export interface FlowIR {
  irVersion: 1;
  flowId: string;
  irHash: Hash;                     // 规范化全树哈希（含 subflow 引用的 irHash，形成联编闭包）
  provider: { name: "devicerail"; version: string };
  requiredFeatures: string[];       // 全流程 feature 并集 → 运行期进 FeatureOffer.required
  lockfileDigest: Hash;             // 绑定时使用的 CapabilityLockfile.digest
  params: ParamDecl[];              // 带 JSON Schema 的输入契约
  outputs: OutputDecl[];
  body: StepIR[];
  handlers?: HandlerBinding[];      // flow 级钩子
  verdictPolicy: "standard" | "strict";  // strict: degraded pass 折叠为 unknown
  sourceMap: SourceMapEntry[];      // IR path -> YAML span + macro 展开链(origin trace)
  subflows: Record<string, { flowId: string; irHash: Hash }>; // 引用不内联
}

export interface ParamDecl  { name: string; schema: unknown; required: boolean; default?: unknown }
export interface OutputDecl { name: string; schema: unknown; from: Expr }

// ─── Step（closed vocabulary：7 种 kind）─────────────────────────
export type StepIR =
  | ActionStepIR | AssertStepIR | CallStepIR | HumanStepIR
  | IfStepIR | ForeachStepIR | LetStepIR;

export interface StepBase {
  stepId: StepId;
  effectHash: Hash;   // 规范化(act/binding + 实参表达式 + effect) 的哈希 —— 对齐用
  judgeHash: Hash;    // 规范化(preflight + observe + assertions + verifyVia + verdict 相关) 的哈希
  preflight?: AssertionIR[];        // 前置探针：resume 漂移检测 & 执行前置（原 expects，改名防撞）
  retry?: RetryPolicy;              // 只作用于 act 阶段
  timeoutMs?: number;
  handlers?: HandlerBinding[];      // step 级覆盖 flow 级
  checkpoint?: boolean;             // 默认 true；macro 展开体内默认 false
}

export interface ActionStepIR extends StepBase {
  kind: "action";
  verb?: CanonicalVerb;             // 仅元数据（报告用）；runner 无 verb switch
  effect: EffectClass;              // mutating | readonly
  idempotent?: boolean;             // 作者声明；reconcile 不确定分支的重放许可
  binding: ActionBinding;           // 编译期完全绑定的 attempt 链（act-chain）
  assertions: AssertionIR[];        // 后置断言；可空 → 本步无 verdict（见 §6.3）
  outputs?: Record<string, Expr>;   // 从 ActionResult.output / Observation 元数据抽取
  outputSchema?: unknown;
}

export interface ActionBinding {
  attempts: BoundAttempt[];         // 有序、封闭；没写 fallback 就只有一项（原则 6）
}

export interface BoundAttempt {
  channel: Channel;                 // vision 永不出现在这里（act-chain 类型约束）
  actionName: string;               // 原生名，如 "tapElement"（以 lockfile.device.actions 为准）
  args: Record<string, Expr>;       // 编译期已按该 action 的 inputSchema 做形状校验
  requiresFeature?: string;         // 如 "device.semanticActions.v1"
  acceptExecutionModes: ExecutionMode[]; // daemon 内部降级白名单（§6.4 R-degrade）
  protection: "standard";           // v0.1：protected 在 bind 阶段拒绝（R6）
}

export interface AssertStepIR extends StepBase {
  kind: "assert";
  observe: "fresh" | { fromStep: StepId; which: "after" | "before" };
  assertions: AssertionIR[];        // 至少一条
}

export interface AssertionIR {
  assertId: string;
  predicate:
    | { type: "elementState"; selector: ElementSelectorIR;
        state: "present" | "visible" | "enabled" | "absent" }  // 对齐 WaitForElementCondition
    | { type: "elementText";  selector: ElementSelectorIR; match: TextMatchIR }
    | { type: "expr";         expr: Expr }                     // 对输出的纯表达式断言
    | { type: "visual";       prompt: string; region?: RectIR };
  verifyVia: Channel[];             // 显式 verify-chain；vision 只准出现在链尾（编译期校验）
  visionPrompt?: string;            // vision 降级提示词（YAML 表面键 visual，03 §1.4 规则 5）：elementState/elementText
                                    // 的 verifyVia 含 vision ⟺ 必填，否则禁止（schema 条件子句强制；visual 谓词的提示词
                                    // 在谓词自身 prompt 里）。作者手写、编译器绝不生成（原则 6）；属 judgeHash 域（02 §12.3）
  onMissingInput: "unknown";        // 固定语义：输入缺料 => unknown（原则 4）
}
// ElementSelectorIR / TextMatchIR 直接同构 DeviceRail ElementSelector / TextMatch：
// { context?: { contextKind: "native"|"web", contextId? }, role?, name?, identifier?,
//   text?: { value, mode: "exact"|"contains", caseSensitive? }, value?, css? }

export interface CallStepIR extends StepBase {
  kind: "call";
  flowRef: { flowId: string; irHash: Hash };  // 按内容哈希锁定 callee 版本
  inputs: Record<string, Expr>;               // call-by-value
}

export interface HumanStepIR extends StepBase {   // 人机协作是正式节点（原则 8）
  kind: "human";
  mode: "confirm" | "judge" | "provideInput" | "repairWorld";
  prompt: string;
  presents: Expr[];                 // 呈给人的 evidence/值
  decisions?: string[];             // judge/confirm 模式的枚举选项
  outputSchema?: unknown;           // provideInput 模式的输入契约
  timeoutMs: number;
  onTimeout: "unknown";             // 固定：超时 => verdict unknown，绝不默认 pass/fail
}

export interface IfStepIR extends StepBase {
  kind: "if";
  cond: Expr;
  then: StepIR[];
  else?: StepIR[];
}

export interface ForeachStepIR extends StepBase {
  kind: "foreach";
  items: Expr;
  as: string;                       // iter 作用域变量名
  body: StepIR[];
}

export interface LetStepIR extends StepBase {
  kind: "let";
  bindings: Record<string, Expr>;   // vars.* 作用域，SSA 单赋值
}

// ─── Handler / Retry ────────────────────────────────────────────
export interface HandlerBinding {
  hook: "onFail" | "onUnknown" | "onError" | "onResumeDrift";
  errorClasses?: ErrorClass[];      // 仅 onError 时过滤
  action: HandlerAction;
  maxTriggers: number;
}

export type HandlerAction =
  | { kind: "retry";    policy: RetryPolicy }          // step 从 acting 重入（预算独立计数）
  | { kind: "continue" }                               // 记录后放行（verdict 不变）
  | { kind: "escalate"; human: HumanStepIR }           // 升级人机节点
  | { kind: "abort" }
  | { kind: "repair";   flowRef: { flowId: string; irHash: Hash } };
    // 修复 subflow：无数据输出，跑完后重探（onResumeDrift）或重入（onFail）

export interface RetryPolicy {
  maxAttempts: number;
  backoffMs: number | { initial: number; factor: number; max: number };
  retryOn: ErrorClass[];            // 见 §5 错误分类；典型 ["action_failed_retryable","target_stale"]
}

// ─── 表达式（非图灵完备）─────────────────────────────────────────
export type Expr =
  | { lit: unknown }
  | { ref: string }                 // "params.x" | "steps.<id>.output.y" | "steps.<id>.verdict"
                                    // | "env.deviceId" | "iter.<as>" | "vars.<name>"
  | { fn: PureFn; args: Expr[] };
export type PureFn =
  | "eq" | "ne" | "not" | "and" | "or"
  | "concat" | "len" | "coalesce" | "jsonPath" | "regexMatch";
```

**双哈希规则（宪法条款）**：`effectHash` 覆盖「这一步对世界做什么」（binding/attempts、实参表达式规范形、effect、call 的 `flowRef.irHash + inputs`）；`judgeHash` 覆盖「这一步如何被判定」（preflight、observe 声明、assertions、verifyVia；逐 kind 权威清单见 02 §12.3。`onTimeout` 在 v0.1 是 const `"unknown"`，不入哈希域——若 v0.2 放开取值须届时裁决归域）。修复只动断言 → 只有 judgeHash 变 → 历史 Observation/Evidence 仍有效，可**离线重判**而不重跑设备（§6.7）。

---

## 4. Provider 契约（`pointlock-provider-kit` SPI）

> **SPI 进程边界（R12）**：SPI 的权威形态是 Rust trait（in-process）；预留 stdio JSON-RPC sidecar 适配形态（v0.2，供非 Rust provider——如 Playwright/Node——使用），v0.1 不实现 sidecar。三个扩展 provider 的包名归属（Rust crate 还是 sidecar 包）列为 v0.2 收编议题，本轮不定名（05 篇）。本节 TS 签名为规范记法（§3 注）。

### 4.1 双层声明：静态 manifest + capability lockfile + 运行期 attestation

```ts
/** provider 包内置、随包版本化的能力声明（编译器消费，不需设备在线） */
export interface ProviderManifest {
  name: string;                     // "devicerail"
  version: string;
  protocol: { major: number; minMinor: number; maxMinor: number };  // 支持 1.5
  features: {
    guaranteed: string[];           // 无条件提供
    conditional: { feature: string; requiresPlatform?: PlatformKind[] }[];
  };
  verbBindings: VerbBinding[];      // 声明式（非代码）动词→原生 action 映射
  channels: ChannelSupport[];
  knownActions: ActionDefinitionStatic[]; // 协议级五件套可内置
}

export interface ChannelSupport {
  channel: Channel;
  role: "act" | "verify" | "both";  // vision 声明只能是 "verify"
  requiresFeature?: string;         // uiTree → "observation.uiSnapshot.v1"
  requiresPlatform?: PlatformKind[];
}

export interface VerbBinding {
  verb: CanonicalVerb;
  actionName: string;               // "tap" → "tapElement"
  requiresFeature?: string;         // 五件套 → "device.semanticActions.v1"
  argMap: Record<string, string>;   // 声明式字段映射（R7：编译器零代码执行）
  // 通道归属判别（M1 收编）：同一动词可有多条绑定，归属通道由 argMap 定义域判别——
  // 含 "element" = 语义绑定（dom/uiTree attempt）；含 "x"+"y" = 坐标绑定（coordinate attempt）；
  // 歧义（两者兼有或皆无）= 编译错误（RF4012）。v0.2 如增 channel 显式字段须兼容此规则。
}

export interface ActionDefinitionStatic {
  name: string;
  inputSchema: unknown;             // JSON Schema Draft 2020-12（与 ActionDefinition.inputSchema 同源）
  outputSchema?: unknown;           // Pointlock 侧补充（协议侧 output 为无约束 JSON）
  protection: "standard" | "protected";
}

/** `pointlock lock` 对真实 daemon 执行 system.hello + device.capabilities 后固化（进版本库，如依赖 lockfile） */
export interface CapabilityLockfile {
  provider: { name: string; version: string };
  attestedAt: string;               // ISO 时间
  hello: {
    protocolSelected: { major: number; minor: number };  // 期望 { major:1, minor:5 }
    featuresEnabled: string[];      // FeatureSelection.enabled 原样
    server: { name: string; version: string };           // PeerInfo
  };
  device: {
    platform: PlatformKind;         // "android"|"ios"|"web"|"harmonyOs"|"macOs"|"windows"|"linux"|"rdp"
    actions: ActionDefinitionStatic[];  // device.capabilities 的 ActionDefinition[] 原样固化
  };
  digest: Hash;                     // 上述内容规范形的 sha256，嵌入 FlowIR.lockfileDigest
                                    // （M1 收编）digest = domain_hash("pointlock-lockfile/1", 规范形)，
                                    //  复用 §3 哈希构造（统一构造与 domain tag 规则见 02 §12.2）；
                                    //  域剔除 digest 自身与易变字段 attestedAt——重复 lock 未变化的
                                    //  daemon 必须得到逐字节相同的 digest（时间戳不作废能力事实）
}
```

**编译规则**：可用 feature 集 = `manifest.features.guaranteed ∪ lockfile.hello.featuresEnabled`；可用 action 集 = `lockfile.device.actions`（无 lockfile 时退回 `manifest.knownActions`，且只允许绑定 guaranteed feature 覆盖的部分）。step 所需能力不在集合内 → **编译错误，拒产 IR**（原则 5）。

**运行期 attestation**：`openSession` 内重放 `system.hello`（IR 的 `requiredFeatures` 全量进 `FeatureOffer.required` —— 协议语义保证 required 不满足则握手失败，免费强制力）+ `device.capabilities`，与 `lockfileDigest` 比对；不一致 → 错误类 `capability_drift`，拒跑，绝不静默降级。

### 4.2 运行时接口（完整 TS 签名）

```ts
export interface Provider {
  readonly manifest: ProviderManifest;
  openSession(opts: OpenSessionOptions): Promise<ProviderSession>;
}

export interface OpenSessionOptions {
  endpoint: ProviderEndpoint;       // devicerail: { spawn: SpawnSpec } | { attach: AttachSpec }
  deviceId: string;                 // devices.list / device.select 是 connection-local：
                                    // 一个 ProviderSession 独占一个 client 实例，绝不共享
  requiredFeatures: string[];       // → FeatureOffer.required
  lockfileDigest: Hash;             // attestation 依据
}

export interface ProviderSession {
  /** attestation 结果（openSession 内已比对通过），暴露给 Evidence/报告 */
  readonly attestation: CapabilityAttestation;

  /** 执行动作。callId 由 runner 生成并先写 WAL（devicerail: device.execute 的 params.id）。
      四分终态不折叠、不翻译 */
  execute(call: BoundActionCall, signal?: AbortSignal): Promise<ActionOutcome>;

  /** 显式观测（devicerail: device.observe） */
  observe(req: { wants: ("screenshot" | "uiSnapshot")[] }, signal?: AbortSignal): Promise<Observation>;

  /** 解引用某次 Observation 的规范化 UI 树（devicerail: ui.snapshot.get { observationId }；
      feature: observation.uiSnapshot.v1；协议限制：仅本 Session 活跃期可读 → 必须及时本地化） */
  uiSnapshot(observationId: string): Promise<
    | { ok: true;  snapshot: UiSnapshot }
    | { ok: false; reason: "driverUnsupported" | "policy" | "protectedAction" }>;

  /** resume 效果核对：在**签发该 callId 的 session**（issuing.sessionId——2026-07-18 收编评审，
      Wave C 实现修订：凭据由 harvest 逐 intent 精确计算——FromBinding（bind 时 run 行 cursor，
      非折叠视图重置值）/ Known（前最近携 cursor 的 resume）/ Unknown（不发 RPC，直接不确定分支））
      的事件日志中查 callId 的下落——跨 session resume 时经旧 session 日志读取
      （devicerail: events.list { afterSequence } 补扫旧 session / session.export），绝不查新 session；
      签发 session ≠ 当前 session 且旧日志不可达/已删除 → 一律 logUnavailable，
      仅当读到**签发** session 完整事件段且无踪迹才返回 neverDispatched。
      匹配 payload.type === "actionStarted" / "actionCompleted" 中 call.id === callId */
  reconcile(callId: string, issuing: EventCursor): Promise<ReconcileResult>;

  /** 拉取 Evidence 字节（devicerail: 按 AssetRef.uri，校验 sha256），供本地内容寻址库归档 */
  fetchEvidence(ref: AssetRef): AsyncIterable<Uint8Array>;

  /** 回写 Pointlock 计算的 Verdict（devicerail: verdict.record；feature: verdict.record.v1；
      daemon 只校验持久化。schema 上限：summary ≤ 16384 字符，evidence ≤ 64 条） */
  recordVerdict(v: { status: "pass" | "fail" | "unknown"; summary: string; evidence: AssetRef[] }): Promise<void>;

  /** checkpoint 事件游标水位（events.list afterSequence / events.stream.v1 epoch+cursor 语义） */
  currentCursor(): Promise<{ sessionId: string; lastSequence: number }>;

  /** 会话健康（映射 session_degraded / device_unavailable 等） */
  health(): Promise<{ ok: boolean; degraded?: string }>;

  /** 结束会话（devicerail: session.end；SessionOutcome 四值） */
  end(outcome: "completed" | "failed" | "cancelled" | "shutdown", reason?: string): Promise<void>;
}

export interface CapabilityAttestation {
  providerId: string;
  protocolSelected: { major: number; minor: number };
  featuresEnabled: ReadonlySet<string>;
  actions: ReadonlyMap<string, ActionDefinitionStatic>;
  lockfileDigest: Hash;
  attestedAt: string;
}

export interface BoundActionCall {
  callId: string;                   // uuid = device.execute params.id；effectively-once 的钥匙
  actionName: string;
  arguments: unknown;               // 运行期表达式求值后已按 inputSchema 二次校验
  actionTimeoutMs?: number;
  requestTimeoutMs?: number;        // 请求信封预算（区别于 action 预算）
}

/** 与 action-outcome schema 一一对应 */
export type ActionOutcome =
  | { outcome: "succeeded"; result: ActionResult }
  | { outcome: "failed";    error: ErrorInfo }
  | { outcome: "cancelled"; error: ErrorInfo }
  | { outcome: "timedOut";  error: ErrorInfo };

export interface ActionResult {
  callId: string;
  startedAtMs: number;
  finishedAtMs: number;
  output: unknown;                  // 如 findElement → { element: UiNodeRef }
  before?: Observation;
  after?: Observation;
  evidence: AssetRef[];
  execution?:                       // daemon 报告的实际执行模式（降级审计的关键输入）
    | { mode: "nativeSemantic";     context: UiContextRef }
    | { mode: "webSemantic";        context: UiContextRef }
    | { mode: "coordinateFallback"; context: UiContextRef;
        fallbackReason: "semanticInteractionUnavailable" | "platformLimitation" };
}

export type ReconcileResult =
  | { fate: "completed"; outcome: ActionOutcome } // 终态已落盘，采认（M1 收编：落盘终态是四分终态，不只 succeeded；
                                                  //  旧形状 { result: ActionResult } 迫使非成功终态降级为 logUnavailable）
  | { fate: "neverDispatched" }                  // 日志无踪迹，可安全重放
  | { fate: "startedNoTerminal" }                // 理论不应出现（终态落盘有 shield），按不确定处理
  | { fate: "logUnavailable"; reason: string };  // session 已结束等 → 不确定分支（§6.7-B）

export interface ErrorInfo { code: string; message: string; retryable: boolean; details?: unknown }

/** （M1 收编，自 04 §1）provider 方法「拿不到终态」时抛出的统一错误载体：wire 事实 + 归一结论并置，两边都进 RunLog */
export interface ProviderError {
  errorClass: ErrorClass;                    // §5 封闭枚举
  message: string;
  wire?: ErrorInfo;                          // daemon 原始 { code, message, retryable, details? }
  clientCode?: string;                       // 客户端错误码（Rust 侧以 devicerail-client 落地为准）
  retryableSource: "daemon" | "classifier";  // retryable 判断出处（04 §6.3 审计要求；枚举见 A.4）
}
```

（M1 收编）分水岭：`execute()` 的四分终态（`failed | cancelled | timedOut` 亦然）是**正常返回值**——世界发生了一件确定的事，不通过 ProviderError 表达；ProviderError 只表达「调用本身没能拿到终态」（transport 断裂、握手失败、协议违例、信封超时）。这是 runner 状态机 `settling` 与 `onError` 两条路径的分水岭。

`pointlock-provider-devicerail` 落地要点（全部有真实接口支撑）：`openSession()` = `DeviceRailClient.spawn/attach` → `system.hello`（协商）→ `devices.list` / `device.select` / `device.connect` → `session.start` → attestation 比对；`execute()` = `device.execute`；`reconcile()` = `events.list` 过滤 callId；事件订阅可用 `events.stream.open`（`events.stream.v1`，epoch+cursor 可续传）；截图证据经 `media.stream.start/capture/end`（`media.stream.v1`）或 Observation 自带 screenshot；日志导出 `session.export`（分页 feature `session.export.page.v1` 可用时用 `{limit, afterSequence}`）。客户端错误类（`TransportClosedError`、`FeatureNotNegotiatedError`、`RpcRemoteError` 等）映射到 §5 错误分类。

**devicerail-client 依赖（R12）**：`pointlock-provider-devicerail` 依赖新的 `devicerail-client` Rust crate——在 DeviceRail 仓库实现、经 path/git 依赖消费（DeviceRail workspace 全部 `publish=false`），是 **M1 的前置依赖**（M1 估算 +1~2 周）。`@devicerail/client`（TS）与 python-client 定位为**协议稳定性佐证与参考实现**。上段 TS 错误类名（`TransportClosedError` / `FeatureNotNegotiatedError` / `RpcRemoteError`）保留为 DeviceRail 生态事实；Rust 客户端将定义等价错误类型，名称以 client crate 落地为准（04 篇错误映射表加注）。响应校验（R12）：协议 DTO 走 serde 严格反序列化；动态 schema（如 `ActionDefinition.inputSchema` 的实参校验）走 jsonschema crate（Draft 2020-12）。

---

## 5. Closed Vocabulary：错误分类 taxonomy

`ErrorClass`（Pointlock 层封闭枚举；DeviceRail `ErrorInfo.code` 是开放字符串集，经下表映射）：

| ErrorClass | 来源与依据 | 默认处置 |
|---|---|---|
| `capability_drift` | attestation 与 lockfileDigest 不符；`FeatureNotNegotiatedError` | 拒绝启动/恢复，要求重 lock 或重编 |
| `bind_arguments_invalid` | 运行期实参未过 inputSchema（客户端侧，先于 daemon 的 `invalid_arguments`）；或 daemon 返回 `invalid_arguments`（retryable=false） | 不重试，step fail——这是编译器/表达式的 bug 信号 |
| `action_failed_retryable` | outcome `failed` 且 `ErrorInfo.retryable === true`（信任 daemon 申报，如 `device_unavailable`） | 命中 `retryOn` 则按 backoff 重试（新 callId、新 WAL 意图） |
| `action_failed_final` | outcome `failed` 且 `retryable === false` | 不重试；attempt 链有后继则尝试下一 attempt，否则 step fail |
| `action_timed_out` | outcome `timedOut`（码 `action_timeout`） | `idempotent: true` 才自动重试；否则先 reconcile/observe 确认，确认不了 → unknown 路径 |
| `action_cancelled` | outcome `cancelled`（码 `action_cancelled`；`request.control.v1` 的 `request.cancel`） | 不重试；用户取消 → run aborted |
| `target_stale` | `UiNodeRef` 的 `documentEpoch` 失效；定位未命中 | 重试前**强制重新 observe/find**（拿新 epoch） |
| `transport_lost` | `TransportClosedError`、daemon 退出 | run 级 suspend → 重开 ProviderSession（新 DeviceRail session，记 lineage）→ 从 checkpoint resume |
| `session_degraded` | 码 `session_degraded` | 当前 step → unknown，触发 flow 级 onError handler |

**非错误**：`evidence_unavailable`（各类 omission：`uiSnapshotOmission: driverUnsupported|policy|protectedAction`、`screenshotOmission: policy|protectedAction`）不是 ErrorClass——它是 verify-chain 的**降级触发器**，最终体现为断言 unknown。AssertionFailure 也不是错误，是 verdict（只走 onFail）。

（fixtures 实测过的 DeviceRail 错误码：`action_timeout`、`action_cancelled`、`device_unavailable`、`invalid_arguments`、`session_degraded`、`stream_slow_consumer`。此外协议含 `request.cancel` 取消语义。后续文档引用错误码仅限已核实项。）

---

## 6. Runner 执行语义

### 6.1 Run 与 RunLog

```
Run        = (irHash, paramsSnapshot, bindingSpec, runId)
RunLog     = append-only 事件序列（SQLite WAL，单调 seq）——唯一真相
Checkpoint = RunLog 的确定性折叠（物化视图，随时可重建）
```

RunLog 事件类型（封闭枚举，camelCase）：
`runStarted, stepEntered, preflightProbed, actionIntent, actionSettled, observationRecorded, assertionEvaluated, verdictRecorded, stepExited, callFramePushed, callFramePopped, handlerTriggered, humanRequested, humanResponded, runSuspended, runResumed, runFinished`。
其中 `runResumed` 携带 `alignmentReport`（§6.7-A），`actionIntent` 是 WAL（先 fsync 再 dispatch）。R13 注记：`runStarted` 与 `runResumed` payload 均携带 `supervisePolicy`（§6.9 运行级监督策略；属 run 而非 IR，不进任何哈希域；账本按执行段逐段记录本段生效策略）；`humanRequested` / `humanResponded` payload 携带判别子 `purpose: "step" | "supervision"`（human step 问答为 `step`，监督问答为 `supervision`，见附录 A.4）。

M1 收编注记（StepRecord 事件载体）：`stepEntered` payload = `{ stepId, effectHash, judgeHash, resolvedInputs }`，落账时机 = 输入求值快照完成（`ready`）之后、`preflight`/`actionIntent` 之前；`stepExited` payload = `{ state, output? }`（`output` 在输出投影完成时携带）。理由：checkpoint fold 借此摆脱哈希占位与 `resolvedInputs` 缺位（M0 已知限制）；跨 IR resume 的子域比对仍需旧 IR（`--old-ir`，M1 保留）。

2026-07-18 收编评审注记（payload 字段追加批——均为 additive 字段、可选、旧账本缺席；R12 单向兼容：旧读者对含新字段的账本 fail-closed 拒读）：`stepExited` = `{ state, output?, providerStateSummary? }`（07 §2.2 形状收编，两处 Option 化偏离见彼处；写入判据内涵式：退出时生效 verdict 为 fail/unknown，aborted 后置退出不写）；`runSuspended` 钉形为 `{ reason: string | null, providerStateSummary? }`（reason 现行写法为显式 null，如实钉档）；`runResumed` = `{ alignmentReport, supervisePolicy, eventCursor? }`（新代 session 重置水位，07 §4.5 收编）；`actionIntent` = `{ callId, argsSnapshot, chainIndex?, channel?, actionName? }`（binding.attempts 1 基链位 + 派发身份判别子，08 §3.4 叠加与卷宗 AttemptRecord 的联接源）；`verdictRecorded` = `{ verdict, localized?, localizationGaps? }`（本次判定的结算/verdict/human 类证据本地化清单与类型化缺口，观测类除外——判定后追加，绝不改 `actionSettled` 终局记录，WAL 锚仍是 `actionIntent`）。

### 6.2 Step 生命周期状态机（状态封闭枚举）

```
                   ┌──────(retryOn 命中且未超 maxAttempts：新 callId，新 actionIntent)──────┐
                   ▼                                                                       │
pending → ready → probing → acting → settling → observing → asserting → judged{pass|fail|unknown}
            │        │(preflight fail)  │(ProviderError)                    │(fail/unknown)
            │        └→ drifted ──onResumeDrift──→ ready | awaitingHuman    ├→ onFail/onUnknown handler
            │                                                               ▼
            ├→ skipped(if 分支未选中)                            awaitingHuman → judged
            └→ blocked(上游 fail 且策略为 halt)                  任意状态 → suspended（进程退出/暂停）
                                                                任意状态 → aborted（handler/用户裁决）
```

关键转移语义：
- `probing` 只在声明了 `preflight` 或处于 resume 首步时发生；`ready → acting` 之间无隐式观察。
- **`acting` 的入口是 WAL**：先追加 `actionIntent{ runPath, callId, argsSnapshot }` 并 fsync，然后才 `provider.execute`。崩溃点无论在 dispatch 前后，重启凭 callId 走 `reconcile()`。
- `settling`：等待四分终态。DeviceRail 保证 durable terminal event finalization 有 shield（取消不会留半开 Action），故 `cancelled`/`timedOut` 也是**有记录的终态**。
- attempt 链推进：前一 attempt 以 `action_failed_final`（可降级失败）终止才尝试下一 attempt；每次 attempt 无论成败都记 Evidence。
- `asserting` 是**纯计算**：输入 = ActionResult.output + before/after Observation + 本步 observe 产物；无 I/O（Evidence 已在 `observing` 阶段本地化）。这保证断言可离线重放。
- R13 注记（监督等待复用 `awaitingHuman`，不新增枚举值）：`ready → awaitingHuman` 新增一条入口 = 监督门控 `humanRequested(purpose="supervision")` 已 fsync（§6.9）；`awaitingHuman` 的退出按 `purpose` 判别——`purpose="step"` → `judged`（既有）；`purpose="supervision"` 时 `proceed` → `acting`、`abort` → `aborted`、`suspend` → `suspended` 且请求保持 pending（落账细则见 §6.9）。

### 6.3 Verdict 判定规则（钉死）

**单条 assertion 沿 verify-chain 求值**：
1. 某通道完成求值且谓词成立 → `pass`（记录通道；非首选通道 → 标 `degradedVerify`）。
2. 某通道完成求值且谓词不成立 → `fail`，**不再尝试后续通道**（降级链解决「看不到」，不解决「不喜欢答案」——R5）。
3. 通道无法完成求值（omission / 缺料 / session_degraded）→ 试下一通道；链耗尽 → `unknown`。

**Step verdict 折叠**（unknown 传染，弱于 fail）：
```
any fail                          → fail
else any unknown                  → unknown
else all pass 且无降级             → pass (degraded=false)
else all pass 但有 degradedVerify
     或 daemon 未授权降级(R-degrade) → pass (degraded=true)；verdictPolicy=strict 时折叠为 unknown
无 assertion 的 mutating action step → 不产生 verdict，只有执行状态（报告标 `unverified`）（R4）
human 节点                         → verdict := 人的判定（记 actor/at）；超时 → unknown
call step                          → callee 的 flow verdict
```
**Flow verdict** = 对「有 verdict 的 step」做同规则折叠。折叠结果经 `provider.recordVerdict`（`verdict.record`）回写——判定权在 Pointlock，daemon 只存证。

### 6.4 daemon 内部降级的处置（R-degrade，宪法条款）

DeviceRail 在 `device.execute` 内部可能自行降级（`execution.mode === "coordinateFallback"` + `fallbackReason`）。Pointlock 无协议手段阻止，但纳入管辖：每个 BoundAttempt 声明 `acceptExecutionModes`（作者 act-chain 没写 `coordinate` → 白名单为 `["nativeSemantic","webSemantic"]`）。运行期 `succeeded` 但 mode 不在白名单 → 判 `degraded_by_provider`：动作已发生（不可撤销），**不重试下一 attempt**，事实记入 Evidence，强制本步验证沿 verify-chain 全量确认；验证无法确认 → verdict `unknown` 而非 pass。未授权的降级不能变成静默成功（原则 6 + 4）。

### 6.5 重试策略挂载点（钉死，只有两处）

1. **attempt 内重试**（`StepBase.retry: RetryPolicy`）：同一 attempt 重发（**新 callId、新 actionIntent**，全部留痕），只对 `action_failed_retryable`、`target_stale`（重试前强制重 observe）及 `idempotent` 步的 `action_timed_out` 生效。
2. **handler 重试**（`onFail/onUnknown` 的 `{kind:"retry"}`）：整 step 从 `acting` 重入，预算独立计数。

没有第三处。断言不重试——「等 UI 到位」用 `wait_for`（原生 `waitForElement`，条件 `present|visible|enabled|absent`）表达，不靠重跑断言。

### 6.6 Checkpoint 模型

**粒度 = step 边界 + act 前 WAL 意图点**（唯一不可重做的是 mutating act；observe 只读、assert 纯，整段重做必然一致）。

```ts
export interface CheckpointView {
  runId: string;
  irHash: Hash;
  lockfileDigest: Hash;
  paramsSnapshot: unknown;
  binding: { deviceId: string;
             sessionLineage: string[];              // 历代 DeviceRail sessionId
             eventCursor: { sessionId: string; lastSequence: number } };
  completed: StepRecord[];
  frames: CallFrame[];                              // 活跃 subflow 调用栈
  frontier: { runPath: RunPath; state: StepState;
              pendingIntent?: { callId: string; argsSnapshot: unknown } };
  humanPending?: { runPath: RunPath; requestId: string;
                   purpose: "step" | "supervision";   // R13；supervision 场景 prompt 为自动生成的门控描述
                   prompt: string };
}

export interface StepRecord {
  runPath: RunPath;
  stepId: StepId;
  effectHash: Hash;  judgeHash: Hash;               // 对齐修复用
  attempts: AttemptRecord[];        // 每次含 callId、outcome、errorClass?、execution.mode(+fallbackReason)
  resolvedInputs: unknown;          // ready 时求值并快照；resume 不重算（杜绝重判后下游漂移）
  output?: unknown;
  observations: ObservationRecord[];  // 含 omission 原因
  evidence: EvidenceRef[];          // 本地内容寻址（AssetRef + sha256 + localPath）
  assertionOutcomes: { assertId: string; result: "pass"|"fail"|"unknown";
                       channel?: Channel; reason: string }[];
  verdict?: { status: "pass"|"fail"|"unknown"; degraded: boolean; supersedes?: string };
}
```

两个刻意选择：**resolvedInputs 快照化**（表达式在 ready 时求值一次入账）；**Evidence 本地化**（`observing` 阶段即按 sha256 拉入本地库——`ui.snapshot.get` 仅限本 Session 活跃期可读、session 删除是整段式，不能指望远端长存）。

### 6.7 Resume 语义与修复后对齐（宪法条款）

**Resume 正确性条件**：resume 合法 ⟺ (A) frontier 之前每个 completed step 的记录在新 IR 下仍被认可，且 (B) frontier 上的悬挂意图已被核对，且 (C) 世界通过 resume 探针。

**A. IR 对齐**（新 IR 正序遍历，按 stepId 匹配旧 StepRecord，产出 `alignmentReport` 入 RunLog）：

| 情形 | 分类（封闭枚举 AlignmentClass） | 处置 |
|---|---|---|
| id 同，effectHash 同，judgeHash 同 | `reusable` | 直接采认 |
| id 同，effectHash 同，judgeHash 变 | `judgeDirty` | **离线重判**：对存档 Observation/Evidence 重跑新断言（纯函数，无设备 I/O），产出新 Verdict（`supersedes` 旧的）。新断言需要旧记录没有的观测通道 → verdict unknown，标为可选「补观测点」 |
| id 同，effectHash 变 | `effectDirty` | 记录不可采认；该步及其**数据依赖下游**全部失效；resume 点回退至最早失效步 |
| 新 IR 有、旧记录无 | `new` | resume 点不得晚于它 |
| 旧记录有、新 IR 无 | `orphaned` | 归档不采认（output 被下游引用时编译期已报悬空引用错，到不了这里） |

resume 点 = 第一个非 `reusable` / 非重判成功的 `judgeDirty` 的位置。

**B. 悬挂意图核对**（副作用安全）：frontier 有 `pendingIntent{callId}` 时：
```
provider.reconcile(callId, issuing) →
  completed         → 采认落盘终态（outcome: ActionOutcome，M1 收编）：
                      succeeded 从 observing/asserting 正常续跑；
                      failed/cancelled/timedOut 补写 actionSettled 后走与
                      实时执行相同的 settled 处置（错误分类、重试判定）
  neverDispatched   → 安全重放（新 attempt）
  startedNoTerminal → 按 logUnavailable 的不确定分支处理
  logUnavailable    → idempotent=true 或 effect="readonly" → 重放；
                      否则触发 onResumeDrift handler，默认 escalate：
                      人以 HumanStepIR{mode:"repairWorld"} 确认世界后裁决 adopt / redo / abort
```

**C. 世界漂移探针**：resume 首个待执行 step 强制执行其 `preflight`（无声明则跳过并在报告标 `unprobed`——`StepBase.preflight` 是唯一载体，FlowIR 无 flow 级探针字段，无「flow 级默认探针」一说）。探针失败 → `drifted` → `onResumeDrift` handler（典型实现：`{kind:"repair"}` 恢复锚点 subflow，如 ensureLoggedIn，跑完重探）；超 `maxTriggers` → `awaitingHuman`。**Pointlock 从不假设设备在暂停期间没被动过。**

**Session 断代**：resume 一律开新 DeviceRail session（`session.start`），旧 sessionId 入 `sessionLineage`；旧 session 证据已本地化，无需跨 session 读取；事件游标跨 session 不假装连续。attestation 重放，`capability_drift` 则拒。

**`UiNodeRef` 时效**：`UiNodeRef { observationId, context: { contextKind, contextId, documentEpoch }, stableNodeId }` 绑定 documentEpoch，导航/重连/resume 后一律视为过期；编译器对跨 epoch 引用自动注入 revalidate（`findElement` 重定位）step（R-固化自方案 3 D11）。

### 6.8 人机协作节点的 durable 语义

`human` step 进入 `awaitingHuman` 即写 `humanRequested{requestId, presents}` 并可 suspend（进程可退出）。回应经 `pointlock-human-cli` 写 `humanResponded{requestId, payload, actor, at}`，runner 拉起后续跑。人的判定即 verdict（mode=judge）或 resolvedInputs（mode=provideInput），与机器产物同账本、同折叠规则。超时固定产出 unknown。

### 6.9 监督模式（supervised run，R13；里程碑 M2）

`pointlock run` 与 `pointlock resume` 新增 `--supervise <mutating|all>` **运行级**策略：与 `verdictPolicy` 同层级，但属于 run 而非 IR——不影响 `irHash`、不进任何哈希域；记录进本段起始事件（`run` 段为 `runStarted`、resume 段为 `runResumed`）payload 的 `supervisePolicy` 字段供审计。**跨段语义（钉死）**：supervisePolicy 不跨段隐式继承——每个执行段以启动该段的命令旗标为准，resume 传入 `--supervise` 即覆盖此前策略，未传即本段无监督；`runStarted` / `runResumed` 一律显式记录本段生效值（无监督记 `null`），账本逐段自明、无需回溯前段推断。

- **门控点**：step 进入 `acting` 之前（此时 `resolvedInputs` 快照已可呈现）。
- **WAL 顺序**：`humanRequested(purpose="supervision", presents 含 runPath / actionName / resolvedInputs 摘要)` 先 fsync → 通知 → `humanResponded(decision)` → `decision = proceed` 才写 `actionIntent` → dispatch。
- **decision 封闭枚举**：`proceed | abort | suspend`。v0.1 **刻意不提供 `skip`**——跳过 mutating 步破坏数据依赖，改动一律走修复路径（§6.7）。
- **与 human step 的关系**：监督问答**不产生 verdict**、默认无超时（可随时 `suspend`）；复用同一 store 单写者仲裁、同一通知通道（`pointlock-human-cli`）、同一收件箱，不新建第二套管道，靠 `purpose` 判别子区分（§6.1）。
- **崩溃语义**：崩溃发生在问答中间时，重启后 supervision 请求仍 pending，惰性结算语义与 human step 同款（§6.8）。
- **StepState 归属（R13 细化）**：监督等待期复用 `awaitingHuman`，不新增枚举值（入口/退出注记见 §6.2；附录 A.4）；checkpoint 的 `frontier.state` 记 `awaitingHuman`，`humanPending.purpose` 记 `"supervision"`（§6.6）。
- **decision 落账细则（R13 细化）**：`proceed` 的 WAL 后续如上，不另立规则；`abort` → 不触发任何 handler（人的直接裁决优先于策略路由），step 记 `aborted`，run 走既有 aborted 终局（`runFinished`），`humanResponded` 事件即审计痕；`suspend` → 写 `runSuspended`，supervision 请求保持 pending，下一段 resume 后该 step 仍处 `awaitingHuman` 照常等待回应（与 §6.8 惰性结算语义一致）。

R13 其余收编（骨架级钉死，细化归下游文档）：
1. **默认升级姿态**：标准 authoring 模板自带 flow 级 `handlers: on_unknown → escalate`（`max_triggers: 1`），使「拿不准就问人」成为默认而非 opt-in；编译器新增 lint 级警告诊断（RF3xxx 段、warning 非 error）：flow 对 unknown 无任何处置时提示（03 篇细化）。
2. **编译期问询（elicitation，M2 NL 链路）**：NL 起草器在四类情形必须发结构化提问——必填 param 缺失、目标选择器歧义、fallback 链授权（coordinate/vision 进链需作者点头）、secret 处理策略；问题为 JSON 结构（question / options / 目标 YAML path），答案织回后重起草，循环至编译通过。LLM 永远只产 YAML 草稿，编译器是唯一执法者（既有原则重申，不变）。
3. **LLM 修复提议循环**：失败 → `pointlock locate --format json` 卷宗 → 起草器提议 YAML patch（diff 形态）→ 人审批门（呈现 diff + align-preview 的 `alignmentReport` 预览：哪些历史保留、哪些重跑、哪些需确认）→ 批准 → resume。CLI 形态属 M2；UI 审批表单属 M3a，且遵守 06 §4.2 既有裁决——v0.1 webUi 不收响应：UI 只呈现 diff 与预览，批准动作经 human-cli / CLI 等价通道完成。

---

## 7. 数据流与表达式（精确规则）

- **作用域（封闭清单）**：`params.*`（run 输入，只读）、`env.*`（binding 注入的只读环境，如 `env.deviceId`）、`steps.<id>.output.*`、`steps.<id>.verdict`、`vars.*`（let 产物，SSA 单赋值）、`iter.<as>`（foreach 项）。**预留不实现（v0.2）**：`secrets.*`（不透明句柄，只可整体进 protected action 实参位，禁止参与运算或出现在 Evidence）。
- **可见性**：step 只能引用**同一 flow 体内、拓扑在前**的 step。Subflow 是硬边界：callee 只见显式 `inputs`，caller 只见 callee 声明的 `outputs`。Handler 无输出。无动态作用域、无全局可变量。
- **表达式语法**：YAML 中 `${{ ... }}`；内部是 §3 的 `Expr`——引用 + 白名单纯函数（`eq ne not and or concat len coalesce jsonPath regexMatch`）。非图灵完备：无循环、无自定义函数、无 I/O。
- **求值时机**：step 进入 `ready` 时求值一次并快照入 `resolvedInputs`；resume 不重算。求值后实参按目标 action 的 `inputSchema` 二次校验，失败 → `bind_arguments_invalid`。
- **类型检查**：编译期完成引用消解与类型检查（五件套 output 有协议 schema：`findElementResult { element: UiNodeRef }`、`waitForElementResult { matched, condition, element? }` 等；`invoke` 的 output 无声明即 `unknown` 类型，取字段必须先用 `expect_schema` 显式收窄）。

---

## 8. 编译链路（五阶段，YAML 在阶段 1 后不复存在）

| # | 阶段 | 职责 | 拒绝条件（示例） |
|---|---|---|---|
| 1 | `parse` | saphyr（yaml-rust2 系，带 span marks，YAML 行号诊断能力不降；serde_yaml 已停维护不可用。YAML 值仍限 JSON 数据模型）→ AST + 源 span；byte/depth/node 上限，fail-closed | 语法错误、超限 |
| 2 | `normalize` | 语法糖消解、默认值填充、macro 卫生展开（origin trace）、subflow 引用按 `irHash` 锁定 | 宏递归、subflow 版本冲突 |
| 3 | `check` | 引用消解、表达式类型检查、数据依赖图（供 §6.7 下游失效计算，成环拒绝）、handler 环检测、悬空引用 | 引用未定义输出、unknown 类型未收窄 |
| 4 | `bind` | **宪法执行处（原则 5/6/7 在此闭合）**：verb→actionName 声明式映射 + `inputSchema` 静态校验；act-chain / verify-chain 逐项能力校验（vision 出现在 act-chain = 编译错；coordinate attempt 无静态坐标 = 编译错；断言需 uiSnapshot 而 lockfile 无 `observation.uiSnapshot.v1` 且无显式 fallback = 编译错）；protected action 拒绝（v0.1）；feature 需求归集 → `requiredFeatures`（M1 收编：归集 `device.semanticActions.v1` 时必须捆绑 `observation.uiSnapshot.v1`——daemon 实测缺后者则握手失败 `semantic_snapshot_dependency_unsatisfied`，04 §9.2）；`acceptExecutionModes` 推导；跨 epoch 引用注入 revalidate step | 能力缺失、schema 不符、未声明的降级 |
| 5 | `seal` | 产出 `FlowIR`：规范化、`irHash` + 每步 `effectHash`/`judgeHash`、sourceMap、嵌入 `lockfileDigest`、人读 binding report（每步绑定到什么、哪些降级被授权） | — |

编译不需设备在线（消费 lockfile）；`pointlock lock` 单独对真实 daemon 固化事实，lockfile 进版本库。运行期入口 attestation 复核（§4.1）。

---

## 9. failedStepPath：RunPath 的表示

结构化路径（帧数组）为准，规范字符串仅供人读：

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

规范串示例：`checkout@a1f3/purchase/call→login@9c2e/enterPassword#2!tokenVisible`（`#n`=attempt，`!`=assertion；assertId 须符合其文法 `[A-Za-z_][A-Za-z0-9_-]*`）。三条硬规则：(1) 路径中 flow 一律带 irHash；(2) 宏展开不出现在路径里，sourceMap 负责译回 YAML 行号与宏调用链；(3) `pointlock locate <path>` 同时返回 IR 节点、YAML span、该步全部 attempts/observations/evidence——交付「可判案卷」，不只是坐标。

---

## 10. 投影协议（Projection Protocol，R14）

**定位**：UI 与 runner/store 之间的**唯一契约**是一组**渲染器无关**的只读投影 DTO + 传输约定。任何渲染器（React Flow web UI、未来的其他 flow 库、TUI、native）都只消费本协议，不触碰 store 内部、不读 RunLog 原始事件。08 篇既有四条 UI 铁律（只读投影不判定、不碰 daemon、写操作 v0.1 仅修复闭环一类且等价于既有 CLI 命令、loopback 单用户）全部不变——本协议是铁律 1 的类型化落地，铁律措辞以 08 §1 为准。

### 10.1 DTO 五族（封闭清单）

| DTO | 投影源 | 钉死语义 |
|---|---|---|
| `FlowGraphView` | FlowIR | 图模型：节点 kind 与 step kind（A.4）对齐，另有 call 折叠节点与 foreach 聚合节点；边分**顺序 / 分支 / hook** 三类语义边；分组 = subflow 按 `flowRef.irHash` 懒加载。**不含坐标与布局**（布局是渲染器职责），**不含任何 React Flow 概念**；节点携带 `runPath` 锚点，供运行状态叠加与深链 |
| `RunTimelineEntry` | RunLog | 既有名保留（08 篇）；五过滤器、50/页上限、evidence 纯引用等既有裁决全部不变 |
| `StepDossierView` | StepRecord + IR + sourceMap | `pointlock locate` 的 JSON 输出形状**即**此 DTO（IR 节点 + YAML span + attempts/observations/evidence/assertionOutcomes/verdict）；CLI 与一切渲染器共享同一查询层、同一形状——08 既有「step 检查器 = locate 的图形化」裁决借此类型化 |
| `HumanInboxEntry` | humanRequested / humanResponded | human step 与 supervision 请求的统一收件箱条目，含 `purpose` 判别子（R13，§6.9 / 附录 A.4） |
| `RunOverview` | RunLog | run 级摘要 + `revision`（= 该 run RunLog 最大 seq，既有裁决不变）+ per-step 状态摘要 `steps: Record<RunPath 规范串, { state: StepState; verdictStatus?: "pass" \| "fail" \| "unknown"; degraded?: boolean }>`（2026-07-17 裁决，additive 扩展故不 bump `projectionVersion`——供图状态叠加，只含叠加所需最小集，卷宗细节仍走 `StepDossierView`） |

> **已裁决（2026-07-17，原 R14 遗留 openQuestion 关闭）**：08 §3.1 原有第四类边 `data`（数据依赖，编译期 `check` 阶段依赖图的可视化，默认关闭）**v0.1 从 UI 削除**；登记为 **v0.2 additive 扩展候选**——届时以 `FlowGraphView` additive 新增边类交付，additive 新增边类不 bump `projectionVersion`（§10.3）。在此期间既有禁令保留：不得以 adapter 私读 store/IR 内部的方式自行推导数据依赖（违反本协议定位）。

### 10.2 真相源与生成

投影 DTO 的 Rust 定义放在 `pointlock-store` 的 **projection 模块**（不新增 crate——store 是读侧权威，locate 查询层同归属，§1.2）；schemars 生成 JSON Schema + type-only TS 包 `@pointlock/projection-types`（管线与 `@pointlock/ir-types` 同款）；golden fixtures 覆盖五族 DTO。

### 10.3 版本化

DTO 携带 `projectionVersion: 1`；演进 **additive-only**，breaking 变更需 bump。与 `irVersion` **相互独立**——投影是读侧契约，不影响 IR / checkpoint。

### 10.4 传输中立（三种消费方式，返回同一 DTO）

1. **HTTP+JSON**（规范形）：`pointlock inspect --serve` 提供。
2. **SSE**：只推 `{revision}` 失效通知，可选推送，轮询完全等价（既有裁决不变）。
3. **同进程直调**：同进程渲染器（如未来 TUI）直接调用 store 查询层。

### 10.5 React Flow 撤出协议层

08 篇原定义的 React Flow node/edge type 字面量（`stepAction/stepAssert/stepCall/stepHuman/stepIf/stepForeach/stepLet`；`seq/branch/hook/data`——其中 `data` 边已裁决 v0.1 从 UI 削除，§10.1）从协议命名**降级为 `@pointlock/ui` 内部 adapter 的实现细节**（FlowGraphView → React Flow 类型的映射层），不进任何 Canonical Vocabulary。`@pointlock/ui` 仍是 M3a 交付的第一个渲染器，交付承诺不变。非 web 渲染器是 v0.2+ 可选项，不进 v0.1 承诺；但协议自 v1 起即按渲染器无关设计（08 的 v0.1 非目标表相应加注）。

### 10.6 里程碑影响

- **M3a 验收新增**：`@pointlock/ui` 与 store 之间只经投影协议——以协议 golden fixtures + 适配层单测验证，UI 不 import store 内部类型。
- **M0**：代码生成管线的验收面把 `@pointlock/projection-types` 列为 **M3a 前置产物**（M0 不要求交付）。

---

## 附录 A：Canonical Vocabulary（后续 8 份文档的强制命名对照表）

> 规则：TS 类型/接口 = PascalCase；TS 字段/方法 = camelCase；枚举字面量 = camelCase（错误分类除外，用 snake_case 以与 DeviceRail 错误码风格对齐）；YAML 关键字 = snake_case；DeviceRail 词汇一律原样透传，不改拼写。

### A.1 Rust crate（10 个）与 TS package（5 个）（R12/R14；2026-07-28 扩编裁决）
Rust crate（`crates/`，kebab 命名）：`pointlock-ir` · `pointlock-expr` · `pointlock-compiler` · `pointlock-provider-kit` · `pointlock-store` · `pointlock-runner` · `pointlock-provider-devicerail` · `pointlock-vision` · `pointlock-human-cli` · `pointlock-cli`

TS package（`packages/`，npm scoped 命名保留）：`@pointlock/ir-types`（由 `pointlock-ir` Rust DTO 生成的 type-only 包，角色镜像 `@devicerail/protocol`）· `@pointlock/projection-types`（由 `pointlock-store` projection 模块生成的 type-only 包，投影协议 §10，R14）· `@pointlock/ui`（web UI，R12 正式收编）· `@pointlock/nl-drafter`（NL 起草器，信任边界外）· `@pointlock/walk-drafter`（**2026-07-28 扩编裁决**：落地式起草器——驱动真实页面、以真快照锚定选择器、执行验证后组装 `*.flow.yaml` 草稿；信任边界外，编译器仍是唯一执法者；与 §7 非目标 10 的录制器不同——LLM 落地式而非 record-replay，产物是普通 YAML 草稿进同一条五阶段管线；v0.1 不发布）

### A.2 CLI 命令（7 个）
`pointlock lock` · `pointlock compile` · `pointlock run` · `pointlock resume` · `pointlock inspect` · `pointlock locate` · `pointlock report`

R13 注记：`pointlock run` 与 `pointlock resume` 新增 `--supervise <mutating|all>` 运行级旗标（§6.9；不影响 `irHash`、不进任何哈希域，逐段记入 `runStarted` / `runResumed` payload 的 `supervisePolicy`；不跨段隐式继承，resume 未传即本段无监督）。

（M1 收编）退出码：`0` = pass / unverified、`1` = fail / 编译拒绝、`2` = unknown、`3` = suspended / blocked / 待确认、`64` = usage 与 not-in-subset、`70` = 基础设施错误。

### A.3 核心类型名（TS）
| 域 | 类型 |
|---|---|
| IR | `FlowIR` `StepIR` `StepBase` `ActionStepIR` `AssertStepIR` `CallStepIR` `HumanStepIR` `IfStepIR` `ForeachStepIR` `LetStepIR` `ActionBinding` `BoundAttempt` `AssertionIR` `HandlerBinding` `HandlerAction` `RetryPolicy` `ParamDecl` `OutputDecl` `Expr` `PureFn` `ElementSelectorIR` `TextMatchIR` `RectIR` `SourceMapEntry` |
| 哈希/路径 | `Hash` `StepId` `RunPath` `PathFrame` |
| Provider | `Provider` `ProviderSession` `ProviderManifest` `CapabilityLockfile` `CapabilityAttestation` `VerbBinding` `ChannelSupport` `ActionDefinitionStatic` `OpenSessionOptions` `BoundActionCall` `ActionOutcome` `ActionResult` `ReconcileResult` `ErrorInfo` `ProviderError`（M1 收编） |
| Runner/Store | `RunLog` `CheckpointView` `StepRecord` `EvidenceRef` `CallFrame` `AlignmentReport`（记录明细类型见下行） |
| 运行时/持久化（M1 收编，定义域归 `pointlock-ir`，§1.2；2026-07-18 收编评审追加后四名） | `RunLogEvent` `RunLogPayload` `AttemptRecord` `ObservationRecord` `AssertionOutcomeRecord` `IterState` `PendingIntent` `Frontier` `HumanPending` `BindingState` `ActionExecution` `UiSnapshotRef` `StepVerdict` `ActionOutcomeKind` `ProviderStateSummary` `AttestationSnapshot` `SessionHealthSnapshot` `EvidenceGap` |
| 投影（R14，§10） | `FlowGraphView` `RunTimelineEntry` `StepDossierView` `HumanInboxEntry` `RunOverview` |
| 判定 | `Verdict` `VerdictStatus` `ErrorClass` `Channel` `ExecutionMode` `EffectClass` `StepState` `AlignmentClass` `CanonicalVerb` `Disposition` |

### A.4 封闭枚举值
| 枚举 | 值（穷尽） |
|---|---|
| step kind | `action` `assert` `call` `human` `if` `foreach` `let` |
| `VerdictStatus` | `pass` `fail` `unknown`（附加布尔 `degraded`；`unverified` 是报告注记，不是 verdict） |
| `verdictPolicy` | `standard` `strict` |
| `EffectClass` | `mutating` `readonly` `pure` |
| `Channel` | `dom` `uiTree` `vision` `coordinate`（vision 仅 verify；coordinate 仅 act） |
| `ExecutionMode` | `nativeSemantic` `webSemantic` `coordinateFallback` |
| `ErrorClass` | `capability_drift` `bind_arguments_invalid` `action_failed_retryable` `action_failed_final` `action_timed_out` `action_cancelled` `target_stale` `transport_lost` `session_degraded` |
| `StepState` | `pending` `ready` `probing` `acting` `settling` `observing` `asserting` `judged` `skipped` `blocked` `drifted` `awaitingHuman` `suspended` `aborted`（R13：`awaitingHuman` 兼用于监督等待，不新增枚举值，§6.2/§6.9） |
| handler hook | `onFail` `onUnknown` `onError` `onResumeDrift` |
| `Disposition`(HandlerAction.kind) | `retry` `continue` `escalate` `abort` `repair` |
| human mode | `confirm` `judge` `provideInput` `repairWorld` |
| human onTimeout | `unknown`（固定） |
| act-chain chip mark（2026-07-18 收编评审；`RunOverview.steps` 叠加用） | `untried` `crossed` `succeeded`（渲染词汇；DTO 条目 Wave D 定形为 `{ chainIndex, mark: succeeded\|crossed, executionMode?, fallbackReason? }`——`untried` 以缺席表达，`degraded` 不入 DTO：白名单判定需 IR 的 `acceptExecutionModes`，由持 IR 渲染器依 `executionMode` 判定） |
| human purpose（`humanRequested`/`humanResponded` payload 判别子，R13） | `step` `supervision` |
| supervision decision（§6.9） | `proceed` `abort` `suspend`（v0.1 刻意无 `skip`） |
| `supervisePolicy`（`--supervise` 旗标值，逐段进 `runStarted` / `runResumed` payload，不进哈希域；本段无监督记 `null`） | `mutating` `all` |
| `projectionVersion`（投影 DTO 版本字段，R14，§10.3；与 `irVersion` 独立，演进 additive-only，breaking 需 bump） | `1` |
| `ReconcileResult.fate` | `completed` `neverDispatched` `startedNoTerminal` `logUnavailable` |
| `ProviderError.retryableSource`（M1 收编，§4.2） | `daemon` `classifier` |
| `AlignmentClass` | `reusable` `judgeDirty` `effectDirty` `new` `orphaned` |
| `requiresConfirmation.cause`（M1 收编，07 §5.4） | `mutatingReexec` `positionalReplay` `orderInvalidated` `frontierUnknown` |
| phase | `preflight` `act` `observe` `assert` |
| RunLog 事件 | `runStarted` `stepEntered` `preflightProbed` `actionIntent` `actionSettled` `observationRecorded` `assertionEvaluated` `verdictRecorded` `stepExited` `callFramePushed` `callFramePopped` `handlerTriggered` `humanRequested` `humanResponded` `runSuspended` `runResumed` `runFinished` |
| `CanonicalVerb` | `tap` `set_value` `clear` `wait_for` `find` `observe` `screenshot` `invoke` |
| assertion predicate type | `elementState` `elementText` `expr` `visual` |
| elementState 值 | `present` `visible` `enabled` `absent`（= DeviceRail `WaitForElementCondition`） |
| `PureFn` | `eq` `ne` `not` `and` `or` `concat` `len` `coalesce` `jsonPath` `regexMatch` |
| 编译阶段 | `parse` `normalize` `check` `bind` `seal` |

### A.5 Provider 接口方法名（穷尽）
`Provider.openSession` · `ProviderSession.execute` · `.observe` · `.uiSnapshot` · `.reconcile` · `.fetchEvidence` · `.recordVerdict` · `.currentCursor` · `.health` · `.end`；属性 `Provider.manifest`、`ProviderSession.attestation`。

### A.6 通用 action 命名空间约定
1. **YAML 层**只写 snake_case 通用动词（A.4 `CanonicalVerb`）或 `invoke: { action: "<原生名>" }` 逃逸门。
2. **IR 层**动词消失：`BoundAttempt.actionName` 恒为 provider 原生名（DeviceRail camelCase：`tapElement` 等）。runner 无 verb switch。
3. 动词表**不追求覆盖**：只收「已被至少两个 provider 语义一致实现」的动作；v0.1 即协议五件套 + observe/screenshot/invoke 的忠实投影。driver 专有 action（如 mock/desktop driver 的 `tap`、`waitForIdle`）一律 `invoke`。
4. 多 provider 限定形式 `"<provider>:<action>"`（如 `devicerail:tapElement`）**预留**；v0.1 单 provider，一律裸原生名。
5. Pointlock 自有扩展 feature 若未来出现，用 `pointlock.*` 前缀，与 DeviceRail feature（`device.*`/`observation.*`/`verdict.*`/`events.*`/`media.*`/`action.*`/`request.*`/`session.*`）不混淆。

### A.7 YAML 关键字（封闭清单）
| 层 | 关键字 |
|---|---|
| flow 顶层 | `flow` `provider` `params` `outputs` `steps` `handlers` `macros` `verdict_policy` |
| step 通用 | `id` `preflight` `expect` `retry` `timeout_ms` `effect` `idempotent` `checkpoint` `on_fail` `on_unknown` `on_error` `on_resume_drift` |
| 动词键 | `tap` `set_value` `clear` `wait_for` `find` `observe` `screenshot` `invoke`（invoke 子键：`action` `args`；M2 收编——`args` 为原生动作实参容器，与结构键 `call` 的 `inputs` 语义不同，不复用） |
| fallback | `locate_via`（act-chain：`[dom, uiTree, coordinate]` 子序列） `verify_via`（verify-chain：`[dom, uiTree, vision]` 子序列） `coordinate`（静态坐标，写了 coordinate 通道就必填） |
| 结构 | `call` `inputs` `human` `if` `then` `else` `foreach` `in` `as` `let` |
| 断言 | `element` `state` `text` `value` `expr` `visual` `region` `expect_schema` |
| retry 子键 | `max_attempts` `backoff_ms` `retry_on` |
| human 子键 | `mode` `prompt` `presents` `decisions` `on_timeout` |
| handler 子键（03 §1.8） | 处置 head key `retry` `continue` `escalate` `abort` `repair`（= `Disposition` 枚举值转 YAML 键；`retry` 值 = retry 子键，`escalate` 值 = human 子键，`repair` 值 = subflow 路径，同 `call` 锁定 irHash）· `error_classes`（仅 `on_error` 可过滤 ErrorClass）· `max_triggers` |
| 表达式定界 | `${{ ... }}` |
| 预留（v0.2，不得挪用） | `secrets` `protected` |

**关键字辨析（防漂移重点）**：`preflight` = 前置/resume 世界探针（原方案 2 的 `expects`，已改名）；`expect` = 后置断言。二者永不混用。

### A.8 DeviceRail 透传词汇（已逐项核实，引用时不得改拼写）
| 类别 | 词汇 |
|---|---|
| RPC 方法（24） | `system.hello` `system.describe` `devices.list` `device.select` `device.connect` `device.disconnect` `device.capabilities` `device.execute` `device.observe` `ui.snapshot.get` `verdict.record` `session.start` `session.current` `session.end` `session.export` `sessions.list` `events.list` `events.subscribe` `events.clear` `events.stream.open` `media.stream.start` `media.stream.capture` `media.stream.end` `request.cancel` |
| stream 通知（2） | `events.stream.event` `events.stream.terminal` |
| feature id | `device.routing.v1` `action.protected.v1` `events.snapshot.v1` `events.stream.v1` `media.stream.v1` `request.control.v1` `session.export.page.v1` `observation.uiSnapshot.v1` `device.semanticActions.v1` `verdict.record.v1` |
| 语义 action 五件套 | `findElement` `tapElement` `clearElement` `setElementValue` `waitForElement` |
| 关键类型 | `ActionDefinition{name,description,inputSchema,protection}` `ActionResult` `ActionExecution` `ActionOutcome(succeeded/failed/cancelled/timedOut)` `ErrorInfo{code,message,retryable,details}` `Observation` `AssetRef{id,mediaType,uri,sha256?}` `UiNodeRef{observationId,context,stableNodeId}` `UiContextRef{contextKind,contextId,documentEpoch}` `ElementSelector` `TextMatch{value,mode,caseSensitive}` `Verdict{status,summary≤16384,evidence≤64}` `FeatureOffer{required,optional}` `FeatureSelection{enabled}` `PeerInfo` `Viewport` |
| 枚举 | `ActionProtection: standard\|protected` · `CoordinateFallbackReason: semanticInteractionUnavailable\|platformLimitation` · `UiSnapshotOmissionReason: driverUnsupported\|policy\|protectedAction` · `ScreenshotOmissionReason: policy\|protectedAction` · `SessionOutcome: completed\|failed\|cancelled\|shutdown` · `WaitForElementCondition: present\|visible\|enabled\|absent` · `TextMatchMode: exact\|contains` · `UiContextKind: native\|web` · `VerdictStatus: pass\|fail\|unknown` |
| 已核实错误码 | `action_timeout` `action_cancelled` `device_unavailable` `invalid_arguments` `session_degraded` `stream_slow_consumer`（开放集合，引用仅限已核实项） |
| 事件 payload 类型 | `sessionStarted` `sessionEnded` `actionStarted` `actionCompleted` `observationCaptured` `verdictRecorded` `mediaStreamStarted` `mediaFrameCaptured` `mediaStreamEnded` `error`（事件信封含 `eventId` `sessionId` `sequence` `requestId` `deviceId` `atMs` `payload`；`actionStarted/actionCompleted` 的 `call.id` 即 reconcile 的 callId） |
| 客户端 | `@devicerail/client`：`DeviceRailClient` `TransportClosedError` `FeatureNotNegotiatedError` `RpcRemoteError` `DeviceRailEventStream` 等；`@devicerail/protocol`：类型 only；`devicerail-client`（Rust，**待实现**，在 DeviceRail 仓库实现、经 path/git 依赖消费，M1 前置）。R12 注：TS 错误类名保留为 DeviceRail 生态事实，Rust 客户端将定义等价错误类型，名称以 client crate 落地为准；`@devicerail/client`（TS）与 python-client 定位为协议稳定性佐证与参考实现 |
| 硬事实 | 设备选择 connection-local（一个 ProviderSession 独占一个 client）；session 事件日志 append-only、一基序列、只能整段删除；`ui.snapshot.get` 仅本 Session 活跃期可读（→ Evidence 必须本地化）；`device.execute` 终态落盘有 shield；`verdict.record` 只校验持久化、不运行断言 |

### A.9 IR 关键字段名（防漂移重点）
`irVersion` `flowId` `irHash` `lockfileDigest` `requiredFeatures` `stepId` `effectHash` `judgeHash` `preflight` `assertions` `verifyVia` `visionPrompt` `binding.attempts` `actionName` `acceptExecutionModes` `resolvedInputs` `runPath` `callId` `sessionLineage` `eventCursor.lastSequence` `alignmentReport` `supersedes` `degraded` `verdictPolicy` `sourceMap`

---

## 附录 B：需求追踪矩阵（权威；覆盖 13 项产出 + 10 条原则）

> 需求基线 = [`docs/requirements.md`](../requirements.md)（编号规范见其 §1：只认「产出 N / 原则 N」，历史「需求 N」编号作废）。本矩阵是产出/原则符合性审计的唯一入口，**取代**各文档页眉的「覆盖需求产出 N」自报声明（页眉声明降级为导读，冲突以本矩阵为准）。修改任何文档的覆盖范围，必须同步更新本矩阵并过评审。

### B.1 产出 → 文档章节（正向），文档 → 产出（反向读同一张表的「落点」列）

| 产出 | 内容（原文见 requirements.md §4） | 落点（骨架条款 + 细化文档章节） |
|---|---|---|
| 1 | 项目定位与命名 | 01 §1（定位）、01 §2（命名评估与推荐） |
| 2 | 核心概念权威定义 | **00 §2（十三概念权威定义）**、§2.1（辨析）、§2.2（关系图）；01 §3–§5（展开与参考系统映射） |
| 3 | Typed IR：schema 与语义 | **00 §3（核心类型）**、§7（数据流与表达式）；02 全篇（锚文档）；验收基线 schema：`schema/flow-ir.v0.1.schema.json`（R12：真相源为 `pointlock-ir` Rust DTO，生成物与基线 diff 评审后基线随生成物滚动） |
| 4 | YAML authoring 格式与示例 | 00 附录 A.7（关键字封闭清单）；03 §1（格式规范）、§2–§3（示例） |
| 5 | 编译链路 | **00 §8（五阶段）**；03 §4（NL → YAML draft → IR 全链、静态检查清单、错误报告） |
| 6 | Provider interface 完整定义 | **00 §4（SPI 契约）**、附录 A.5（方法名穷尽）；04 §1–§8（生命周期、执行/观测/错误/超时契约、一致性套件） |
| 7 | DeviceRail provider 完整映射设计 | 00 §4.2 末尾落地要点、附录 A.8（透传词汇）；04 §9–§10（进程管理、协商、动作/选择器/错误码映射、验收面） |
| 8 | 扩展 Provider 设计：Playwright / HTTP / CLI | 05 全篇（§1 共同基座、§2 Playwright、§3 双路径裁决、§4 HTTP、§5 CLI） |
| 9 | Human-in-the-loop 接口 | **00 §3 `HumanStepIR`、§6.8（durable 语义）**；06 §1–§6（四 mode 契约、通知通道、等待语义、响应即 Evidence） |
| 10 | 执行语义核心 (a)–(f) | **00 §6（runner 语义）、§9（RunPath）**；07：(a) §1 Subflow；(b) §2 失败定位与卷宗；(c) §3 Checkpoint；(d) §4 Resume 与副作用安全；(e) §5 局部修复与对齐；(f) §6 与 LangGraph / Prefect 对比 |
| 11 | 交互式验证 handler 家族 | 06 §7（`handle_interactive_verification` 六阶段模式；building block 见 00 §3 `HandlerBinding` / `HandlerAction`） |
| 12 | @pointlock/ui 信息架构 | **00 §10（投影协议，R14）**；08 §1–§5（铁律、页面清单、图模型与 React Flow 适配、timeline、实时通道） |
| 13 | MVP 里程碑与 v0.1 非目标 | 08 §6（M0/M1/M2…）、§7（非目标清单） |

（反向核对口径：01 = 产出 1+2；02 = 产出 3；03 = 产出 4+5；04 = 产出 6+7；05 = 产出 8；06 = 产出 9+11；07 = 产出 10；08 = 产出 12+13。无产出悬空、无文档失配。）

### B.2 原则 → 落点条款（结构性保证优先于口号）

| 原则 | 本文落点（结构性保证） | 下游主落点 |
|---|---|---|
| P1 YAML 是界面 | §8（阶段 1 后 YAML 不复存在）；§1.2 硬规则 | 03 §1.0 |
| P2 Runner 只执行 Typed IR | §1.2（runner 入口只接受 `FlowIR`，不接受字符串） | 02 §1 |
| P3 Action/Assertion/Verdict 分离 | §2 概念 8/10/12 + §2.1 辨析；R4（无断言 mutating step 不产生 verdict） | 01 §3、04 §1 总则 1 |
| P4 无法确认输出 unknown | §3 `onMissingInput: "unknown"`、`onTimeout: "unknown"`；§6.3 求值规则 3；§6.4 | 06 §5.3、07 §4 |
| P5 capability-bound 编译 | §4.1 编译规则（能力缺失 → 编译错误，拒产 IR）；§8 `bind` 阶段 | 04 §9.2、02 §5.2 |
| P6 fallback 显式声明 | §3 `ActionBinding`（没写 fallback 就只有一项）；§6.4 R-degrade；附录 A.7 `locate_via`/`verify_via` | 03 §1.4、04 §1 总则 2 |
| P7 视觉只能降级验证 | §3 verify-chain 尾部约束 + act-chain 类型禁 vision；§4.1 `ChannelSupport`（vision 只能 `"verify"`）；R3/R5 | 02 §5.3、03 §1.4 |
| P8 人机协作正式节点 | §3 `HumanStepIR`；§6.8 durable 语义（偏离登记 D3：不是 provider） | 06 §1–§2 |
| P9 Subflow 一等公民 | §2 概念 3；§3 `CallStepIR`（按 `irHash` 锁定）；§6.7 调用帧与对齐 | 07 §1 |
| P10 第一版轻量 | §6.1（SQLite WAL 单进程，无 Temporal/队列） | 07 §3.3、08 §5/§7 |

---

*本文档由三方案合成评审产出。修改本文任何封闭枚举或命名，必须同步更新全部下游文档并重新过评审。*
