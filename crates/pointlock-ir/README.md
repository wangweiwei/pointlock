# pointlock-ir

Pointlock's Typed IR: the content-addressed, dual-hash intermediate representation for capability-bound flows.

The source of type truth for Pointlock. These serde + schemars DTOs generate the JSON Schema, the `@pointlock/*` TypeScript types, and the golden fixtures; the IR carries a dual-hash identity and is the durable contract the runner executes.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
