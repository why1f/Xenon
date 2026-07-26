# Changelog

## 0.1.0-alpha.10 - 2026-07-26

### Changed

- Rework the Xray node editor into a focused sb-manager-style form: protocol and managed host are row-level selectors, and left/right only changes the currently selected option.
- Reduce protocol forms to connection essentials. Publish overrides and Reality client parameters are no longer entered in the TUI; existing hidden values remain intact when editing.
- Keep subscription UUID generation automatic and generate Reality short IDs internally instead of exposing credential fields.
- Replace the repeated raw Agent registration result with a concise explanation when `[agent_install]` is disabled.

### Fixed

- Accept the production installer's per-architecture Agent SHA-256 values and local CA path when validating `[agent_install]`.
- Update the production installer hint to use `a` for managed-host creation.

## 0.1.0-alpha.9 - 2026-07-26

### Added

- Add `R` manual refresh across the five primary TUI pages and user management views.
- Add a dedicated subscription node picker: press `n` in user detail, move with arrow keys, toggle active Xray nodes with Space, and save with Enter.
- Allow an existing managed host to issue a fresh one-hour Agent registration token and display its installation command with `i`.

### Changed

- Standardize user, Xray node, and managed-host creation on the `a` shortcut while retaining the old create keys for compatibility.
- Redesign the Xray node form around left/right protocol switching and protocol-specific fields, following the compact sb-manager layout without introducing sing-box.
- Permit disabled Reality node records to wait for Agent-generated public parameters, while refusing activation until required credentials exist.

### Fixed

- Show the complete one-time Agent installation command in a persistent, wrapping TUI dialog after creating a managed host instead of briefly clipping it in the one-line footer.
- Preserve one-time operation results across periodic TUI refreshes until the operator dismisses them.
- Keep interactive tracing output out of the alternate screen so server log lines no longer remain mixed into TUI tables and footers.

## 0.1.0-alpha.8 - 2026-07-26

### Added

- Separate Xray protocol-node records and subscription assignments, allowing multiple protocol nodes to belong to one managed Agent host.
- Dedicated host and Agent event pages, plus per-host NIC rates and cumulative RX/TX counters.

### Changed

- TUI now has 仪表盘/用户/节点/主机/日志 tabs with separate CRUD flows for Xray protocol nodes and managed Agent hosts.
- New protocol nodes remain disabled until Agent multi-inbound deployment is implemented; existing single-inbound nodes are migrated without changing IDs or subscription assignments.

### Fixed

- Include VLESS WebSocket paths and VLESS Encryption values in generated VLESS subscription links.

### Known limitations

- Agent reconciliation still manages one local VLESS inbound. Multi-inbound deployment, Agent-side Reality key generation, per-protocol-node Xray identities, and SS2022 credentials remain pending.

## 0.1.0-alpha.7 - 2026-07-26

### Added

- `scripts/install-panel.sh`: production Panel one-line install that downloads the latest release, generates a self-signed server CA/certificate and Agent client CA, writes an mTLS + Enrollment configuration, and starts the systemd service; `--uninstall`/`--purge` remove the Panel (and a detected loopback test Agent) again.
- Releases ship raw per-architecture `xenon-agent` binaries with SHA-256 digests.
- The TUI-generated Agent install command resolves `{arch}` on the target machine, pins per-architecture digests, and embeds the Panel CA via `--ca-b64`, so one command enrolls any x86_64/aarch64 VPS.
- `xenon-tui` wrapper: stops the headless service, opens the TUI, and restarts the service on exit.

### Changed

- TUI now has 仪表盘/用户/节点 tabs: users get a dedicated page with the summary strip and full ranking; the dashboard keeps a Top-5 view.
- All create/edit/confirm flows render as centered modal dialogs over their parent page with Chinese labels and unified key hints.
- Panel startup clears stale `online` agent/node presence left behind by an unclean shutdown.
- The test bootstrap defaults to the latest release instead of a pinned tag.

## 0.1.0-alpha.6 - 2026-07-26

### Added

- Dashboard realtime downlink/uplink rate sparklines computed from the latest NIC absolute counters, with automatic rebaselining after an Agent counter reset.
- Dashboard user summary strip with enabled, over-quota, and expired counts plus the cycle charged total.
- User ranking quota column showing the aggregate traffic limit and usage percentage; users with any unlimited subscription show `∞`.

### Tests

- Cover NIC rate computation, unchanged-sample holds, and counter-reset rebaselining in a dedicated tracker test.

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
