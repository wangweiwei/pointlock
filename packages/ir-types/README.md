# @pointlock/ir-types

Type-only surface generated from the `pointlock-ir` Rust DTOs (R12): the
`FlowIR` schema (`schema/generated/flow-ir.schema.json`) compiled to
TypeScript, with every positive golden fixture `satisfies`-checked
against the generated root type (`pnpm check` = the TS leg of the
golden-fixture consistency test, 02 §1.1).

```bash
cargo run -p pointlock-ir --bin pointlock-ir-schema-gen   # re-emit the schema
pnpm --filter @pointlock/ir-types generate               # regenerate src/
pnpm --filter @pointlock/ir-types check                  # fixture anchor
```

Do not edit `src/` by hand.
