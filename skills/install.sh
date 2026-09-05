#!/usr/bin/env bash
# Symlink the agentssh skills into Claude Code's skills directory.
#
#   ./skills/install.sh              # install/refresh (default: ~/.claude/skills)
#   ./skills/install.sh --uninstall  # remove the symlinks
#   CLAUDE_SKILLS_DIR=... ./skills/install.sh   # e.g. a project's .claude/skills
#
# Symlinks, not copies: edits in this repo take effect on the next session.
set -euo pipefail

src=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dest=${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}
skills=(ssh ssh-contexts ssh-sessions ssh-files)
uninstall=0
[ "${1:-}" = "--uninstall" ] && uninstall=1

mkdir -p "$dest"
for s in "${skills[@]}"; do
  link=$dest/$s
  if [ "$uninstall" = 1 ]; then
    if [ -L "$link" ] && [ "$(readlink -f "$link")" = "$src/$s" ]; then
      rm "$link"; echo "removed  $link"
    else
      echo "skipped  $link (not ours)"
    fi
    continue
  fi

  if [ -e "$link" ] && [ ! -L "$link" ]; then
    echo "SKIPPED  $link — a real file/directory is already there; move it aside first" >&2
    continue
  fi
  ln -sfn "$src/$s" "$link"
  echo "linked   $link -> $src/$s"
done

if [ "$uninstall" = 0 ]; then
  command -v agentssh >/dev/null \
    || echo "note: 'agentssh' is not on PATH — run 'cargo install --path .' from the repo root" >&2
  echo
  echo "Start a new Claude Code session, then try:  /ssh to <context> and check uptime"
fi
