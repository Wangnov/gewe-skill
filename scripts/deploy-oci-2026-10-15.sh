#!/usr/bin/env bash
# Only execute --deploy after the owner releases this reviewed workspace for production.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# 连接信息放本机配置，不进仓库：GEWE_SSH_TARGET（user@host）与 GEWE_SSH_KEY（私钥路径）。
gewe_config="${GEWE_CONFIG:-$HOME/.config/gewe/env}"
if [[ -f "$gewe_config" ]]; then set -a; . "$gewe_config"; set +a; fi
target="${GEWE_SSH_TARGET:-}"
key="${GEWE_SSH_KEY:-$HOME/.ssh/id_ed25519}"
[[ -n "$target" ]] || { printf 'gewe: 未配置 GEWE_SSH_TARGET，请写入 %s 或设环境变量。\n' "$gewe_config" >&2; exit 2; }
bin_dir="${GEWE_DEPLOY_REMOTE_BIN_DIR:-/opt/gewe-skill-memory/bin}"
env_file="${GEWE_REMOTE_ENV:-/opt/gewe-skill-memory/config/gewe-skill-memory.env}"
service="gewe-skill-memory.service"
health_url="${GEWE_DEPLOY_HEALTH_URL:-http://127.0.0.1:8788/healthz}"
build_target_dir="${CARGO_TARGET_DIR:-$repo_dir/target}"
[[ "$build_target_dir" == /* ]] || build_target_dir="$repo_dir/$build_target_dir"
artifact_dir="${GEWE_DEPLOY_ARTIFACT_DIR:-$build_target_dir/aarch64-unknown-linux-gnu/release}"

usage() {
  cat <<'EOF'
用法（本脚本不会自行取得上线授权）：
  scripts/deploy-oci-2026-10-15.sh --build-only      # 仅本机交叉编译
  scripts/deploy-oci-2026-10-15.sh --deploy          # 本机编译、上传、备份、替换、重启、验收；失败回滚
  scripts/deploy-oci-2026-10-15.sh --mime-dry-run    # 独立的生产 MIME 回填预览，不改记录
  scripts/deploy-oci-2026-10-15.sh --mime-apply      # 独立回填；先 dry-run，再 apply

构建要求：Rust stable、aarch64-unknown-linux-gnu target、cargo-zigbuild、Zig。
默认 GNU glibc 下限 2.28，可设 GEWE_DEPLOY_GLIBC；SQLx 的 SQLite C 代码由 Zig 构建。
GEWE_DEPLOY_PREBUILT=1 可使用 GEWE_DEPLOY_ARTIFACT_DIR 中已审过的两份 Linux ARM64 产物。
连接配置复用 GEWE_SSH_TARGET、GEWE_SSH_KEY、GEWE_REMOTE_ENV；不复制或输出 token。
--deploy 不运行 MIME 回填，不修改 systemd/定时器，也不部署 edge。
EOF
}

mode="${1:-}"
case "$mode" in --build-only|--deploy|--mime-dry-run|--mime-apply) ;;
  -h|--help|"") usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac
[[ $# -eq 1 ]] || { usage >&2; exit 2; }
[[ "$target" != -* ]] || { printf '无效的 SSH 目标。\n' >&2; exit 2; }

quote() {
  local rest="$1" encoded=""
  while [[ "$rest" == *"'"* ]]; do
    encoded+="${rest%%\'*}'\"'\"'"
    rest="${rest#*\'}"
  done
  REPLY="'$encoded$rest'"
}
require_command() { command -v "$1" >/dev/null || { printf '缺少本机工具：%s\n' "$1" >&2; exit 2; }; }

if [[ "$mode" == --build-only || "$mode" == --deploy ]]; then
  if [[ "${GEWE_DEPLOY_PREBUILT:-0}" != 1 ]]; then
    require_command rg
    require_command cargo
    require_command rustup
    require_command zig
    require_command cargo-zigbuild
    rustup target list --installed | rg -qx aarch64-unknown-linux-gnu || {
      printf '请先运行 rustup target add aarch64-unknown-linux-gnu。\n' >&2; exit 2;
    }
    (cd "$repo_dir" && cargo zigbuild --locked --release \
      --target "aarch64-unknown-linux-gnu.${GEWE_DEPLOY_GLIBC:-2.28}" \
      -p gewe-skill-cli -p gewe-skill-memory --bin gewe-skill --bin gewe-skill-memory)
  fi
  require_command file
  require_command rg
  for name in gewe-skill gewe-skill-memory; do
    [[ -x "$artifact_dir/$name" ]] || { printf '未找到产物：%s\n' "$artifact_dir/$name" >&2; exit 2; }
    file "$artifact_dir/$name" | rg -q 'ELF 64-bit.*ARM aarch64' || { printf '产物不是 Linux ARM64：%s\n' "$name" >&2; exit 2; }
  done
  if [[ "$mode" == --build-only ]]; then
    printf '本机产物：%s/gewe-skill、gewe-skill-memory\n' "$artifact_dir"
    exit 0
  fi
fi

require_command ssh
transport=(-o BatchMode=yes -o ConnectTimeout=8 -o ServerAliveInterval=15 -o ServerAliveCountMax=2 -i "$key")

if [[ "$mode" == --mime-dry-run || "$mode" == --mime-apply ]]; then
  quote "$env_file"; remote="set -a; . $REPLY; set +a; "
  quote "$bin_dir/gewe-skill"; remote+="$REPLY --json maintenance attachment-mime-backfill --dry-run"
  if [[ "$mode" == --mime-apply ]]; then
    quote "$bin_dir/gewe-skill"; remote+=" && $REPLY --json maintenance attachment-mime-backfill --apply"
  fi
  quote "$remote"
  ssh -T "${transport[@]}" "$target" "sudo -n bash -c $REPLY"
  exit 0
fi

require_command scp
require_command shasum
scratch="$(mktemp -d "${TMPDIR:-/tmp}/gewe-build.XXXXXXXX")"
stage=""
cleanup() {
  rm -rf "$scratch"
  if [[ -n "$stage" ]]; then
    quote "$stage"
    ssh -T "${transport[@]}" "$target" "rm -rf -- $REPLY" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Everything below is a production action. The owner must authorize execution first.
stage="$(ssh -T "${transport[@]}" "$target" 'mktemp -d /tmp/gewe-deploy-20261015.XXXXXXXX')"
[[ "$stage" == /tmp/gewe-deploy-20261015.* && "$stage" != *$'\n'* ]] || { printf '远端暂存目录无效。\n' >&2; stage=""; exit 1; }
(cd "$artifact_dir" && shasum -a 256 gewe-skill gewe-skill-memory) >"$scratch/checksums.sha256"
scp "${transport[@]}" "$artifact_dir/gewe-skill" "$artifact_dir/gewe-skill-memory" "$scratch/checksums.sha256" "$target:$stage/"

remote="sudo -n bash -s --"
for value in "$stage" "$bin_dir" "$env_file" "$service" "$health_url"; do quote "$value"; remote+=" $REPLY"; done
ssh -T "${transport[@]}" "$target" "$remote" <<'REMOTE'
set -Eeuo pipefail
stage="$1"
bin_dir="$2"
env_file="$3"
service="$4"
health_url="$5"
[[ "$(uname -m)" == aarch64 ]] || { printf '远端架构不是 aarch64。\n' >&2; exit 2; }
for tool in install cp mv sha256sum systemctl curl timeout; do command -v "$tool" >/dev/null; done
[[ -r "$env_file" && -x "$bin_dir/gewe-skill" && -x "$bin_dir/gewe-skill-memory" ]]
systemctl is-active --quiet "$service"
(cd "$stage" && sha256sum -c checksums.sha256)

stamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
backup="$bin_dir/backups/$stamp"
install -d -m 0755 "$backup"
cp -a "$bin_dir/gewe-skill" "$bin_dir/gewe-skill-memory" "$backup/"
(cd "$backup" && sha256sum gewe-skill gewe-skill-memory) >"$backup/checksums.sha256"
new_cli="$bin_dir/.gewe-skill-$stamp"
new_memory="$bin_dir/.gewe-skill-memory-$stamp"
mutated=0

probe() {
  for attempt in {1..20}; do
    if systemctl is-active --quiet "$service" && curl --max-time 3 -fsS "$health_url" >/dev/null; then
      (
        set -a
        . "$env_file"
        set +a
        "$bin_dir/gewe-skill" --base-url "${health_url%/healthz}" --json messages list --limit 1 >/dev/null
      ) && return 0
    fi
    sleep 1
  done
  return 1
}

rollback() {
  local status="$1"
  trap - ERR INT TERM HUP
  set +e
  if ((mutated)); then
    printf '上线失败，恢复备份：%s\n' "$backup" >&2
    install -o root -g root -m 0755 "$backup/gewe-skill" "$new_cli" && mv -f "$new_cli" "$bin_dir/gewe-skill"
    install -o root -g root -m 0755 "$backup/gewe-skill-memory" "$new_memory" && mv -f "$new_memory" "$bin_dir/gewe-skill-memory"
    if (cd "$bin_dir" && sha256sum -c "$backup/checksums.sha256" --status) && timeout 45s systemctl restart "$service" && probe; then
      printf '已回滚，原服务健康检查通过。\n' >&2
    else
      printf '自动回滚未能确认恢复；请人工检查 %s 和备份 %s。\n' "$service" "$backup" >&2
    fi
  fi
  rm -f "$new_cli" "$new_memory"
  exit "$status"
}
trap 'rollback $?' ERR
trap 'rollback 130' INT
trap 'rollback 143' TERM
trap 'rollback 129' HUP

install -o root -g root -m 0755 "$stage/gewe-skill" "$new_cli"
install -o root -g root -m 0755 "$stage/gewe-skill-memory" "$new_memory"
mutated=1
mv -f "$new_cli" "$bin_dir/gewe-skill"
mv -f "$new_memory" "$bin_dir/gewe-skill-memory"
if ! (cd "$bin_dir" && sha256sum -c "$stage/checksums.sha256" --status); then rollback 1; fi
timeout 45s systemctl restart "$service"
probe
trap - ERR INT TERM HUP
printf '上线验收通过；备份保留在 %s。MIME 回填尚未执行。\n' "$backup"
REMOTE
