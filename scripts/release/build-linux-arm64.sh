#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-${REPO_ROOT}/dist/linux-arm64}"
BUILD_UPSTREAM_WEBUI="${BUILD_UPSTREAM_WEBUI:-1}"
SYNC_RACE_ADMIN_ASSETS="${SYNC_RACE_ADMIN_ASSETS:-1}"

log() {
  printf '[build-linux-arm64] %s\n' "$*"
}

fail() {
  printf '[build-linux-arm64] ERROR: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

sync_race_admin_assets() {
  local src_dir dst_dir
  src_dir="${REPO_ROOT}/race-gateway/webui/src"
  dst_dir="${REPO_ROOT}/race-gateway/src/web/assets"

  [[ -d "${src_dir}" ]] || fail "missing race-gateway webui source directory: ${src_dir}"
  mkdir -p "${dst_dir}"

  cp "${src_dir}/admin.html" "${dst_dir}/admin.html"
  cp "${src_dir}/admin.css" "${dst_dir}/admin.css"
  cp "${src_dir}/admin.js" "${dst_dir}/admin.js"

  log "synced race-gateway admin assets"
}

build_upstream_webui() {
  local webui_dir
  webui_dir="${REPO_ROOT}/upstream-gateway/webui"

  [[ -d "${webui_dir}" ]] || fail "missing upstream-gateway webui directory: ${webui_dir}"
  require_cmd npm

  pushd "${webui_dir}" >/dev/null
  if [[ -f package-lock.json ]]; then
    npm ci
  else
    npm install
  fi
  npm run build
  popd >/dev/null

  log "built upstream-gateway webui"
}

build_rust_binary() {
  local workdir="$1"
  local manifest_label="$2"
  shift 2

  pushd "${workdir}" >/dev/null
  cargo build --release "$@"
  popd >/dev/null

  log "built ${manifest_label}"
}

copy_artifacts() {
  local out_bin out_docs
  out_bin="${OUTPUT_DIR}/bin"
  out_docs="${OUTPUT_DIR}/docs"

  mkdir -p "${out_bin}" "${out_docs}"

  cp "${REPO_ROOT}/target/release/nyro-server" "${out_bin}/nyro-server"
  cp "${REPO_ROOT}/upstream-gateway/target/release/upstream-gateway" "${out_bin}/upstream-gateway"
  cp "${REPO_ROOT}/race-gateway/target/release/race-gateway" "${out_bin}/race-gateway"
  cp "${REPO_ROOT}/docs/server/nyro-upstream-gateway-startup.md" "${out_docs}/nyro-upstream-gateway-startup.md"
  cp "${REPO_ROOT}/docs/server/gateway-admin-manual.md" "${out_docs}/gateway-admin-manual.md"

  if command -v sha256sum >/dev/null 2>&1; then
    (
      cd "${out_bin}"
      sha256sum nyro-server upstream-gateway race-gateway > SHA256SUMS
    )
  fi

  log "copied artifacts into ${OUTPUT_DIR}"
}

main() {
  require_cmd cargo
  require_cmd cmake

  if [[ "$(uname -s)" != "Linux" ]]; then
    fail "this script is intended to run on a Linux host"
  fi

  case "$(uname -m)" in
    aarch64|arm64)
      ;;
    *)
      log "warning: host architecture is $(uname -m), not ARM64"
      ;;
  esac

  if [[ "${BUILD_UPSTREAM_WEBUI}" == "1" ]]; then
    build_upstream_webui
  else
    log "skipping upstream-gateway webui build because BUILD_UPSTREAM_WEBUI=${BUILD_UPSTREAM_WEBUI}"
  fi

  if [[ "${SYNC_RACE_ADMIN_ASSETS}" == "1" ]]; then
    sync_race_admin_assets
  else
    log "skipping race-gateway admin asset sync because SYNC_RACE_ADMIN_ASSETS=${SYNC_RACE_ADMIN_ASSETS}"
  fi

  build_rust_binary "${REPO_ROOT}" "nyro-server" -p nyro-server
  build_rust_binary "${REPO_ROOT}/upstream-gateway" "upstream-gateway"
  build_rust_binary "${REPO_ROOT}/race-gateway" "race-gateway"

  copy_artifacts

  cat <<EOF

Build completed successfully.

Artifacts:
  ${OUTPUT_DIR}/bin/nyro-server
  ${OUTPUT_DIR}/bin/upstream-gateway
  ${OUTPUT_DIR}/bin/race-gateway

Checksums:
  ${OUTPUT_DIR}/bin/SHA256SUMS

Docs:
  ${OUTPUT_DIR}/docs/nyro-upstream-gateway-startup.md
  ${OUTPUT_DIR}/docs/gateway-admin-manual.md

EOF
}

main "$@"
