<div align="center">

<img src="assets/logo.svg" alt="Pointlock logo" width="150">

<h1 align="center">Pointlock</h1>

<p align="center"><strong>把自然语言任务编译成强类型、capability-bound、崩溃安全的 flow IR —— 在真实设备、浏览器与 API 上可靠执行。</strong></p>

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](rust-toolchain.toml)
[![Node](https://img.shields.io/badge/node-%3E%3D22-3c873a.svg)](package.json)
[![Status](https://img.shields.io/badge/status-v0.1_M0--M3a-success.svg)](#路线图)
[![Tests](https://img.shields.io/badge/tests-437%20passing-success.svg)](#测试)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

[English](README.md) · **简体中文** · [文档](docs/README.md) · [路线图](#路线图)

</div>

---

## Pointlock 是什么？

**Pointlock 是一个轻量、开源的流程编排引擎——它把一项自然语言任务编译成强类型、capability-bound 的中间表示（Typed IR），并可靠执行**：崩溃可续跑、失败可局部修复、每个判定都是 evidence 支撑的 `pass` / `fail` / `unknown`，覆盖真实设备、浏览器与 API。

多数 agent / RPA 栈在**运行期**才判断某一步到底可不可行、无法确认时乐观地假定成功、进程中途死掉就丢了执行位置。Pointlock 把这些决策提前并做成持久的：能力在**编译期**校验、每个 mutating 动作在派发前**先写 WAL 意图**、判定只有在内容寻址的 evidence 支撑下才可能是 `pass`。

> Pointlock **不是** **DeviceRail** 的一部分——但 DeviceRail 是它的第一个 execution provider。引擎与 provider 解耦；DeviceRail（真实 Android/设备自动化）只是第一个接入的后端。

## 为什么用 Pointlock —— 三个承诺

| 承诺 | 含义 | 如何保证 |
|---|---|---|
| 🧭 **编译期能力绑定** | 能力缺失是**编译错误**，不是运行期惊喜。编译消费 provider 的能力 **lockfile**——不需要设备在线。 | `pointlock lock` 把 provider 的真实能力目录固化成版本化 lockfile；编译器据此绑定每个动作并**封印**结果（`irHash`、`lockfileDigest`）。运行期复核 attestation，**漂移即拒跑**。 |
| 🛟 **运行期崩溃安全** | 崩溃发生在*任何*时刻，重启都能确定**「那个动作到底发生了没有」**。 | 事件溯源 **RunLog** + act 前**写 WAL 意图** + 重启后 `callId` **reconcile**。不重复触发、不丢失终态。 |
| 🔬 **evidence 支撑的三值判定** | `pass` / `fail` / `unknown`——无法确认真相时绝不乐观 `pass`。 | Action / Assertion / Verdict 三者分离；每个 verdict 引用**内容寻址的 evidence**；视觉检查只能*降级*置信度，绝不凭空造出一个 pass。 |

## 快速上手

> **前置：** [Rust](https://rustup.rs)（2024 edition，stable）与 [Node ≥ 22](https://nodejs.org) + [pnpm](https://pnpm.io)（UI 用）。DeviceRail provider 的构建说明见[从源码构建](#从源码构建)。

```bash
# 1. 把某个 provider 的能力固化成 lockfile（离线；这里用内置 fake）
cargo run -p pointlock-cli -- lock --provider fake --out fake.lock.json

# 2. 把 YAML 任务编译成封印的、capability-bound 的 FlowIR
cargo run -p pointlock-cli -- compile examples/wifi-demo.flow.yaml \
  --lockfile fake.lock.json --out demo.ir.json

# 3. 运行——提前停止以模拟崩溃/挂起（退出码 3 = suspended）
cargo run -p pointlock-cli -- run demo.ir.json --store .runs --stop-after set_ssid

# 4. 从 checkpoint 续跑——对齐后只补跑未持久完成的部分
cargo run -p pointlock-cli -- resume demo.ir.json --store .runs --run <run-id>

# 5. 检视 ledger、checkpoint 与判案卷宗
cargo run -p pointlock-cli -- inspect --store .runs --run <run-id> --rebuild-checkpoint

# …或在浏览器里打开只读投影控制台
cargo run -p pointlock-cli -- inspect --store .runs --serve
```

## 工作原理

YAML 是**书写界面**，不是执行协议。Runner 只执行 Typed IR；编译器与 runner 互不依赖。

```
  YAML（书写界面）
    │  pointlock-compiler： parse → normalize → check → bind → seal
    ▼
  FlowIR （irHash · 双哈希域 · lockfileDigest）                 ← 持久契约
    │
    ▼
  pointlock-runner ── 状态机 · verdict 折叠 · resume 对齐 · 监督门控
    ├── pointlock-store             RunLog / Checkpoint / Evidence （SQLite + WAL）
    └── pointlock-provider-kit      Provider SPI（Rust trait）
          └── pointlock-provider-devicerail → devicerail-client → daemon
  共享内核： pointlock-ir + pointlock-expr
  读侧：     投影协议 → @pointlock/projection-types → @pointlock/ui（首个渲染器）
```

1. **书写** —— 把任务写成 `*.flow.yaml`（或用 `@pointlock/nl-drafter` 从自然语言起草）。
2. **固化** —— 离线固化 provider 能力（`pointlock lock`）。
3. **编译** —— 绑定能力并封印 IR 哈希；能力缺失即编译错误。
4. **运行** —— 每个 mutating step 派发前写 WAL；verdict 折叠进 checkpoint。
5. **续跑 / 修复** —— 崩溃或局部失败时，对齐算法核对 ledger，只补跑未证实的部分。
6. **检视** —— 渲染器无关的只读**投影协议**喂给 React 控制台、`report` 或你自己的 UI。

## 特性

- **带双哈希身份的 Typed IR** —— 内容寻址的 flow；确定性、版本化、可 diff。
- **能力 lockfile** —— 针对 provider 真实目录的可复现、离线编译。
- **事件溯源 RunLog + SQLite/WAL checkpoint** —— `verify_checkpoint` 不变式证明物化视图 ≡ 从头重折。
- **`callId` reconcile** —— 跨崩溃对在途动作做「恰好一次」推理。
- **三值、evidence 支撑的判定** —— `pass` / `fail` / `unknown`，内容寻址 evidence，离线重判。
- **局部修复** —— 只重跑单个失败 step 或子链，不重做已证实的工作。
- **人机协作是一等节点** —— 监督式运行（`run --supervise <mutating|all>`）通过与 human step 同一条持久管道门控每个 mutating step。
- **子流程一等公民** —— `RunPath` 在嵌套调用内精确定位失败。
- **渲染器无关的投影协议** —— 五族只读 DTO（`FlowGraphView` / `RunTimelineEntry` / `StepDossierView` / `HumanInboxEntry` / `RunOverview`），独立版本化；HTTP+JSON 规范、SSE 可选、同进程直调等价。
- **单一静态二进制** —— CLI 以一个自包含可执行文件交付。

## 与其他方案对比

| | **Pointlock** | LangGraph | Temporal | n8n / Zapier | Prefect / Airflow |
|---|:---:|:---:|:---:|:---:|:---:|
| 主战场 | 真实设备/浏览器/API 任务自动化 | LLM agent 图 | 持久化微服务工作流 | SaaS 应用集成 | 数据管线 |
| 能力校验 | **编译期（lockfile 绑定）** | 运行期 | 运行期 | 运行期 | 运行期 |
| 崩溃恢复 | **事件溯源 + WAL 意图 + reconcile** | 应用自定义 | 事件溯源（服务端） | 有限 | task 级重试 |
| 判定模型 | **三值 + 内容寻址 evidence** | 自由形式 | 应用自定义 | 成功/失败 | 成功/失败 |
| 局部修复 | **支持——单 step / 子链** | 重跑图 | replay | 重跑 | 重跑 task |
| 体量 | **单二进制 + SQLite** | 库 | 服务器集群 | 托管/服务器 | 服务器 + DB |
| 书写 | YAML → Typed IR（或 NL 起草） | Python | 代码（Go/Java/…） | 可视化编辑器 | Python |

Pointlock 刻意保持**轻量**（单进程、SQLite——不上 Temporal 集群），优化的是*分发面与类型刚性*，不是吞吐（runner 是 I/O 密集型）。

## 常见问题

**Pointlock 能上生产了吗？**
Pointlock 处于 **v0.1** —— M0–M3a 里程碑已完成（编译器、runner、store、DeviceRail provider、human/监督子系统、vision 校验器、投影协议、只读 UI，以及 `report`/`inspect` 命令面），437 测试全绿。公开 SPI 与多 provider 面（Playwright / HTTP / CLI provider）已设计、将在 v0.2 落地。把它当作经过充分测试的早期地基，而非 1.0。

**它和 LLM agent 框架有什么不同？**
Agent 框架边跑边判断可行性、常常假定成功。Pointlock 在任何执行之前先把计划针对 provider 冻结的能力**编译**、能力漂移即拒跑、没有 evidence 绝不记 `pass`。自然语言是书写便利（`@pointlock/nl-drafter`），不是执行基底。

**编译 flow 需要连着设备吗？**
不需要。编译针对 `pointlock lock` 离线产出的**能力 lockfile** 绑定。只有运行期才需要设备——那时会复核 attestation。

**进程在动作中途崩溃会怎样？**
每个 mutating 动作在派发前先写意图。重启后 `callId` reconcile 判定该动作是否真的发生，于是既不重复触发、也不静默丢失终态——从一致的 checkpoint 续跑。

**今天支持哪些 provider？**
**DeviceRail**（真实设备/Android 自动化）是 v0.1 中第一个也是唯一接线的 provider。Provider SPI 是稳定的 Rust trait；Playwright、HTTP、CLI provider 在[设计文档](docs/design/05-providers-playwright-http-cli.md)中规范，属 v0.2。

**为什么 Rust + TypeScript？**
类型真相源是 `pointlock-ir` 的 Rust DTO（serde + schemars）；JSON Schema、`@pointlock/projection-types` TypeScript 类型与 golden fixtures 都由它生成。选 Rust 的首要理由是分发面与类型刚性；TypeScript UI 是投影协议之上的首个渲染器。

## 目录结构

```
pointlock/
├── crates/                     # Rust 核心（Cargo workspace，10 crate）
│   ├── pointlock-ir/            #   Typed IR —— 类型真相源（serde + schemars）
│   ├── pointlock-expr/          #   编译器与 runner 共享的表达式语言
│   ├── pointlock-compiler/      #   YAML → normalize → check → bind → seal
│   ├── pointlock-provider-kit/  #   Provider SPI（Rust trait）
│   ├── pointlock-store/         #   RunLog / Checkpoint / Evidence + 投影（rusqlite）
│   ├── pointlock-runner/        #   状态机、verdict 折叠、resume 对齐
│   ├── pointlock-provider-devicerail/  # 首个 provider（→ devicerail-client → daemon）
│   ├── pointlock-vision/        #   Anthropic 支撑的视觉校验器（只降级）
│   ├── pointlock-human-cli/     #   human-step / 监督交互通道
│   └── pointlock-cli/           #   单一静态二进制
├── packages/                   # TypeScript workspace（pnpm）
│   ├── ir-types/               #   IR 类型（type-only 包）
│   ├── projection-types/       #   生成的投影 DTO（UI 的唯一契约）
│   ├── nl-drafter/             #   自然语言 → YAML 起草
│   ├── walk-drafter/           #   落地式起草：驱动真实页面 → *.flow.yaml 草稿（不发布）
│   └── ui/                     #   @pointlock/ui —— v0.1 只读投影控制台
├── schema/                     # 生成的 JSON Schema + golden fixtures
├── docs/                       # 设计文档（见 docs/README.md）
├── examples/                   # 可运行的 *.flow.yaml 示例
└── assets/                     # 品牌资产（logo、icon）
```

## 文档

从**[文档索引](docs/README.md)**开始。设计系列是权威规范：

| # | 文档 | 主题 |
|---|---|---|
| — | [需求基线](docs/requirements.md) | 13 项产出 · 10 条设计原则 · 偏离登记表 |
| 00 | [架构骨架](docs/design/00-architecture-spine.md) | **最高权威** —— 分层、13 核心概念、Typed IR、Provider SPI、错误分类、runner 状态机、双哈希 resume、Canonical Vocabulary |
| 01 | [定位与概念](docs/design/01-positioning-and-concepts.md) | 项目定位、命名、概念生命周期、横向对照 |
| 02 | [Typed IR](docs/design/02-typed-ir.md) | IR v0.1 设计；规范 schema 见 [`schema/flow-ir.v0.1.schema.json`](schema/) |
| 03 | [YAML 界面与编译链路](docs/design/03-authoring-and-compilation.md) | authoring 格式、完整示例、五阶段编译、诊断编码 |
| 04 | [Provider SPI 与 DeviceRail](docs/design/04-provider-interface-and-devicerail.md) | provider 契约 + DeviceRail 完整映射 |
| 05 | [Playwright / HTTP / CLI Provider](docs/design/05-providers-playwright-http-cli.md) | 三个扩展 provider（v0.2 起） |
| 06 | [Human 交互与安全验证 Handler](docs/design/06-provider-human-and-secure-handlers.md) | human step、通知通道、secret 铁律、验证 handler |
| 07 | [Subflow / Checkpoint / Resume / 修复](docs/design/07-subflow-checkpoint-resume-repair.md) | 执行语义、checkpoint 模型、resume 正确性、局部修复 |
| 08 | [UI 与 MVP](docs/design/08-ui-and-mvp.md) | 投影控制台、M0–M3 里程碑、v0.1 非目标 |

## 路线图

- ✅ **v0.1（M0 → M3a）** —— Typed IR、capability-bound 编译器、崩溃安全 runner + store、DeviceRail provider、human/监督子系统、vision 校验器、投影协议、只读 UI、`report` / `inspect` / `locate`。
- 🔜 **v0.2** —— 公开 Provider SPI（sidecar）、Playwright / HTTP / CLI provider、修复写侧 UI、checkpoint 物化节奏、`pointlock report` 扩展。
- 🧭 **更远** —— 多 provider 流、secrets 子系统、web-UI 书写。

偏离登记见[需求](docs/requirements.md)，里程碑定义见[设计 08](docs/design/08-ui-and-mvp.md)。

## 从源码构建

```bash
# Rust 核心
cargo build --workspace
cargo test  --workspace

# TypeScript UI + 生成类型
pnpm install
pnpm -r build
pnpm -r test
```

> **DeviceRail provider 说明。** `pointlock-provider-devicerail` 依赖 `devicerail-client`，目前以**兄弟路径 checkout**（`../device-rail/crates/client`）引用，待其发布到 crates.io。构建完整 workspace 需要把 DeviceRail 仓库检出在本仓旁边。此事记录在 [`Cargo.toml`](Cargo.toml) 与[设计文档](docs/design/04-provider-interface-and-devicerail.md)中。

## 测试

Rust workspace 现有 **437 个通过的测试**（单元、集成、conformance），含 `verify_checkpoint` 不变式（物化视图 ≡ 从头重折）与 provider conformance 套件。生成物（JSON Schema、`@pointlock/projection-types`、golden fixtures）在 CI 中再生并校验。再生与 fixture bless 流程见 [CONTRIBUTING](CONTRIBUTING.md)。

## 参与贡献

欢迎贡献。请先阅读：

- [**CONTRIBUTING.md**](CONTRIBUTING.md) —— 开发环境、工具链、代码生成与 fixture 流程、PR 流程
- [**CODE_OF_CONDUCT.md**](CODE_OF_CONDUCT.md) —— 社区行为准则（Contributor Covenant）
- [**SECURITY.md**](SECURITY.md) —— 如何报告漏洞
- [**CHANGELOG.md**](CHANGELOG.md) —— 版本历史

## 许可证

采用 [Apache License 2.0](LICENSE)。

## 致谢

Pointlock 的设计经由多智能体评审流程产出（竞争骨架 → 合成裁决 → 并行章节细化 → 对抗式交叉校验并修复所有 critical/major 发现），并在每个里程碑后以对抗式「评审—修复」循环逐步加固。

<div align="center">
<sub>用 Rust 与 TypeScript 构建 · <a href="docs/README.md">文档</a> · <a href="CONTRIBUTING.md">贡献</a> · <a href="README.md">English</a></sub>
</div>
