#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"

default_base_image() {
  # Prefer docker daemon registry mirror if present, so buildkit doesn't touch docker.io directly.
  if [[ -z "${DEB_BASE_IMAGE:-}" ]] && [[ -r /etc/docker/daemon.json ]]; then
    local mirror
    mirror="$(python3 - <<'PY' 2>/dev/null || true
import json
try:
  with open("/etc/docker/daemon.json","r",encoding="utf-8") as f:
    data=json.load(f)
  mirrors=data.get("registry-mirrors") or []
  if mirrors:
    m=str(mirrors[0]).strip()
    m=m.removeprefix("https://").removeprefix("http://").rstrip("/")
    if m:
      print(m)
except Exception:
  pass
PY
)"
    if [[ -n "${mirror}" ]]; then
      echo "${mirror}/library/rust:1-bookworm"
      return
    fi
  fi
  echo "rust:1-bookworm"
}

BASE_IMAGE="${DEB_BASE_IMAGE:-$(default_base_image)}"

mkdir -p "${DIST_DIR}"

PKG="${1:-usbip-server}"

need_cmd() {
  local name="$1"
  local hint="${2:-}"
  if ! command -v "${name}" >/dev/null 2>&1; then
    echo "${name} not found"
    if [[ -n "${hint}" ]]; then
      echo "hint: ${hint}"
    fi
    exit 1
  fi
}

FORCE="${FORCE:-0}"
STRICT="${STRICT:-0}"
DEB_ARCHES="${DEB_ARCHES:-amd64,arm64}"

pkg_version() {
  local pkg="$1"
  (cd "${ROOT_DIR}" && PKG="${pkg}" python3 - <<'PY'
import json,subprocess,os
pkg=os.environ.get("PKG") or ""
try:
  out=subprocess.check_output(["cargo","metadata","--no-deps","--format-version","1"], text=True)
  data=json.loads(out)
  for p in data.get("packages",[]):
    if p.get("name")==pkg:
      print(p.get("version","0.0.0"))
      raise SystemExit(0)
except Exception:
  pass
print("0.0.0")
PY
  )
}

VERSION="$(pkg_version "${PKG}")"

need_cmd cargo "install rust toolchain first"

if ! command -v cargo-deb >/dev/null 2>&1; then
  echo "cargo-deb not found; installing..."
  cargo install cargo-deb --locked
fi

build_amd64() {
  local pkg="$1"
  local version
  version="$(pkg_version "${pkg}")"
  local out="${DIST_DIR}/${pkg}_${version}_amd64.deb"
  if [[ "${FORCE}" != "1" ]] && [[ -f "${out}" ]]; then
    echo "${out}"
    return
  fi

  (cd "${ROOT_DIR}" && cargo build -p "${pkg}" --release)
  (cd "${ROOT_DIR}" && cargo deb -p "${pkg}" --no-build -o "${DIST_DIR}")

  # Pick newest generated deb and copy to stable name (avoid mv same-file errors).
  local deb
  deb="$(ls -1t "${DIST_DIR}"/"${pkg}"_*_amd64.deb 2>/dev/null | head -n1 || true)"
  if [[ -z "${deb}" ]]; then
    echo "amd64 deb not found in ${DIST_DIR} for ${pkg}"
    exit 1
  fi
  cp -f "${deb}" "${out}"
  echo "${out}"
}

build_arm64() {
  local pkg="$1"
  local version
  version="$(pkg_version "${pkg}")"
  local out="${DIST_DIR}/${pkg}_${version}_arm64.deb"
  if [[ "${FORCE}" != "1" ]] && [[ -f "${out}" ]]; then
    echo "${out}"
    return
  fi

  need_cmd zig "sudo apt-get install zig"
  if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "cargo-zigbuild not found; installing..."
    cargo install cargo-zigbuild --locked
  fi

  local target="aarch64-unknown-linux-gnu"
  if [[ "${pkg}" == "usbip-server" ]]; then
    # Cross deps for libudev-sys (pkg-config):
    # Debian example:
    #   sudo dpkg --add-architecture arm64
    #   sudo apt-get update
    #   sudo apt-get install -y pkg-config-aarch64-linux-gnu libudev-dev:arm64
    if ! command -v aarch64-linux-gnu-pkg-config >/dev/null 2>&1; then
      echo "aarch64-linux-gnu-pkg-config not found; arm64 cross build needs it."
      echo "Debian: sudo dpkg --add-architecture arm64 && sudo apt-get update && sudo apt-get install -y pkg-config-aarch64-linux-gnu libudev-dev:arm64"
      return 2
    fi
    if [[ ! -e /usr/lib/aarch64-linux-gnu/libudev.so ]]; then
      echo "/usr/lib/aarch64-linux-gnu/libudev.so not found; arm64 cross build needs arm64 libudev."
      echo "Debian: sudo dpkg --add-architecture arm64 && sudo apt-get update && sudo apt-get install -y libudev-dev:arm64"
      return 2
    fi

    (cd "${ROOT_DIR}" && \
      PKG_CONFIG_ALLOW_CROSS=1 \
      PKG_CONFIG=aarch64-linux-gnu-pkg-config \
      PKG_CONFIG_SYSROOT_DIR=/ \
      PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
      PKG_CONFIG_PATH= \
      cargo zigbuild -p "${pkg}" --release --target "${target}")
  else
    (cd "${ROOT_DIR}" && cargo zigbuild -p "${pkg}" --release --target "${target}")
  fi
  (cd "${ROOT_DIR}" && cargo deb -p "${pkg}" --target "${target}" --no-build -o "${DIST_DIR}")

  local deb
  # cargo-deb names the package by Debian arch (arm64), not by Rust target triple.
  deb="$(ls -1t "${DIST_DIR}"/"${pkg}"_*_arm64.deb 2>/dev/null | head -n1 || true)"
  if [[ -z "${deb}" ]]; then
    echo "arm64 deb not found in ${DIST_DIR} for ${pkg}"
    exit 1
  fi
  cp -f "${deb}" "${out}"
  echo "${out}"
}

want_arch() {
  local a="$1"
  [[ ",${DEB_ARCHES}," == *",${a},"* ]]
}

if want_arch "amd64"; then
  build_amd64 "${PKG}"
fi

if want_arch "arm64"; then
  if ! build_arm64 "${PKG}"; then
    rc=$?
    if [[ "${STRICT}" == "1" ]]; then
      exit "${rc}"
    fi
    echo "arm64 deb skipped (missing cross deps). To fail hard: STRICT=1 make deb"
  fi
fi

echo "Debs written to: ${DIST_DIR}"

