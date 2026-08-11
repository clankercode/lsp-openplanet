#!/usr/bin/env bash
# Local smoke: build the host binary, stuff it into the matching npm platform
# package, pack meta + platform, and run --version through the node launcher.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
echo "version=${VERSION}"

echo "== cargo build --release =="
cargo build --release --locked

HOST_BIN="${ROOT}/target/release/openplanet-lsp"
if [[ ! -f "${HOST_BIN}" ]]; then
  HOST_BIN="${ROOT}/target/release/openplanet-lsp.exe"
fi
test -f "${HOST_BIN}"

# Map host → npm package slug
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)   SLUG=linux-x64; BIN_NAME=openplanet-lsp ;;
  Linux-aarch64)  SLUG=linux-arm64; BIN_NAME=openplanet-lsp ;;
  Darwin-x86_64)  SLUG=darwin-x64; BIN_NAME=openplanet-lsp ;;
  Darwin-arm64)   SLUG=darwin-arm64; BIN_NAME=openplanet-lsp ;;
  MINGW*|MSYS*|CYGWIN*) SLUG=win32-x64; BIN_NAME=openplanet-lsp.exe ;;
  *)
    echo "Unsupported host for local smoke: $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

PKG_DIR="npm/openplanet-lsp-${SLUG}"
mkdir -p "${PKG_DIR}/bin"
cp "${HOST_BIN}" "${PKG_DIR}/bin/${BIN_NAME}"
chmod +x "${PKG_DIR}/bin/${BIN_NAME}" || true

# Pin versions
node <<NODE
const fs = require('fs');
const version = '${VERSION}';
for (const dir of fs.readdirSync('npm')) {
  const p = \`npm/\${dir}/package.json\`;
  if (!fs.existsSync(p)) continue;
  const j = JSON.parse(fs.readFileSync(p, 'utf8'));
  j.version = version;
  if (j.optionalDependencies) {
    for (const k of Object.keys(j.optionalDependencies)) j.optionalDependencies[k] = version;
  }
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + '\n');
}
NODE

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "== npm pack platform + meta =="
(cd npm && npm pack "./openplanet-lsp-${SLUG}" --pack-destination "$TMP")
(cd npm && npm pack ./openplanet-lsp --pack-destination "$TMP")
ls -lh "$TMP"

echo "== install into temp prefix =="
PREFIX="$TMP/prefix"
mkdir -p "$PREFIX"
npm install --prefix "$PREFIX" "$TMP"/openplanet-lsp-"${VERSION}".tgz "$TMP"/openplanet-lsp-"${SLUG}"-"${VERSION}".tgz

echo "== run launcher --version =="
"$PREFIX/node_modules/.bin/openplanet-lsp" --version

echo "OK: local smoke passed for ${SLUG}"
