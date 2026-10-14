#!/usr/bin/env bash
# SSH-only local entry point. Credentials remain in the existing remote env file.
set -euo pipefail

# 连接信息放本机配置，不进仓库：GEWE_SSH_TARGET（user@host）与 GEWE_SSH_KEY（私钥路径）。
gewe_config="${GEWE_CONFIG:-$HOME/.config/gewe/env}"
if [[ -f "$gewe_config" ]]; then set -a; . "$gewe_config"; set +a; fi
target="${GEWE_SSH_TARGET:-}"
key="${GEWE_SSH_KEY:-$HOME/.ssh/id_ed25519}"
[[ -n "$target" ]] || { printf 'gewe: 未配置 GEWE_SSH_TARGET，请写入 %s 或设环境变量。\n' "$gewe_config" >&2; exit 2; }
remote_env="${GEWE_REMOTE_ENV:-/opt/gewe-skill-memory/config/gewe-skill-memory.env}"
remote_cli="${GEWE_REMOTE_CLI:-/usr/local/bin/gewe-skill}"
args=("$@")

# POSIX single-quote encoding, including embedded quotes and newlines. Never eval input.
quote() {
  local rest="$1" encoded=""
  while [[ "$rest" == *"'"* ]]; do
    encoded+="${rest%%\'*}'\"'\"'"
    rest="${rest#*\'}"
  done
  REPLY="'$encoded$rest'"
}

fail() { printf 'gewe: %s\n' "$*" >&2; exit 2; }
[[ "$target" != -* ]] || fail "GEWE_SSH_TARGET 不能以 - 开头。"
command -v ssh >/dev/null || fail "未找到 ssh，请安装或启用 OpenSSH。"

text_format=false
json_requested=false
for ((i=0; i<${#args[@]}; i++)); do
  if [[ "${args[i]}" == --json ]]; then json_requested=true; fi
  if [[ "${args[i]}" == --format=text ]] || { [[ "${args[i]}" == --format ]] && [[ "${args[i+1]:-}" == text ]]; }; then text_format=true; fi
done

# Find the command after the CLI's optional global arguments.
command_index=0
while ((command_index < ${#args[@]})); do
  case "${args[command_index]}" in
    --json) command_index=$((command_index + 1)) ;;
    --base-url|--read-token|--write-token) command_index=$((command_index + 2)) ;;
    --base-url=*|--read-token=*|--write-token=*) command_index=$((command_index + 1)) ;;
    *) break ;;
  esac
done
local_output=""
file_command=false
if [[ "${args[command_index]:-}" == attachments && "${args[command_index+1]:-}" == download ]] ||
   [[ "${args[command_index]:-}" == sns && "${args[command_index+1]:-}" == export ]]; then
  file_command=true
  for ((i=command_index+2; i<${#args[@]}; i++)); do
    case "${args[i]}" in
      -o|--output)
        [[ -z "$local_output" ]] || fail "只能指定一个输出路径。"
        ((i + 1 < ${#args[@]})) || fail "--output/-o 缺少路径。"
        local_output="${args[i+1]}"
        [[ -n "$local_output" ]] || fail "输出路径不能为空。"
        args[i+1]='-'
        i=$((i+1))
        ;;
      --output=*|-o?*)
        [[ -z "$local_output" ]] || fail "只能指定一个输出路径。"
        if [[ "${args[i]}" == --output=* ]]; then local_output="${args[i]#--output=}"; else local_output="${args[i]#-o}"; fi
        [[ -n "$local_output" ]] || fail "输出路径不能为空。"
        args[i]='--output=-'
        ;;
    esac
  done
fi

quote "$remote_env"; remote_script="set -a; . $REPLY; set +a; "
if [[ -n "${TZ:-}" ]]; then quote "$TZ"; remote_script+="export TZ=$REPLY; "; fi
if $file_command; then
  # 旧 CLI 会把 '-' 当服务器文件名，先只读检查能力，避免写错位置。
  quote "$remote_cli"
  remote_script+="case \$( $REPLY attachments download --help ) in *--message-key*) ;; *) printf '%s\\n' '远端 CLI 尚不支持 stdout 下载/导出，请先升级；没有写入服务器输出文件。' >&2; exit 2 ;; esac; "
fi
quote "$remote_cli"; remote_script+="exec $REPLY"
if ! $text_format && ! $json_requested; then remote_script+=' --json'; fi
for arg in "${args[@]}"; do quote "$arg"; remote_script+=" $REPLY"; done
quote "$remote_script"; remote_command="sudo -n bash -c $REPLY"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/gewe.XXXXXXXX")"
output_tmp=""
cleanup() {
  rm -rf "$scratch"
  if [[ -n "$output_tmp" ]]; then rm -f "$output_tmp"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

ssh_args=(-T -o BatchMode=yes -o ConnectTimeout=8 -o ServerAliveInterval=15 -o ServerAliveCountMax=2 -i "$key")
status=0
if $file_command && [[ -n "$local_output" && "$local_output" != - ]]; then
  # Prefix relative paths so leading hyphens cannot become options to filesystem commands.
  [[ "$local_output" == /* ]] || local_output="./$local_output"
  parent="$(dirname "$local_output")"
  mkdir -p "$parent"
  output_tmp="$(mktemp "$parent/.gewe-output.XXXXXXXX")"
  ssh "${ssh_args[@]}" "$target" "$remote_command" >"$output_tmp" 2>"$scratch/stderr" || status=$?
else
  ssh "${ssh_args[@]}" "$target" "$remote_command" 2>"$scratch/stderr" || status=$?
fi

if ((status == 255)); then
  printf 'gewe: 无法连接记忆服务器，请检查 Tailscale、SSH 密钥和 GEWE_SSH_TARGET。\n' >&2
  if [[ "${GEWE_SSH_VERBOSE:-0}" == 1 ]]; then cat "$scratch/stderr" >&2; fi
  exit "$status"
fi
cat "$scratch/stderr" >&2
if ((status != 0)); then
  printf 'gewe: 远端命令失败（退出码 %s），请查看上方具体原因。\n' "$status" >&2
  exit "$status"
fi
if [[ -n "$output_tmp" ]]; then
  mv -f "$output_tmp" "$local_output"
  output_tmp=""
  printf 'gewe: 已保存到 %s\n' "$local_output" >&2
fi
