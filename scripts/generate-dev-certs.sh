#!/usr/bin/env bash
set -euo pipefail

# Development-only certificates for the configured tonic mTLS endpoints.
# Do not use these files in production and do not commit the output directory.

OUT_DIR="${1:-dev-certs}"
mkdir -p "$OUT_DIR"
chmod 700 "$OUT_DIR"

openssl req -x509 -newkey rsa:3072 -nodes -days 825 \
  -subj "/CN=xenon-dev-ca" \
  -keyout "$OUT_DIR/ca.key" -out "$OUT_DIR/ca.crt"

openssl req -newkey rsa:3072 -nodes \
  -subj "/CN=panel.internal" \
  -keyout "$OUT_DIR/server.key" -out "$OUT_DIR/server.csr"
printf '%s\n' \
  'basicConstraints=CA:FALSE' \
  'keyUsage=digitalSignature,keyEncipherment' \
  'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:panel.internal,IP:127.0.0.1' > "$OUT_DIR/server.ext"
openssl x509 -req -days 825 -sha256 \
  -in "$OUT_DIR/server.csr" -CA "$OUT_DIR/ca.crt" -CAkey "$OUT_DIR/ca.key" \
  -CAcreateserial -extfile "$OUT_DIR/server.ext" \
  -out "$OUT_DIR/server.crt"

openssl req -newkey rsa:3072 -nodes \
  -subj "/CN=xenon-agent-dev" \
  -keyout "$OUT_DIR/agent.key" -out "$OUT_DIR/agent.csr"
printf '%s\n' \
  'basicConstraints=CA:FALSE' \
  'keyUsage=digitalSignature,keyEncipherment' \
  'extendedKeyUsage=clientAuth' > "$OUT_DIR/agent.ext"
openssl x509 -req -days 825 -sha256 \
  -in "$OUT_DIR/agent.csr" -CA "$OUT_DIR/ca.crt" -CAkey "$OUT_DIR/ca.key" \
  -CAcreateserial -extfile "$OUT_DIR/agent.ext" \
  -out "$OUT_DIR/agent.crt"

rm -f "$OUT_DIR"/*.csr "$OUT_DIR"/*.ext "$OUT_DIR"/*.srl
chmod 600 "$OUT_DIR"/*.key
printf 'Generated development certificates in %s\n' "$OUT_DIR"
