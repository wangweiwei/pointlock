# @pointlock/nl-drafter

阶段 0 的 authoring 助手（03 §4.0/§4.1/§4.1.1）：自然语言意图 → `*.flow.yaml` 草稿。**信任边界外**——它只产 YAML 草稿与结构化问询，`pointlock compile` 是唯一执法者，草稿永远经人评审后才进编译。

## 结构

- **核心（零运行时依赖，可无 API key 单测）**：`draftFlow` 起草循环（起草 → 守卫 → 编译 → 诊断回喂）、`proposeRepair` 修复提议循环（R13，08 §6.3 验收 9：`pointlock locate --format json` 卷宗 + 当前 YAML → 最小修订全文 + 统一 diff（`unifiedDiff`），同一守卫与编译门；审批门在包外——人对照 diff 与 `pointlock resume --preview` 的对齐预演，批准动作即去掉 `--preview` 重跑 resume）、回复协议解析（`parseReply`）、fallback 授权守卫（`scanFallbackChains`/`checkFallbackAuthorization`）、CLI 编译门（`cliCompiler`，驱动真实 `pointlock compile --format json`）。
- **Claude 适配器**（`@pointlock/nl-drafter/claude`）：`claudeLlm()`，基于 `@anthropic-ai/sdk`（默认 `claude-opus-4-8`，adaptive thinking + 流式）。LLM 以注入函数 `DrafterLlm` 进入核心，测试用脚本桩替换。

## 四类必问（03 §4.1.1，封闭词汇）

`missingRequiredParam` / `ambiguousSelector` / `fallbackAuthorization` / `secretStrategy`。其中 fallback 授权（`coordinate` 进 `locate_via`、`vision` 进 `verify_via`）由确定性守卫在起草侧强制：无作者答案 → 产出问询而非草稿；作者拒绝 → 回喂 LLM 移除通道。

## 用法

```ts
import { draftFlow, cliCompiler } from "@pointlock/nl-drafter";
import { claudeLlm } from "@pointlock/nl-drafter/claude";

const result = await draftFlow({
  intent: "打开 Wi-Fi 面板，输入 SSID 并回读确认",
  llm: claudeLlm(),
  compile: cliCompiler({ bin: "target/debug/pointlock", lockfile: "device.lock.json" }),
  context: { lockfileHints, authoringSchema, uiSnapshot },
  answers: [],            // 上一轮问询的作者答案，织回重起草
});
// result.status: "compiled" | "needsInput" | "exhausted"
```

`needsInput` 时把 `result.questions` 呈给作者，答案作为 `answers` 再跑一轮；循环至 `compiled`（草稿仍需人评审 + binding report 放行，03 §4.5）。

## 测试

```sh
pnpm --filter @pointlock/nl-drafter test    # node:test，桩 LLM；pointlock 二进制存在时含真实编译门集成
pnpm --filter @pointlock/nl-drafter check   # tsc 类型检查
```

## 登记限制（v0.1）

- authoring schema 由调用方注入；产出口已落地——`pointlock compile --emit-authoring-schema --out authoring.json` 从编译器封闭关键字表生成 `pointlockAuthoringSchema: 1` 文档，直接作为 `context.authoringSchema` 传入。
- 守卫是行扫描启发式（提示词已约束链写成内联数组）；权威执法仍是编译器 + 人评审。
- `ambiguousSelector`/`missingRequiredParam`/`secretStrategy` 三类靠提示词强制（需理解意图/素材，无确定性判据）；`fallbackAuthorization` 双保险（提示词 + 守卫）。
