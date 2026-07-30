# Pointlock《需求与目标》—— 仓库权威基线

> **地位**：本文件是仓库内《需求与目标》的**唯一权威基线**。全部设计文档（`docs/design/00`–`08`）对「产出 N」「原则 N」的引用，一律以本文件编号为准；审计以骨架（`docs/design/00-architecture-spine.md`）附录 B 的追踪矩阵为入口。
>
> **来源声明**：本文件 §2–§4 为需求方原始输入的**逐字原文**（2026-07-16 由主会话回填入库，取代当日早先的「标题重建版」；差异见 §7 变更记录）。§1、§5、§6 为仓库治理增量。

---

## 1. 编号规范（防止两套编号并存）

- 权威编号形如 **「产出 N」**（N = 1…13，见 §4）与 **「原则 N」**（N = 1…10，见 §3）。
- 历史评审意见中出现过「需求 N」写法且所指与「产出 N」不一致，该编号体系**无权威来源，作废**；存量引用一律换算为本文件编号。
- 设计文档页眉的「覆盖需求产出 N」自报声明仅是导读；与骨架附录 B 追踪矩阵冲突时，**以矩阵为准**。

## 2. 背景与边界（原文）

我们要设计一个独立的轻量级通用流程编排项目，暂定名 Pointlock / TaskRail。它不是 DeviceRail 的一部分，但 DeviceRail 会作为第一个 provider 接入。

DeviceRail 已经能提供设备抽象、动作执行、Observation、Evidence、Session/Event Log。但在复杂测试中，仅靠模型直接操作设备很脆弱：容易猜坐标、猜状态、无法稳定处理地址栏清空、权限弹窗、图形鉴权、登录、网络恢复等复杂流程。因此需要一个上层流程编排系统，把自然语言任务翻译成标准化、可验证、可恢复、可视化、可局部修复的流程。

**核心目标**：

1. 支持自然语言输入，但自然语言不能直接驱动执行。
2. 支持 YAML/JSON 作为人类可读的 authoring format，但 runner 不直接执行自由 YAML。
3. 执行前必须编译成强类型 Typed IR。
4. Typed IR 必须 schema-first、closed vocabulary、capability-bound。
5. 每个 step 都必须有稳定 id、输入、动作、输出、断言、evidence、verdict、错误信息。
6. 失败时必须能定位 failedStepId / failedSubflowId。
7. 支持只修正失败步骤或子流程，然后从 checkpoint 继续执行。
8. 支持可视化流程图和 step timeline。
9. 支持 DeviceRail、Playwright、HTTP/API、CLI、Human-in-the-loop 等 provider。
10. 支持复杂子流程、macro、handler，例如 open_browser、clear_address_bar、login、handle_permission_dialog、handle_interactive_verification。
11. 图形鉴权、验证码、短信、安全校验等不应默认自动绕过，应进入 human.pause / humanRequired 节点，并保存 evidence。

**项目边界**：

- DeviceRail：设备能力 provider，负责 device action、observation、evidence。
- Pointlock/TaskRail：流程编排、自然语言编译、Typed IR、runner、断言、checkpoint、resume、可视化、人机协作。
- Provider Adapter：把通用 action 映射到 DeviceRail / Playwright / API / CLI / Human。

**推荐架构**：pointlock-core（Flow schema、Typed IR、compiler、runner、assertion runner、checkpoint/resume、evidence model、provider interface）、pointlock-provider-devicerail、pointlock-provider-playwright、pointlock-provider-http、pointlock-provider-cli、pointlock-provider-human、pointlock-ui（React Flow 可视化流程图；step 状态、截图、日志、失败定位、局部修复入口）。

**参考开源项目**：React Flow（可视化流程编辑器）；LangGraph（durable execution、state、human-in-the-loop、checkpoint 思路）；Prefect（task state、retry、failure handling、UI timeline）；Node-RED / n8n（可视化节点和子流程，但不要直接照搬其自由节点模型）；Temporal（只作 durable execution 长期参考，第一版不要上重型架构）。

## 3. 十条设计原则（原文）

| # | 原则 |
|---|---|
| P1 | YAML 是界面，不是执行协议。 |
| P2 | Runner 只执行 Typed IR。 |
| P3 | Action 做事，Assertion 判断，Verdict 记录。 |
| P4 | 无法确认时输出 unknown，不能乐观 pass。 |
| P5 | Provider 必须声明 capabilities，compiler 根据 capabilities 做 binding。 |
| P6 | 任何 fallback 都必须显式声明，例如 dom -> ui_tree -> vision -> coordinate。 |
| P7 | 视觉判断只能作为降级验证，不能默认成为主证据。 |
| P8 | 人机协作是正式节点，不是异常 hack。 |
| P9 | 子流程是一等公民，支持输入、输出、局部失败、局部重试。 |
| P10 | 第一版保持轻量，不直接引入大型工作流平台如 Temporal。 |

## 4. 十三项产出（原文）

| # | 产出 |
|---|---|
| 1 | 项目定位和命名建议。 |
| 2 | 核心概念模型：Flow、Step、Subflow、Macro、Handler、Provider、Capability、Action、Observation、Assertion、Evidence、Verdict、Checkpoint。 |
| 3 | Typed IR v0.1 JSON Schema 草案。 |
| 4 | YAML authoring format 示例。 |
| 5 | Natural language -> YAML draft -> Typed IR -> Runner 的编译链路。 |
| 6 | Provider interface 设计。 |
| 7 | DeviceRail provider 如何映射 app.open、screen.tap、text.input、observe、evidence.capture。 |
| 8 | Playwright provider 如何映射 web.navigate、dom.query、element.click、element.fill、assert。 |
| 9 | Human provider 如何实现 human.pause、human.input、human.confirm。 |
| 10 | 子流程调用、输入输出、失败定位、局部修复、resume 的设计。 |
| 11 | 图形鉴权/验证码/短信/安全校验的 handler 设计。 |
| 12 | 可视化 UI 的信息架构。 |
| 13 | 第一阶段 MVP 范围和里程碑。 |

产出 → 文档章节的完整落点见骨架附录 B.1；原则 → 落点条款见骨架附录 B.2。

## 5. 需求措辞偏离登记表（集中登记，替代各文档零散自曝）

设计文档中每一处「需求侧措辞未被原样采纳」都必须登记于此；未登记的偏离视为违规。现存已知偏离：

| # | 需求原文措辞 | 采纳形态 | 裁决出处 |
|---|---|---|---|
| D1 | 产出 7 的 `app.open` / `screen.tap` / `text.input` 通用动作命名空间 | 未采纳该命名空间：YAML 层只写 `CanonicalVerb` 动词键（tap / set_value / clear / wait_for / find / observe / screenshot / invoke）或 `invoke` 逃逸门；IR 层恒为 provider 原生 `actionName` | 骨架 R7、附录 A.6；04 §9.4.1–§9.4.2（逐条转换表，需求侧动作全部有落点） |
| D2 | 产出 7 的 `evidence.capture` | 定名 `ProviderSession.fetchEvidence`（evidence 由 action/observe 自动产生并以引用透传，无独立"拍证据"动作） | 骨架附录 A.5；04 §4.3 |
| D3 | 产出 9 的「Human provider」与 `human.pause` / `human.input` / `human.confirm` | human 不实现 Provider SPI，是一等 step kind（`HumanStepIR`），mode 枚举 confirm / judge / provideInput / repairWorld 覆盖并细化原三方法；pause 语义 = runner 挂起 + checkpoint 落盘 | 骨架 §3 / §6.8（P8 落点）；06 §1 |
| D4 | 核心目标 6 的 `failedStepId` / `failedSubflowId` | 统一为 `RunPath`（含嵌套 call 帧与 foreach 迭代的路径表示），两个 id 是其投影 | 骨架 §5；07 §2 |
| D5 | 骨架方案期的 `expects` 关键字 | 改名 `preflight`（与后置断言 `expect` 防撞） | 骨架 R9、附录 A.7 |
| D6 | §2 推荐架构的包清单（pointlock-core / pointlock-provider-* / pointlock-ui） | R12 混合栈下细分为 10 个 Rust crate + 3 个 TS 包：pointlock-core 拆为 pointlock-ir/-expr/-compiler/-store/-runner/-provider-kit；pointlock-ui 定名 `@pointlock/ui`；provider 包 v0.1 仅 pointlock-provider-devicerail，Playwright/HTTP/CLI 归 v0.2 | 骨架 §0.1 R12、§1.2、附录 A.1 |

## 6. 补充上下文

- DeviceRail 真实接口的事实基线：主会话对 `/Users/dengfengwang/Codes/projects/device-rail` 的探索报告 + 评审代理对源仓库的直接核实（协议 v1.5、24 个 RPC 方法、feature 清单、错误码、事件 payload、evidence 内容寻址等），已固化进骨架附录 A.8。
- 设计文档系列由多智能体工作流产出：3 份竞争骨架 → 评审合成 → 8 章并行细化 → 6 路对抗校验（56 项发现）→ 9 个修订代理修复全部 critical/major（31 项）。

## 7. 变更记录

| 日期 | 变更 | 依据 |
|---|---|---|
| 2026-07-16 | 初版：原始需求缺位下的「标题重建版」入库 | 评审发现「需求源文件缺失，符合性不可审计」 |
| 2026-07-16 | 原文回填：§2–§4 替换为需求方逐字原文；重建版的编号推断（产出 3、8）经原文核实**正确**；原则措辞以原文为准（旧 P4/P5/P10 为意译）；偏离登记表增补 D4，D2/D3 按原文措辞修正 | 主会话持有原始输入 |
| 2026-07-16 | 偏离登记表增补 D6（推荐架构包清单 → R12 混合栈包结构） | R12 修订评审（需求方确认采纳） |
