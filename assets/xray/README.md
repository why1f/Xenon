# Xray build asset

Linux release builds embed exactly `Xray-core v26.6.27`. Do not commit an unverified binary or use an upstream `latest` URL.

Provide the extracted executable to Cargo with all three variables:

```bash
XRAY_BINARY_PATH=/secure/build-cache/xray \
XRAY_BINARY_VERSION=26.6.27 \
XRAY_BINARY_SHA256=<sha256-of-extracted-executable> \
cargo build --release --target x86_64-unknown-linux-musl -p xenon-agent
```

Use the matching official Linux executable for each target architecture. The Agent build script rejects another version, an empty file, or a SHA-256 mismatch. A Linux release build without an asset fails instead of producing an Agent without Xray.
