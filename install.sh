#!/usr/bin/env bash
#
# Install the latest GeWu release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/regulatory-genomics/GeWu/main/install.sh | bash
#
# Environment variables:
#   GEWU_VERSION      release tag to install (default: the latest release)
#   GEWU_INSTALL_DIR  destination directory (default: $HOME/.local/bin)
#   GEWU_REPO         GitHub repository to fetch from (default: regulatory-genomics/GeWu)

set -euo pipefail

repo="${GEWU_REPO:-regulatory-genomics/GeWu}"
install_dir="${GEWU_INSTALL_DIR:-${HOME:-}/.local/bin}"

say() { printf '%s\n' "$*"; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

need curl
need tar
need uname

if [ -z "${install_dir}" ] || [ "${install_dir}" = "/.local/bin" ]; then
  die "HOME is not set; set GEWU_INSTALL_DIR to choose an install directory"
fi

os="$(uname -s)"
arch="$(uname -m)"

case "${os}" in
  Linux)
    if [ "${arch}" != "x86_64" ]; then
      die "no prebuilt binary for Linux/${arch} (only x86_64); build from source with 'cargo install --path gewu'"
    fi
    target="x86_64-unknown-linux-gnu"
    ;;
  Darwin)
    if [ "${arch}" != "arm64" ]; then
      die "no prebuilt binary for macOS/${arch} (only arm64); build from source with 'cargo install --path gewu'"
    fi
    target="aarch64-apple-darwin"
    ;;
  *)
    die "unsupported operating system: ${os}; build from source with 'cargo install --path gewu'"
    ;;
esac

if [ "${os}" = "Linux" ]; then
  libc="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
  if [ -z "${libc}" ]; then
    die "glibc not detected; the prebuilt binary requires glibc >= 2.28 (build from source otherwise)"
  fi
  libc_version="${libc#glibc }"
  if [ "$(printf '%s\n%s\n' "${libc_version}" "2.28" | sort -V | head -n 1)" != "2.28" ]; then
    die "glibc ${libc_version} is too old; the prebuilt binary requires glibc >= 2.28 (build from source otherwise)"
  fi
fi

version="${GEWU_VERSION:-}"
if [ -z "${version}" ]; then
  latest_url="$(
    curl -fsSLI -o /dev/null -w '%{url_effective}' \
      "https://github.com/${repo}/releases/latest"
  )" || die "could not resolve the latest release of ${repo}"
  case "${latest_url}" in
    */releases/tag/*) version="${latest_url##*/}" ;;
    *) die "could not determine the latest release tag (is a release published?)" ;;
  esac
fi
case "${version}" in
  v*) ;;
  *) version="v${version}" ;;
esac

asset="gewu-${version}-${target}.tar.gz"
base_url="https://github.com/${repo}/releases/download/${version}"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

say "Downloading GeWu ${version} (${target})"
curl -fsSL --proto '=https' --tlsv1.2 -o "${tmp}/${asset}" "${base_url}/${asset}" \
  || die "download failed: ${base_url}/${asset}"
curl -fsSL --proto '=https' --tlsv1.2 -o "${tmp}/SHA256SUMS" "${base_url}/SHA256SUMS" \
  || die "download failed: ${base_url}/SHA256SUMS"

expected="$(awk -v name="${asset}" '$2 == name { print $1; exit }' "${tmp}/SHA256SUMS")"
if [ -z "${expected}" ]; then
  die "no checksum for ${asset} in SHA256SUMS"
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${tmp}/${asset}" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "${tmp}/${asset}" | awk '{ print $1 }')"
fi
if [ "${actual}" != "${expected}" ]; then
  die "checksum mismatch for ${asset}: expected ${expected}, got ${actual}"
fi

mkdir -p "${tmp}/extract"
tar -xzf "${tmp}/${asset}" -C "${tmp}/extract" gewu || die "could not extract ${asset}"
if [ ! -f "${tmp}/extract/gewu" ]; then
  die "archive ${asset} does not contain the gewu binary"
fi

mkdir -p "${install_dir}"
if command -v install >/dev/null 2>&1; then
  install -m 0755 "${tmp}/extract/gewu" "${install_dir}/gewu"
else
  cp "${tmp}/extract/gewu" "${install_dir}/gewu"
  chmod 0755 "${install_dir}/gewu"
fi

installed="$("${install_dir}/gewu" --version)"
say "Installed ${installed} to ${install_dir}/gewu"

case ":${PATH:-}:" in
  *":${install_dir}:"*) ;;
  *)
    say ""
    say "${install_dir} is not on your PATH. Add it with:"
    say "  export PATH=\"${install_dir}:\$PATH\""
    ;;
esac
