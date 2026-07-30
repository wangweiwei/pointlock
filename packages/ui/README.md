# @pointlock/ui

v0.1 的唯一渲染器（08 全篇）：本地单用户只读操作台。四条铁律照 08 §1——只读投影不判定、不碰 daemon、写操作只等价于 CLI 命令（W3b 落修复闭环）、loopback + token。**只依赖 `@pointlock/projection-types`**，不 import store 内部（M3a 验收 8；`test/adapter.test.ts` 以协议 golden fixtures 驱动适配层验证）。

## 结构

- `src/adapter/graph.ts` — FlowGraphView → React Flow 的内部适配层（08 §3.0 两层结构）：node/edge type 字面量、dagre 布局、`RunOverview.steps` 叠加 join（剥 `[i]` 迭代帧、最坏实例聚合）都是渲染器细节，不进协议。
- `src/pages/` — `/flows`、`/flows/:flowId`、`/runs/:runId(/steps/:runPath)` 三栏、`/inbox`（只读呈现 + human-cli 回应指引）。hash 路由（无需服务端 fallback）；`:runPath` 即 locate 参数，深链 = locate 超链接。
- `src/api.ts` — 唯一 HTTP 面：token capability 注入、SSE `{revision}` 订阅 + 2s 轮询降级（完全等价，08 §5）。

## 用法

```sh
pnpm --filter @pointlock/ui build           # → dist/
pointlock inspect --serve --store <store> --artifacts <ir 目录> --ui packages/ui/dist
# 打开启动打印的带 token URL

# 开发：
POINTLOCK_HOST=http://127.0.0.1:<port> pnpm --filter @pointlock/ui dev   # vite 代理 /api、/evidence
```

## 测试

```sh
pnpm --filter @pointlock/ui test    # vitest：适配层 × 协议 golden fixtures
pnpm --filter @pointlock/ui check   # tsc
```

## W3a 边界（W3b 交付）

修复闭环 UI（在 YAML 打开 → 重编译 + 对齐预演 → resume）、`/api/repair/*` 三端点、R13 LLM 修复提议呈现（diff + alignmentReport，无批准控件）。subflow 懒加载展开为 v0.1 内后续增量。
