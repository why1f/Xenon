# Changelog

## 0.1.0-alpha.3 - 2026-07-26

### Fixed

- Build x86_64 and ARM64 release binaries for a minimum glibc 2.17 runtime instead of inheriting Ubuntu 24.04's glibc 2.39 requirement.
- Reject release artifacts in CI when their required GLIBC symbol version exceeds 2.17.

## 0.1.0-alpha.2 - 2026-07-26

### Added

- Loopback-only single-host test installer in `scripts/install-test.sh`.
- CI syntax checks for all Bash scripts.

### Fixed

- Grant the non-root Agent service `CAP_NET_BIND_SERVICE` so embedded Xray can bind port 443.

## 0.1.0-alpha.1 - 2026-07-26

First Xenon test release.

### Included

- Rust TUI control plane and Linux Agent workspace.
- SQLite accounting, subscriptions, node management, backup and restore.
- Xray-core `26.6.27` embedding with Linux memfd execution.
- Xray user traffic and server interface accounting.
- mTLS enrollment, certificate binding, rotation and revocation.
- Linux x86_64 and arm64 build artifacts with SHA-256 files.

### Known limitations

- This is a prerelease and is not production-ready.
- Real Linux systemd and full end-to-end validation remain required.
- 100/500/1000-user Xray load testing has not been completed.
