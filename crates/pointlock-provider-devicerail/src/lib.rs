//! # pointlock-provider-devicerail
//!
//! The DeviceRail implementation of the Pointlock provider SPI
//! (design doc 04 §9; spine §4). The provider is a capability declaration
//! plus faithful execution — it never folds, translates, improvises,
//! retries, or degrades (04 §1):
//!
//! - [`DeviceRailProvider`]: static [`devicerail_manifest`] + the atomic
//!   `openSession` sequence (spawn → `system.hello` → `devices.list` →
//!   `device.select` → `device.connect` → `device.capabilities` →
//!   attestation → `session.start`, 04 §9.3) with lockfile-digest
//!   attestation (04 §9.2).
//! - [`DeviceRailSession`]: the ten `ProviderSession` methods over one
//!   exclusively-owned `devicerail-client` connection.
//! - [`lock_via_spawn`] / [`make_lockfile`]: the `pointlock lock` freeze
//!   path (CLI wiring follows).
//!
//! Wire names follow spine A.8 verbatim; DTO transcription is
//! field-by-field (`convert`); error normalization implements the 04 §6 /
//! §9.6 tables (`error_map`).
//!
//! ## Documented divergences and gaps (honesty over fabrication)
//!
//! - `fetchEvidence` is a typed unsupported error: DeviceRail asset URIs
//!   (`devicerail://assets/sha256/<digest>`) have no byte channel on the
//!   control plane and `devicerail-client` exposes no fetch API.
//! - `PlatformKind` has no `mock` counterpart; the mock platform maps
//!   provisionally to `linux` on both the lock and attestation paths (see
//!   `convert::platform_kind_from_wire`).
//! - Client-side backpressure errors surface directly as `transport_lost`
//!   (04 §9.6 envisions provider-internal queueing; M1 has none).
//! - The spawn exit protocol's SIGTERM step is collapsed into the client's
//!   kill path (stdin EOF → grace → kill).

mod budget;
mod convert;
mod endpoint;
mod error_map;
mod lock;
mod manifest;
mod provider;
mod scan;
mod session;

pub use endpoint::{DEFAULT_DAEMON_COMMAND, DEFAULT_SHUTDOWN_GRACE_MS, SpawnSpec};
pub use error_map::{
    DRIVER_ERROR_RPC_CODE, classify_remote_rpc, classify_wire_code, execute_terminal_from_rpc,
    provider_error_from_client,
};
pub use lock::{lockfile_provider_identity, make_lockfile};
pub use manifest::{
    INFRA_REQUIRED_FEATURES, OFFERED_OPTIONAL_FEATURES, PROVIDER_NAME, devicerail_manifest,
};
pub use provider::{DeviceRailProvider, lock_via_spawn, lock_via_spawn_at};
pub use session::DeviceRailSession;
