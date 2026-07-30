//! Shared harness for tests that need a real `devicerail-daemon`: locating
//! the sibling DeviceRail checkout, building the daemon binary, and a
//! minimal RAII temporary directory. Extracted from the M1 smoke test so
//! the provider integration tests reuse the same build/locate logic.

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use uuid::Uuid;

/// Budget for the nested `cargo build -p devicerail-daemon` invocation.
const DAEMON_BUILD_BUDGET: Duration = Duration::from_secs(600);

/// Locates the sibling DeviceRail checkout relative to this crate directory.
pub fn device_rail_repo() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest_dir.join("../../../device-rail");
    match repo.canonicalize() {
        Ok(repo) if repo.join("Cargo.toml").is_file() => repo,
        _ => panic!(
            "the DeviceRail sibling checkout was not found at {} \
             (expected `device-rail` next to the `pointlock` repository); \
             the M1 tests need it to build and spawn `devicerail-daemon`",
            repo.display()
        ),
    }
}

/// Builds `devicerail-daemon` in the sibling repository and returns the
/// resulting debug binary path.
pub fn build_daemon(repo: &Path) -> PathBuf {
    let target_dir = repo.join("target");
    let mut child = Command::new("cargo")
        .args(["build", "-p", "devicerail-daemon", "--quiet"])
        .current_dir(repo)
        // Pin the target directory so the binary path below is deterministic
        // even when the outer environment redirects CARGO_TARGET_DIR.
        .env("CARGO_TARGET_DIR", &target_dir)
        // Let the DeviceRail repository's own rust-toolchain.toml choose the
        // toolchain instead of inheriting this test invocation's pin.
        .env_remove("RUSTUP_TOOLCHAIN")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn `cargo build -p devicerail-daemon` in the DeviceRail repo");

    let deadline = Instant::now() + DAEMON_BUILD_BUDGET;
    let status = loop {
        match child.try_wait().expect("poll daemon build") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "building devicerail-daemon exceeded the {}s budget",
                    DAEMON_BUILD_BUDGET.as_secs()
                );
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    assert!(status.success(), "devicerail-daemon build failed: {status}");

    let binary = target_dir.join("debug").join("devicerail-daemon");
    assert!(
        binary.is_file(),
        "daemon binary missing after successful build: {}",
        binary.display()
    );
    binary
}

/// Minimal RAII temporary directory (avoids a `tempfile` dependency).
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temporary directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
