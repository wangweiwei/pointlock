# pointlock-vision

Pointlock's Anthropic-backed visual verifier (downgrade-only, evidence-backed).

An optional visual verifier that speaks raw HTTP to the Anthropic Messages API. It may only *downgrade* a verdict's confidence against content-addressed evidence, never fabricate a pass.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
