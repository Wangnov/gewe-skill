#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_DIR="$ROOT_DIR/skills/gewe-skill"

if [[ ! -f "$SOURCE_DIR/SKILL.md" ]]; then
  echo "SKILL.md not found: $SOURCE_DIR" >&2
  exit 1
fi

if [[ $# -gt 1 ]]; then
  echo "usage: $0 [destination-directory]" >&2
  exit 2
fi

if [[ $# -eq 1 ]]; then
  DEST_DIR="$1"
else
  SKILLS_DIR="${GEWE_SKILL_SKILLS_DIR:-$HOME/.gewe-skill/skills}"
  DEST_DIR="$SKILLS_DIR/gewe-skill"
fi

mkdir -p "$DEST_DIR"
cp "$SOURCE_DIR/SKILL.md" "$DEST_DIR/SKILL.md"
cp "$SOURCE_DIR/README.md" "$DEST_DIR/README.md"

cat <<EOF
Installed gewe-skill Markdown skill:
  $DEST_DIR

Set GEWE_SKILL_BASE_URL and GEWE_SKILL_READ_TOKEN in your Agent runtime before use.
EOF
