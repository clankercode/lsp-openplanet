#!/usr/bin/env bash
# Local smoke: build the host binary, stuff it into the matching npm platform
# package, pack meta + platform, and run --version through the node launcher.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
echo "version=${VERSION}"

# Do not paper over a missed bump: the release workflow requires these source
# manifests and all meta-package platform pins to already be in lockstep.
node - "${VERSION}" <<'NODE'
const fs = require('fs');
const path = require('path');
const version = process.argv[2];
const errors = [];
const manifests = fs.readdirSync('npm')
  .map((dir) => path.join('npm', dir, 'package.json'))
  .filter((file) => fs.existsSync(file))
  .map((file) => ({ file, manifest: JSON.parse(fs.readFileSync(file, 'utf8')) }));
for (const { file, manifest } of manifests) {
  if (manifest.version !== version) {
    errors.push(`${file} version (${manifest.version}) != Cargo.toml (${version})`);
  }
}
const meta = manifests.find(({ manifest }) => manifest.name === 'openplanet-lsp');
if (!meta) errors.push('npm/openplanet-lsp meta package is missing');
for (const [packageName, pin] of Object.entries(meta?.manifest.optionalDependencies || {})) {
  if (pin !== version) errors.push(`${packageName} pin (${pin}) != Cargo.toml (${version})`);
}
if (errors.length > 0) {
  console.error(errors.join('\n'));
  process.exit(1);
}
NODE

echo "== cargo build --release =="
cargo build --release --locked

HOST_BIN="${ROOT}/target/release/openplanet-lsp"
if [[ ! -f "${HOST_BIN}" ]]; then
  HOST_BIN="${ROOT}/target/release/openplanet-lsp.exe"
fi
test -f "${HOST_BIN}"

# Map host → npm package slug
HOST="$(uname -s)-$(uname -m)"
case "${HOST}" in
  Linux-x86_64)   SLUG=linux-x64; BIN_NAME=openplanet-lsp ;;
  Linux-aarch64)  SLUG=linux-arm64; BIN_NAME=openplanet-lsp ;;
  Darwin-x86_64)  SLUG=darwin-x64; BIN_NAME=openplanet-lsp ;;
  Darwin-arm64)   SLUG=darwin-arm64; BIN_NAME=openplanet-lsp ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64)
    SLUG=win32-x64; BIN_NAME=openplanet-lsp.exe ;;
  MINGW*-aarch64|MSYS*-aarch64|CYGWIN*-aarch64|MINGW*-arm64|MSYS*-arm64|CYGWIN*-arm64)
    SLUG=win32-arm64; BIN_NAME=openplanet-lsp.exe ;;
  *)
    echo "Unsupported host for local smoke: ${HOST}" >&2
    exit 1
    ;;
esac

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
STAGE_ROOT="${TMP}/npm"
mkdir -p "${STAGE_ROOT}"
cp -R "npm/openplanet-lsp-${SLUG}" "${STAGE_ROOT}/"
cp -R npm/openplanet-lsp "${STAGE_ROOT}/"

PKG_DIR="${STAGE_ROOT}/openplanet-lsp-${SLUG}"
mkdir -p "${PKG_DIR}/bin"
cp "${HOST_BIN}" "${PKG_DIR}/bin/${BIN_NAME}"
chmod +x "${PKG_DIR}/bin/${BIN_NAME}" || true

echo "== npm pack platform + meta =="
(cd "${STAGE_ROOT}" && npm pack "./openplanet-lsp-${SLUG}" --pack-destination "$TMP")
(cd "${STAGE_ROOT}" && npm pack ./openplanet-lsp --pack-destination "$TMP")
ls -lh "$TMP"

echo "== install into temp prefix =="
PREFIX="$TMP/prefix"
mkdir -p "$PREFIX"
npm install --prefix "$PREFIX" "$TMP"/openplanet-lsp-"${VERSION}".tgz "$TMP"/openplanet-lsp-"${SLUG}"-"${VERSION}".tgz

echo "== run launcher --version =="
"$PREFIX/node_modules/.bin/openplanet-lsp" --version

echo "OK: local smoke passed for ${SLUG}"
