#!/usr/bin/env bash
# Smoke: pack host npm packages, install into a temp prefix, pretend to be an
# older version, run `openplanet-lsp update`, and confirm the launcher still works.
#
# Dev/CI overrides used:
#   OPENPLANET_LSP_VERSION / OPENPLANET_LSP_LATEST_VERSION / OPENPLANET_LSP_UPDATE_PACKAGE
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

HOST="$(uname -s)-$(uname -m)"
case "${HOST}" in
  Linux-x86_64)   SLUG=linux-x64; BIN_NAME=openplanet-lsp ;;
  Linux-aarch64)  SLUG=linux-arm64; BIN_NAME=openplanet-lsp ;;
  Darwin-x86_64)  SLUG=darwin-x64; BIN_NAME=openplanet-lsp ;;
  Darwin-arm64)   SLUG=darwin-arm64; BIN_NAME=openplanet-lsp ;;
  *)
    echo "Unsupported host for self-update smoke: ${HOST}" >&2
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
chmod +x "${PKG_DIR}/bin/${BIN_NAME}"

echo "== npm pack platform + meta =="
(cd "${STAGE_ROOT}" && npm pack "./openplanet-lsp-${SLUG}" --pack-destination "$TMP")
(cd "${STAGE_ROOT}" && npm pack ./openplanet-lsp --pack-destination "$TMP")
META_TGZ="${TMP}/openplanet-lsp-${VERSION}.tgz"
PLAT_TGZ="${TMP}/openplanet-lsp-${SLUG}-${VERSION}.tgz"
test -f "${META_TGZ}"
test -f "${PLAT_TGZ}"

echo "== install into temp prefix =="
PREFIX="${TMP}/prefix"
mkdir -p "${PREFIX}"
npm install --prefix "${PREFIX}" "${META_TGZ}" "${PLAT_TGZ}"
BIN="${PREFIX}/node_modules/.bin/openplanet-lsp"
test -x "${BIN}"

echo "== baseline --version =="
BEFORE="$("${BIN}" --version)"
echo "${BEFORE}"
echo "${BEFORE}" | grep -F "openplanet-lsp ${VERSION}"

CFG="${TMP}/cfg"
mkdir -p "${CFG}"
export OPENPLANET_LSP_CONFIG_DIR="${CFG}"

echo "== pretend older + check =="
export OPENPLANET_LSP_VERSION="0.0.0"
export OPENPLANET_LSP_LATEST_VERSION="${VERSION}"
export OPENPLANET_LSP_UPDATE_PACKAGE="${META_TGZ} ${PLAT_TGZ}"
CHECK_OUT="$("${BIN}" update --check)"
echo "${CHECK_OUT}"
echo "${CHECK_OUT}" | grep -F 'status:   update available'
echo "${CHECK_OUT}" | grep -F 'method:   npm-local'
test -f "${CFG}/update-status.json"
python3 - <<'PY'
import json, os
p = os.path.join(os.environ["OPENPLANET_LSP_CONFIG_DIR"], "update-status.json")
s = json.load(open(p))
assert s["current_version"] == "0.0.0", s
assert s["latest_version"] == os.environ["OPENPLANET_LSP_LATEST_VERSION"], s
assert s["update_available"] is True, s
assert s["install_method"] == "npm-local", s
print("status json ok")
PY

echo "== apply self-update via local tarballs =="
APPLY_OUT="$("${BIN}" update)"
echo "${APPLY_OUT}"
echo "${APPLY_OUT}" | grep -F 'Update command finished'
echo "${APPLY_OUT}" | grep -F 'installed — restart required'
echo "${APPLY_OUT}" | grep -F "installed: ${VERSION}"
python3 - <<'PY'
import json, os
p = os.path.join(os.environ["OPENPLANET_LSP_CONFIG_DIR"], "update-status.json")
s = json.load(open(p))
assert s["update_available"] is False, s
assert s["pending_restart"] is True, s
assert s["installed_version"] == os.environ["OPENPLANET_LSP_LATEST_VERSION"], s
print("post-apply status json ok")
PY

echo "== post-update still works =="
AFTER="$("${BIN}" --version)"
echo "${AFTER}"
echo "${AFTER}" | grep -F "openplanet-lsp ${VERSION}"
"${BIN}" update --help >/dev/null
"${BIN}" check --help >/dev/null

unset OPENPLANET_LSP_VERSION OPENPLANET_LSP_LATEST_VERSION OPENPLANET_LSP_UPDATE_PACKAGE
echo "== real check after update (no pretend) =="
# Latest may lag registry; just ensure check does not crash and writes status.
"${BIN}" update --check >/dev/null
test -f "${CFG}/update-status.json"

echo "OK: self-update smoke passed for ${SLUG} (${VERSION})"
