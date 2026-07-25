# Changelog

## 0.1.0-alpha.5 - 2026-07-26

### Changed

- Redesign the Dashboard and Nodes TUI around a shared tab bar, fixed status line, green structural borders, resource gauges, structured tables, and visible row selection.
- Show real aggregate CPU, memory, disk, Xray user usage, Agent state, and published node endpoints without synthesizing traffic history.
- Apply consistent field colors and active-row highlighting to subscription, node, and NIC forms.
- Move the loopback test subscription and health endpoint from port `18081` to `18181`; the test installer migrates its exact generated legacy address while preserving custom addresses.

### Tests

- Verify the redesigned pages with populated `120x36` terminal buffers and retain the all-pages `24x4` no-panic coverage.

## 0.1.0-alpha.4 - 2026-07-26

### Fixed

- Pass the explicit JSON format to Xray when its configuration is provided through a memfd path.
- Treat a stopped Panel gRPC, enrollment, or subscription server as a fatal process error instead of reporting a false healthy service.
- Stop existing services, clear systemd start limits, reject occupied loopback ports, and wait for the Xray API in the test installer.

### Tests

- Start the real embedded Xray from a memfd configuration during x86_64 release CI.

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
