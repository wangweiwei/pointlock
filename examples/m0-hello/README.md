# examples/m0-hello — M0 验收 demo（08 §6.1）

3 步 hello flow：`waitInput → typeNote → checkNote`，fake 注册（echo 语义）下
断言是对步输出的真实判定。

```bash
cargo build -p pointlock-cli
./demo.sh
```

脚本覆盖验收 1/2/3 的可脚本化部分：编译确定性（同输入 irHash 逐字节相同）、
三步 judged + flow verdict pass、账本事件序与检查点重建自检。其余验收项的
证明位置：

| 验收项 | 位置 |
|---|---|
| 假屏改坏 → checkNote fail；删 expect → unverified | `crates/pointlock-cli/tests/e2e_m0.rs` |
| kill -9 两注入点 → CheckpointView 重建 + pendingIntent | `crates/pointlock-runner/tests/runner.rs` |
| FakeProvider 一致性套件 | `crates/pointlock-provider-kit/tests/conformance_fake.rs` |
| 代码生成管线行为等价 | `crates/pointlock-ir/tests/behavioral_equivalence.rs` |
