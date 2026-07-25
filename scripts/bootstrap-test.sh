#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() {
  printf 'bootstrap-test: %s\n' "$1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail "run as root"
[ "$(uname -s)" = "Linux" ] || fail "Linux is required"

for command_name in curl sha256sum tar; do
  command -v "$command_name" >/dev/null 2>&1 || \
    fail "$command_name is required"
done

case "$(uname -m)" in
  x86_64 | amd64)
    architecture="x86_64"
    ;;
  aarch64 | arm64)
    architecture="aarch64"
    ;;
  *)
    fail "unsupported architecture: $(uname -m)"
    ;;
esac

repository="${XENON_REPOSITORY:-why1f/Xenon}"
version="${XENON_VERSION:-v0.1.0-alpha.2}"
artifact="xenon-linux-${architecture}"
archive="${artifact}.tar.gz"
checksum="${archive}.sha256"
release_url="https://github.com/${repository}/releases/download/${version}"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

printf 'Downloading Xenon %s for %s...\n' "$version" "$architecture"
curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
  "$release_url/$archive" --output "$work_dir/$archive"
curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
  "$release_url/$checksum" --output "$work_dir/$checksum"

(
  cd "$work_dir"
  sha256sum --check --strict "$checksum"
  tar -xzf "$archive"
  "./$artifact/scripts/install-test.sh"
)
