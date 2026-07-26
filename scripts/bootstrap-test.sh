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
version="${XENON_VERSION:-}"
if [ -z "$version" ]; then
  version="$(curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
    "https://api.github.com/repos/${repository}/releases?per_page=1" |
    grep -o '"tag_name": *"[^"]*"' | head -n 1 | cut -d '"' -f 4)" || true
  [ -n "$version" ] || \
    fail "unable to determine the latest release; set XENON_VERSION explicitly"
fi
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
