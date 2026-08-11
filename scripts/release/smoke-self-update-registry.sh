#!/usr/bin/env bash
# Registry self-update smoke across JS package managers.
#
# For each PM in PACKAGE_MANAGERS (default: npm pnpm yarn bun):
#   1. install openplanet-lsp@FROM_VERSION via that PM (global)
#   2. pretend to be older (OPENPLANET_LSP_VERSION=0.0.0)
#   3. run update toward TARGET (registry @latest by default)
#   4. verify the binary still runs
#   5. uninstall via the same PM
#
# Env:
#   FROM_VERSION        version to install first (default: latest)
#   TARGET_VERSION      update target version or "latest" (default: latest)
#   PACKAGE_MANAGERS    space-separated list (default: npm pnpm yarn bun)
#   SKIP_UNINSTALL=1    leave packages installed (debug)
#   WAIT_FOR_VERSION    if set, poll npm until this version is visible first
#   UPDATER_BIN         binary used to run `update` (default: installed bin if it
#                       supports `update`, else ./target/release/openplanet-lsp)
#   NPM_GLOBAL_PREFIX   writable npm global prefix (default: $HOME/.local)
#
# Not for every PR — use after a release or via workflow_dispatch.
set -euo pipefail

FROM_VERSION="${FROM_VERSION:-latest}"
TARGET_VERSION="${TARGET_VERSION:-latest}"
PACKAGE_MANAGERS="${PACKAGE_MANAGERS:-npm pnpm yarn bun}"
SKIP_UNINSTALL="${SKIP_UNINSTALL:-0}"
WAIT_FOR_VERSION="${WAIT_FOR_VERSION:-}"
NPM_GLOBAL_PREFIX="${NPM_GLOBAL_PREFIX:-${HOME}/.local}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

log() { printf '+ %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

export PATH="${NPM_GLOBAL_PREFIX}/bin:${HOME}/.local/share/pnpm:${HOME}/.bun/bin:${HOME}/.yarn/bin:${PATH}"
if command -v yarn >/dev/null 2>&1; then
  yarn_bin="$(yarn global bin 2>/dev/null || true)"
  if [[ -n "${yarn_bin}" ]]; then
    export PATH="${yarn_bin}:${PATH}"
  fi
fi

wait_for_registry() {
  local ver="$1"
  [[ -z "${ver}" || "${ver}" == "latest" ]] && return 0
  log "waiting for openplanet-lsp@${ver} on npm registry..."
  local i
  for i in $(seq 1 20); do
    if npm view "openplanet-lsp@${ver}" version >/dev/null 2>&1; then
      log "registry has ${ver}"
      return 0
    fi
    log "  try ${i}/20..."
    sleep 15
  done
  die "openplanet-lsp@${ver} not visible on npm after wait"
}

resolve_spec() {
  local ver="$1"
  if [[ "${ver}" == "latest" ]]; then
    echo "openplanet-lsp@latest"
  else
    echo "openplanet-lsp@${ver}"
  fi
}

install_pm() {
  local pm="$1" spec="$2"
  case "${pm}" in
    npm)
      mkdir -p "${NPM_GLOBAL_PREFIX}"
      npm install -g --prefix "${NPM_GLOBAL_PREFIX}" "${spec}"
      ;;
    pnpm) pnpm add -g "${spec}" ;;
    yarn) yarn global add "${spec}" ;;
    bun)  bun add -g "${spec}" ;;
    *) die "unknown package manager: ${pm}" ;;
  esac
}

uninstall_pm() {
  local pm="$1"
  case "${pm}" in
    npm)  npm uninstall -g --prefix "${NPM_GLOBAL_PREFIX}" openplanet-lsp || true ;;
    pnpm) pnpm remove -g openplanet-lsp || true ;;
    yarn) yarn global remove openplanet-lsp || true ;;
    bun)  bun remove -g openplanet-lsp || true ;;
    *) die "unknown package manager: ${pm}" ;;
  esac
}

# Resolve the native binary installed by a given PM (not an unrelated PATH hit).
find_installed_bin() {
  local pm="$1"
  local candidates=()
  case "${pm}" in
    npm)
      candidates+=(
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp-darwin-x64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp-darwin-arm64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp/node_modules/openplanet-lsp-darwin-x64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/lib/node_modules/openplanet-lsp/node_modules/openplanet-lsp-darwin-arm64/bin/openplanet-lsp"
        "${NPM_GLOBAL_PREFIX}/bin/openplanet-lsp"
      )
      if [[ -d "${NPM_GLOBAL_PREFIX}/lib/node_modules" ]]; then
        while IFS= read -r -d '' f; do
          candidates+=("$f")
        done < <(find "${NPM_GLOBAL_PREFIX}/lib/node_modules" -type f -name openplanet-lsp -print0 2>/dev/null | head -z -n 20)
      fi
      ;;
    pnpm)
      candidates+=(
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp-darwin-x64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp-darwin-arm64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/global/5/node_modules/openplanet-lsp/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
        "${HOME}/.local/share/pnpm/openplanet-lsp"
      )
      # Prefer global package tree, never the content-addressable store.
      if [[ -d "${HOME}/.local/share/pnpm/global" ]]; then
        while IFS= read -r -d '' f; do
          candidates+=("$f")
        done < <(find "${HOME}/.local/share/pnpm/global" -type f -name openplanet-lsp -print0 2>/dev/null | head -z -n 20)
      fi
      ;;
    yarn)
      local yroot
      yroot="$(yarn global dir 2>/dev/null || true)"
      if [[ -n "${yroot}" ]]; then
        candidates+=(
          "${yroot}/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
          "${yroot}/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
          "${yroot}/node_modules/openplanet-lsp-darwin-x64/bin/openplanet-lsp"
          "${yroot}/node_modules/openplanet-lsp-darwin-arm64/bin/openplanet-lsp"
        )
      fi
      candidates+=(
        "${HOME}/.config/yarn/global/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${HOME}/.yarn/bin/openplanet-lsp"
      )
      ;;
    bun)
      candidates+=(
        "${HOME}/.bun/bin/openplanet-lsp"
        "${HOME}/.bun/install/global/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp"
        "${HOME}/.bun/install/global/node_modules/openplanet-lsp-linux-arm64/bin/openplanet-lsp"
        "${HOME}/.bun/install/global/node_modules/openplanet-lsp-darwin-x64/bin/openplanet-lsp"
        "${HOME}/.bun/install/global/node_modules/openplanet-lsp-darwin-arm64/bin/openplanet-lsp"
      )
      ;;
  esac

  local c
  # Prefer native platform package binaries over JS shims.
  for c in "${candidates[@]}"; do
    if [[ -n "${c}" && -x "${c}" && "${c}" == *"/openplanet-lsp-"*"/bin/openplanet-lsp" ]]; then
      echo "${c}"
      return 0
    fi
  done
  for c in "${candidates[@]}"; do
    if [[ -n "${c}" && -x "${c}" && "${c}" != *.js ]]; then
      # Skip JS launchers when a better candidate may exist later — already scanned natives.
      if file -b "${c}" 2>/dev/null | grep -qiE 'ELF|Mach-O|executable'; then
        echo "${c}"
        return 0
      fi
    fi
  done
  for c in "${candidates[@]}"; do
    if [[ -n "${c}" && -x "${c}" ]]; then
      echo "${c}"
      return 0
    fi
  done
  return 1
}

ensure_local_updater() {
  local bin="${ROOT}/target/release/openplanet-lsp"
  # Rebuild when sources are newer than the binary (or binary missing).
  if [[ ! -x "${bin}" || "src/update.rs" -nt "${bin}" || "src/main.rs" -nt "${bin}" ]]; then
    log "building local release updater binary..."
    cargo build --release --locked
  fi
  [[ -x "${bin}" ]] || die "missing ${bin}"
  "${bin}" update --help >/dev/null
  echo "${bin}"
}

pick_updater() {
  local installed="$1"
  if [[ -n "${UPDATER_BIN:-}" ]]; then
    echo "${UPDATER_BIN}"
    return 0
  fi
  # Prefer installed binary when it already has the update subcommand.
  if "${installed}" update --help >/dev/null 2>&1; then
    # Prefer local build if it is newer feature-wise and installed lacks VERSION override.
    if OPENPLANET_LSP_VERSION=0.0.0 "${installed}" update --check 2>/dev/null | grep -q 'current:  0.0.0'; then
      echo "${installed}"
      return 0
    fi
  fi
  ensure_local_updater
}

expected_method() {
  local pm="$1"
  echo "${pm}-global"
}

smoke_one() {
  local pm="$1"
  local from_spec target_spec
  from_spec="$(resolve_spec "${FROM_VERSION}")"
  target_spec="$(resolve_spec "${TARGET_VERSION}")"
  local expect_method
  expect_method="$(expected_method "${pm}")"

  log "======== ${pm}: install ${from_spec} ========"
  need_cmd "${pm}"
  uninstall_pm "${pm}" >/dev/null 2>&1 || true
  install_pm "${pm}" "${from_spec}"

  local installed
  installed="$(find_installed_bin "${pm}")" \
    || die "${pm}: could not locate installed openplanet-lsp binary"
  log "installed_bin=${installed}"
  "${installed}" --version

  local updater
  updater="$(pick_updater "${installed}")"
  log "updater_bin=${updater}"

  local cfg
  cfg="$(mktemp -d "${TMPDIR:-/tmp}/oplsp-su-${pm}-XXXXXX")"
  export OPENPLANET_LSP_CONFIG_DIR="${cfg}"
  export OPENPLANET_LSP_VERSION="0.0.0"
  export OPENPLANET_LSP_EXE="${installed}"
  export OPENPLANET_LSP_PACKAGE_MANAGER="${pm}"
  unset OPENPLANET_LSP_LATEST_VERSION || true
  export OPENPLANET_LSP_UPDATE_PACKAGE="${target_spec}"
  # Keep npm -g writes under the same user prefix used for install.
  if [[ "${pm}" == "npm" ]]; then
    export npm_config_prefix="${NPM_GLOBAL_PREFIX}"
    export NPM_CONFIG_PREFIX="${NPM_GLOBAL_PREFIX}"
  else
    unset npm_config_prefix NPM_CONFIG_PREFIX || true
  fi

  log "${pm}: update --check (pretend 0.0.0 → ${TARGET_VERSION})"
  local check_out
  check_out="$("${updater}" update --check)"
  printf '%s\n' "${check_out}"
  printf '%s\n' "${check_out}" | grep -F 'current:  0.0.0' >/dev/null \
    || die "${pm}: expected current 0.0.0 from VERSION override"
  printf '%s\n' "${check_out}" | grep -F 'status:   update available' >/dev/null \
    || die "${pm}: expected update available"
  printf '%s\n' "${check_out}" | grep -F "method:   ${expect_method}" >/dev/null \
    || die "${pm}: expected method ${expect_method}"

  log "${pm}: update (apply)"
  local apply_out
  apply_out="$("${updater}" update)"
  printf '%s\n' "${apply_out}"
  printf '%s\n' "${apply_out}" | grep -F 'Update command finished' >/dev/null \
    || die "${pm}: expected Update command finished"
  printf '%s\n' "${apply_out}" | grep -F 'installed — restart required' >/dev/null \
    || die "${pm}: expected pending restart status"

  # Re-resolve installed path after replace.
  installed="$(find_installed_bin "${pm}")" \
    || die "${pm}: openplanet-lsp missing after update"
  "${installed}" --version
  # Use updater for help if installed is still an older feature set.
  if "${installed}" update --help >/dev/null 2>&1; then
    "${installed}" update --help >/dev/null
  fi
  if "${installed}" check --help >/dev/null 2>&1; then
    "${installed}" check --help >/dev/null
  fi

  unset OPENPLANET_LSP_VERSION OPENPLANET_LSP_UPDATE_PACKAGE OPENPLANET_LSP_PACKAGE_MANAGER
  unset OPENPLANET_LSP_CONFIG_DIR OPENPLANET_LSP_EXE
  rm -rf "${cfg}"

  if [[ "${SKIP_UNINSTALL}" != "1" ]]; then
    log "${pm}: uninstall"
    uninstall_pm "${pm}"
  fi
  log "OK: ${pm}"
}

main() {
  need_cmd npm
  need_cmd curl
  need_cmd cargo

  log "FROM_VERSION=${FROM_VERSION}"
  log "TARGET_VERSION=${TARGET_VERSION}"
  log "PACKAGE_MANAGERS=${PACKAGE_MANAGERS}"
  log "NPM_GLOBAL_PREFIX=${NPM_GLOBAL_PREFIX}"
  log "host=$(uname -s)-$(uname -m)"

  if [[ -n "${WAIT_FOR_VERSION}" && "${WAIT_FOR_VERSION}" != "latest" ]]; then
    wait_for_registry "${WAIT_FOR_VERSION}"
  elif [[ "${FROM_VERSION}" != "latest" ]]; then
    wait_for_registry "${FROM_VERSION}"
  elif [[ "${TARGET_VERSION}" != "latest" ]]; then
    wait_for_registry "${TARGET_VERSION}"
  fi

  # Ensure local updater exists up front (used when installed build lacks overrides).
  ensure_local_updater >/dev/null

  local pm
  local failed=0
  for pm in ${PACKAGE_MANAGERS}; do
    if ! smoke_one "${pm}"; then
      echo "FAIL: ${pm}" >&2
      failed=1
      # Best-effort cleanup so later PMs are not polluted.
      uninstall_pm "${pm}" >/dev/null 2>&1 || true
    fi
  done

  if [[ "${failed}" -ne 0 ]]; then
    die "one or more package managers failed"
  fi
  log "OK: registry self-update smoke for all requested PMs"
}

main "$@"
