# examples/m1-login — M1 验收 demo（08 §6.2）

真 DeviceRail daemon（内置 mock driver）上的 login flow：能力闭环
（lock→bind→attestation）、真实跑通、单次调用取回可判案卷。

```bash
cargo build -p pointlock-cli
# 兄弟仓构建 daemon：cd ../device-rail && cargo build --bin devicerail-daemon
./demo.sh
```

脚本覆盖验收 1（含篡改 lockfile 的 capability_drift 拒跑）、2、3 的可脚本化
部分。其余验收项的证明位置：

| 验收项 | 位置 |
|---|---|
| 双哈希两幕戏（judgeDirty 零交互重判 / effectDirty 回退重跑） | `crates/pointlock-cli/tests/e2e_m1.rs`、`crates/pointlock-runner/tests/runner.rs` |
| 崩溃窗口 reconcile（三分支） | `crates/pointlock-runner/tests/verify_chain.rs`、provider 套件 |
| evidence sha256 一致性 | `crates/pointlock-provider-devicerail/tests/devicerail_provider.rs` |
