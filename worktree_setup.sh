#!/usr/bin/env bash
set -euo pipefail

echo "=== herdr worktree setup ==="
echo "worktree path : ${HERDR_WORKTREE_PATH:-<unset>}"
echo "source repo   : ${HERDR_SOURCE_REPO_ROOT:-<unset>}"
echo "workspace id  : ${HERDR_WORKSPACE_ID:-<unset>}"
echo "pane id       : ${HERDR_PANE_ID:-<unset>}"
echo "==========================="
echo
echo "(replace this script with your real bootstrap: bun install, env copy, codegen, ...)"
echo
read -r -p "press Enter to drop into a shell> " _
