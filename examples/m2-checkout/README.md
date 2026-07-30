# examples/m2-checkout — M2 验收 demo（08 §6.3）

带人工闸门的 checkout flow：foreach 三件商品逐项加购 → `human: confirm`
闸门挂起（进程退出，账本存续）→ 次日 inspect 可见待办 → cli 通道回应 →
resume 续跑至 pass → report 出「谁在何时判了什么」。

```bash
cargo build -p pointlock-cli
./demo.sh
```

脚本覆盖验收 1 与 5（foreach 部分）的可脚本化叙事。其余验收项的证明位置：

| 验收项 | 位置 |
|---|---|
| escalate 升级 + maxTriggers 耗尽 | `crates/pointlock-runner/tests/handlers.rs` |
| onResumeDrift: repair 教科书路径 | `crates/pointlock-runner/tests/control_flow.rs` |
| 宏 origin trace / call outputs 契约 | `crates/pointlock-compiler/tests/compile_m2.rs` |
| 监督模式全序（R13） | `crates/pointlock-cli/tests/e2e_m2.rs` |
| 编译期问询 / LLM 修复提议循环 | `packages/nl-drafter`（elicitation + proposeRepair）、`resume --preview` |
