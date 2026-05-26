#!/bin/sh
# SCR1B3 one-line installer (Linux/macOS).
#   curl -fsSL https://raw.githubusercontent.com/itasha-corp/scr1b3/main/packaging/install.sh | sh
#
# Downloads the release artifact matching this OS/arch from GitHub Releases,
# verifies its SHA-256, and installs the `scr1b3` binary to a bin dir on PATH.
# POSIX sh, shellcheck-clean. No telemetry. No data leaves your machine beyond
# the GitHub download itself.
set -eu

REPO="itasha-corp/scr1b3"
BIN="scr1b3"

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  target_os="unknown-linux-gnu" ;;
  Darwin) target_os="apple-darwin" ;;
  *) echo "unsupported OS: $os (use the Windows installer / winget)" >&2; exit 1 ;;
esac

case "$arch" in
  x86_64|amd64) target_arch="x86_64" ;;
  arm64|aarch64) target_arch="aarch64" ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac

target="${target_arch}-${target_os}"
asset="${BIN}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/latest/download"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading ${asset} ..."
curl -fsSL "${base}/${asset}" -o "${tmp}/${asset}"
curl -fsSL "${base}/${BIN}-${target}.sha256" -o "${tmp}/sum" 2>/dev/null || true

if [ -f "${tmp}/sum" ] && command -v sha256sum >/dev/null 2>&1; then
  echo "verifying checksum ..."
  expected=$(awk '{print $1}' "${tmp}/sum")
  actual=$(sha256sum "${tmp}/${asset}" | awk '{print $1}')
  if [ "$expected" != "$actual" ]; then
    echo "checksum mismatch — aborting" >&2
    exit 1
  fi
fi

tar -xzf "${tmp}/${asset}" -C "$tmp"

# Pick a writable bin dir on PATH.
if [ -w "/usr/local/bin" ]; then
  dest="/usr/local/bin"
else
  dest="${HOME}/.local/bin"
  mkdir -p "$dest"
fi

install -m 0755 "${tmp}/${BIN}" "${dest}/${BIN}"
echo "installed ${BIN} to ${dest}"
case ":${PATH}:" in
  *":${dest}:"*) ;;
  *) echo "note: add ${dest} to your PATH" ;;
esac
echo "run: ${BIN}"
