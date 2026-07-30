# pointlock-compiler

Compiles Pointlock YAML tasks into a sealed, capability-bound FlowIR (parse to normalize to check to bind to seal).

The five-stage pipeline that turns a `*.flow.yaml` task into a sealed FlowIR. Capabilities are bound against a provider lockfile; a missing capability is a compile error, not a runtime surprise.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
