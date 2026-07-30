# pointlock-runner

The Pointlock execution engine: state machine, verdict fold, crash-safe resume alignment, and localized repair.

Executes a sealed FlowIR. Every mutating action is write-ahead-logged before dispatch; `callId` reconcile makes restart exactly-once; dual-hash alignment resumes only what is unproven.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
