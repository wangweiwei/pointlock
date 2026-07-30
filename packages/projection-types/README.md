# @pointlock/projection-types

投影协议（骨架 §10，R14）五族 DTO 的 type-only 包。真相源是 `pointlock-store::projection` 的 Rust DTO——**不要手改 `src/`**。

## 管线（02 §1.1 同款）

```sh
cargo run -p pointlock-store --bin pointlock-projection-schema-gen   # Rust DTO → schema/generated/projection/
pnpm --filter @pointlock/projection-types generate                  # schema → src/*.d.ts + test/fixtures.generated.ts
pnpm --filter @pointlock/projection-types check                     # tsc：类型 + golden fixture 锚
```

`test/fixtures.generated.ts` 把 `schema/fixtures/projection/` 的五份 golden fixture 以字面量内联并 `satisfies` 校验——字面量保有 literal 类型（含 `projectionVersion: 1` 常量），`pnpm check` 即 TS 侧的 fixture 一致性测试；Rust 侧的等价测试在 `crates/pointlock-store/tests/projection.rs`。

## 导出

`FlowGraphView` / `TimelinePage`（含 `RunTimelineEntry`）/ `StepDossierView` / `HumanInboxEntry` / `RunOverview` 及其全部子类型。消费方：`@pointlock/ui`（M3a-W3）与任何未来渲染器。
