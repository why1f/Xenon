param(
    [string]$OutDir = "dev-certs"
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Invoke-OpenSsl {
    & openssl @args
    if ($LASTEXITCODE -ne 0) {
        throw "openssl failed with exit code $LASTEXITCODE"
    }
}

# Development-only certificates. Never use or commit this output in production.
Invoke-OpenSsl req -x509 -newkey rsa:3072 -nodes -days 825 `
    -subj "/CN=xenon-dev-ca" `
    -keyout "$OutDir/ca.key" -out "$OutDir/ca.crt"

Invoke-OpenSsl req -newkey rsa:3072 -nodes `
    -subj "/CN=panel.internal" `
    -keyout "$OutDir/server.key" -out "$OutDir/server.csr"
@(
    "basicConstraints=CA:FALSE"
    "keyUsage=digitalSignature,keyEncipherment"
    "extendedKeyUsage=serverAuth"
    "subjectAltName=DNS:panel.internal,IP:127.0.0.1"
) | Set-Content -Encoding ascii "$OutDir/server.ext"
Invoke-OpenSsl x509 -req -days 825 -sha256 `
    -in "$OutDir/server.csr" -CA "$OutDir/ca.crt" -CAkey "$OutDir/ca.key" `
    -CAcreateserial -extfile "$OutDir/server.ext" -out "$OutDir/server.crt"

Invoke-OpenSsl req -newkey rsa:3072 -nodes `
    -subj "/CN=xenon-agent-dev" `
    -keyout "$OutDir/agent.key" -out "$OutDir/agent.csr"
@(
    "basicConstraints=CA:FALSE"
    "keyUsage=digitalSignature,keyEncipherment"
    "extendedKeyUsage=clientAuth"
) | Set-Content -Encoding ascii "$OutDir/agent.ext"
Invoke-OpenSsl x509 -req -days 825 -sha256 `
    -in "$OutDir/agent.csr" -CA "$OutDir/ca.crt" -CAkey "$OutDir/ca.key" `
    -CAcreateserial -extfile "$OutDir/agent.ext" -out "$OutDir/agent.crt"

Remove-Item "$OutDir/server.csr", "$OutDir/server.ext", "$OutDir/agent.csr", "$OutDir/agent.ext", "$OutDir/ca.srl"
Write-Output "Generated development certificates in $OutDir"
