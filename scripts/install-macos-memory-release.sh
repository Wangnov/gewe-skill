#!/bin/bash
set +x
set -euo pipefail
umask 077
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_DIR="$ROOT_DIR/crates/gewe-skill-memory/deploy/macos"
VERSION="${GEWE_SKILL_VERSION:-v0.1.48}"
REPO="${GEWE_SKILL_RELEASE_REPO:-Wangnov/gewe-skill}"
PREFIX="${GEWE_SKILL_INSTALL_DIR:-$HOME/srv/gewe-skill-memory}"
RADAR_PREFIX="${GEWE_SKILL_RADAR_DIR:-$HOME/srv/wechat-radar}"
DATA_DIR="${GEWE_SKILL_DATA_DIR:-/Volumes/DATA/gewe.noindex}"
VOLUME="${GEWE_SKILL_VOLUME:-/Volumes/DATA}"
LOG_DIR="${GEWE_SKILL_LOG_DIR:-$HOME/Library/Logs/gewe-skill}"
PYTHON="${GEWE_SKILL_PYTHON:-/opt/homebrew/bin/python3}"
activate=false
case "${1:-}" in
  --activate) activate=true ;;
  --help|-h) echo '用法：GEWE_SKILL_VERSION=v0.1.48 install-macos-memory-release.sh [--activate]；默认仅安装并暂存。'; exit 0 ;;
  '') ;;
  *) exit 64 ;;
esac
[[ $# -le 1 && "$(uname -s)" == Darwin && "$(uname -m)" == arm64 && "$(id -u)" -ne 0 ]] || exit 64
[[ -d "$DEPLOY_DIR" && -x "$PYTHON" ]] || exit 69
for directory in "$PREFIX" "$PREFIX/bin" "$PREFIX/config" "$PREFIX/launchd" "$LOG_DIR"; do
  /usr/bin/install -d -m 0700 "$directory"
done
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
base="https://github.com/$REPO/releases/download/$VERSION"
for package in cli memory; do
  asset="gewe-skill-$package-aarch64-apple-darwin.tar.xz"
  /usr/bin/curl --fail --silent --show-error --location --connect-timeout 20 --max-time 600 --retry 3 \
    "$base/$asset" -o "$tmpdir/$asset"
  /usr/bin/curl --fail --silent --show-error --location --connect-timeout 20 --max-time 120 --retry 3 \
    "$base/$asset.sha256" -o "$tmpdir/$asset.sha256"
  (cd "$tmpdir" && /usr/bin/shasum -a 256 -c "$asset.sha256")
  /bin/mkdir "$tmpdir/$package"
  /usr/bin/tar -xJf "$tmpdir/$asset" -C "$tmpdir/$package"
done
cli_bin="$(/usr/bin/find "$tmpdir/cli" -type f -name gewe-skill | /usr/bin/head -n 1)"
memory_bin="$(/usr/bin/find "$tmpdir/memory" -type f -name gewe-skill-memory | /usr/bin/head -n 1)"
[[ -n "$cli_bin" && -n "$memory_bin" ]] || exit 65
for binary in "$cli_bin" "$memory_bin"; do
  /usr/bin/file "$binary" | /usr/bin/grep -q 'arm64' || exit 65
  /usr/bin/install -m 0755 "$binary" "$PREFIX/bin/$(basename "$binary")"
  # Remove quarantine only from binaries in checksum-verified archives.
  /usr/bin/xattr -d com.apple.quarantine "$PREFIX/bin/$(basename "$binary")" 2>/dev/null || true
done
"$PYTHON" - "$DEPLOY_DIR" "$PREFIX" "$DATA_DIR" "$LOG_DIR" "$PYTHON" "$HOME" "$RADAR_PREFIX" "$VOLUME" <<'PY'
import os, pathlib, plistlib, sys
source, prefix, data, log, python, home, radar, volume = sys.argv[1:]
values = {'__PREFIX__': prefix, '__DATA_DIR__': data, '__LOG_DIR__': log,
          '__PYTHON__': python, '__HOME__': home, '__RADAR_PREFIX__': radar, '__VOLUME__': volume}
def render(text):
    for token, value in values.items():
        text = text.replace(token, value)
    return text
wrapper = pathlib.Path(prefix) / 'bin/gewe-run'
wrapper.write_text(render((pathlib.Path(source) / 'gewe-run').read_text()))
wrapper.chmod(0o755)
def replace(value):
    if isinstance(value, str):
        return render(value)
    if isinstance(value, list):
        return [replace(item) for item in value]
    if isinstance(value, dict):
        return {key: replace(item) for key, item in value.items()}
    return value
for template in sorted(pathlib.Path(source).glob('*.plist')):
    target = pathlib.Path(prefix) / 'launchd' / template.name
    target.write_bytes(plistlib.dumps(replace(plistlib.loads(template.read_bytes()))))
    target.chmod(0o600)
PY
/bin/bash -n "$PREFIX/bin/gewe-run"
for plist in "$PREFIX/launchd/"*.plist; do /usr/bin/plutil -lint "$plist"; done
"$PREFIX/bin/gewe-skill" --version
"$PREFIX/bin/gewe-skill-memory" --version
if "$activate"; then
  [[ -r "$PREFIX/config/gewe-skill-memory.env" && -r "$RADAR_PREFIX/config/wechat-radar.env" ]] || exit 78
  /usr/bin/install -d -m 0700 "$HOME/Library/LaunchAgents"
  for plist in "$PREFIX/launchd/"*.plist; do
    /usr/bin/install -m 0600 "$plist" "$HOME/Library/LaunchAgents/$(basename "$plist")"
    /bin/launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$(basename "$plist")"
  done
else
  echo '安装完成：9 个 agent 已暂存，尚未激活；既有 env 保留。'
fi
