# pointlock-store

Pointlock's event-sourced RunLog, SQLite/WAL checkpoints, evidence store, and read-side projections.

The durability layer. An append-only RunLog folds into SQLite/WAL checkpoints (with a `verify_checkpoint` invariant), a content-addressed evidence store, and the read-side projection DTOs.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
