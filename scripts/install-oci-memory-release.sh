#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_DIR="$ROOT_DIR/crates/gewe-skill-memory/deploy"

REPO="${GEWE_SKILL_RELEASE_REPO:-Wangnov/gewe-skill}"
VERSION="${GEWE_SKILL_VERSION:-latest}"
INSTALL_DIR="${GEWE_SKILL_INSTALL_DIR:-/opt/gewe-skill-memory}"
BIN_DIR="$INSTALL_DIR/bin"
CONFIG_DIR="$INSTALL_DIR/config"
DATA_DIR="$INSTALL_DIR/data"
ENV_FILE="$CONFIG_DIR/gewe-skill-memory.env"
SYSTEMD_DIR="${GEWE_SKILL_SYSTEMD_DIR:-/etc/systemd/system}"
SERVICE_USER="${GEWE_SKILL_SERVICE_USER:-gewe-skill}"
SERVICE_GROUP="${GEWE_SKILL_SERVICE_GROUP:-gewe-skill}"

usage() {
  cat <<'EOF'
Usage:
  sudo GEWE_SKILL_VERSION=v0.1.18 scripts/install-oci-memory-release.sh

Environment:
  GEWE_SKILL_VERSION        Release tag such as v0.1.18, or latest. Default: latest
  GEWE_SKILL_RELEASE_REPO   GitHub repo owner/name. Default: Wangnov/gewe-skill
  GEWE_SKILL_TARGET         Release target. Default: auto glibc target for host arch
  GEWE_SKILL_INSTALL_DIR    Install root. Default: /opt/gewe-skill-memory
  GEWE_SKILL_SYSTEMD_DIR    systemd unit dir. Default: /etc/systemd/system
  GEWE_SKILL_SERVICE_USER   Service user. Default: gewe-skill
  GEWE_SKILL_SERVICE_GROUP  Service group. Default: gewe-skill
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "required command not found: $1" >&2
    exit 1
  fi
}

if [[ "$(id -u)" -ne 0 ]]; then
  echo "this installer must run as root because it writes $INSTALL_DIR and $SYSTEMD_DIR" >&2
  exit 1
fi

require_command curl
require_command sha256sum
require_command tar
require_command find
require_command systemctl

if [[ ! -d "$DEPLOY_DIR" ]]; then
  echo "deploy directory not found: $DEPLOY_DIR" >&2
  echo "run this script from a gewe-skill checkout or source release archive" >&2
  exit 1
fi

auto_target() {
  case "$(uname -m)" in
    aarch64 | arm64)
      echo "aarch64-unknown-linux-gnu"
      ;;
    x86_64 | amd64)
      echo "x86_64-unknown-linux-gnu"
      ;;
    *)
      echo "unsupported architecture: $(uname -m)" >&2
      echo "set GEWE_SKILL_TARGET explicitly" >&2
      exit 1
      ;;
  esac
}

TARGET="${GEWE_SKILL_TARGET:-$(auto_target)}"
if [[ "$VERSION" == "latest" ]]; then
  BASE_URL="https://github.com/$REPO/releases/latest/download"
else
  BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
fi

if ! getent group "$SERVICE_GROUP" >/dev/null 2>&1; then
  groupadd --system "$SERVICE_GROUP"
fi
if ! id "$SERVICE_USER" >/dev/null 2>&1; then
  useradd --system --gid "$SERVICE_GROUP" --home-dir "$INSTALL_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"
fi

install -d -o "$SERVICE_USER" -g "$SERVICE_GROUP" -m 0750 "$INSTALL_DIR" "$BIN_DIR" "$CONFIG_DIR" "$DATA_DIR"
if [[ ! -f "$ENV_FILE" ]]; then
  install -o root -g "$SERVICE_GROUP" -m 0640 "$DEPLOY_DIR/gewe-skill-memory.env.example" "$ENV_FILE"
  echo "created example env file: $ENV_FILE" >&2
  echo "edit secrets in $ENV_FILE before starting production traffic" >&2
fi

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

download_asset() {
  local asset="$1"
  curl -fsSLo "$tmpdir/$asset" "$BASE_URL/$asset"
}

cli_archive="gewe-skill-cli-$TARGET.tar.xz"
memory_archive="gewe-skill-memory-$TARGET.tar.xz"

download_asset "$cli_archive"
download_asset "$cli_archive.sha256"
download_asset "$memory_archive"
download_asset "$memory_archive.sha256"

(cd "$tmpdir" && sha256sum -c "$cli_archive.sha256" && sha256sum -c "$memory_archive.sha256")

tar -xJf "$tmpdir/$cli_archive" -C "$tmpdir"
tar -xJf "$tmpdir/$memory_archive" -C "$tmpdir"

cli_bin="$(find "$tmpdir" -type f -name gewe-skill -perm -u+x | head -n 1)"
memory_bin="$(find "$tmpdir" -type f -name gewe-skill-memory -perm -u+x | head -n 1)"
if [[ -z "$cli_bin" || -z "$memory_bin" ]]; then
  echo "release archives did not contain expected binaries" >&2
  exit 1
fi

install -o root -g root -m 0755 "$cli_bin" "$BIN_DIR/gewe-skill"
install -o root -g root -m 0755 "$memory_bin" "$BIN_DIR/gewe-skill-memory"

unit_files=(
  gewe-skill-memory.service
  gewe-skill-edge-sync.service
  gewe-skill-edge-sync.timer
  gewe-skill-edge-attachment-sync.service
  gewe-skill-edge-attachment-sync.timer
  gewe-skill-edge-chatroom-event-sync.service
  gewe-skill-edge-chatroom-event-sync.timer
  gewe-skill-identity-refresh.service
  gewe-skill-identity-refresh.timer
  gewe-skill-identity-event-backfill.service
  gewe-skill-identity-event-backfill.timer
)

for unit in "${unit_files[@]}"; do
  install -o root -g root -m 0644 "$DEPLOY_DIR/$unit" "$SYSTEMD_DIR/$unit"
done

chown -R "$SERVICE_USER:$SERVICE_GROUP" "$DATA_DIR"
systemctl daemon-reload
systemctl enable --now gewe-skill-memory.service
systemctl restart gewe-skill-memory.service

for timer in \
  gewe-skill-edge-sync.timer \
  gewe-skill-edge-attachment-sync.timer \
  gewe-skill-edge-chatroom-event-sync.timer \
  gewe-skill-identity-refresh.timer \
  gewe-skill-identity-event-backfill.timer; do
  systemctl enable --now "$timer"
done

for _ in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsS http://127.0.0.1:8788/healthz >/dev/null; then
    break
  fi
  sleep 1
done

"$BIN_DIR/gewe-skill" --version
curl -fsS http://127.0.0.1:8788/healthz
systemctl --no-pager --plain list-timers \
  gewe-skill-edge-sync.timer \
  gewe-skill-edge-attachment-sync.timer \
  gewe-skill-edge-chatroom-event-sync.timer \
  gewe-skill-identity-refresh.timer \
  gewe-skill-identity-event-backfill.timer
