# pointlock-provider-kit

The Pointlock Provider SPI: the Rust trait a backend implements to execute FlowIR actions.

The stable provider contract. Implement this trait to teach Pointlock a new execution backend (devices, browsers, HTTP, CLI). DeviceRail is the first implementation.

Part of [**Pointlock**](https://github.com/wangweiwei/pointlock) — compile natural-language tasks into a typed, capability-bound, crash-safe flow IR. See the [architecture spine](https://github.com/wangweiwei/pointlock/blob/main/docs/design/00-architecture-spine.md) for how this crate fits in.

## License

Licensed under the [Apache License 2.0](https://github.com/wangweiwei/pointlock/blob/main/LICENSE).
