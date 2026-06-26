#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$HOME/PF-Sniper-BOT"
BRANCH="main"

cd "$REPO_DIR"

echo "==> Status:"
git status -s

# Guard: flag anything that looks like a real secret/config before staging
if git status -s | grep -E -i 'config\.toml$|\.env$|keypair|\.key$|wallet' | grep -v 'config.sample.toml'; then
  echo "WARNING: the file(s) above look like they could contain secrets."
  echo "Ctrl+C now to abort and check .gitignore, or it'll continue in 5s."
  sleep 5
fi

git add -A

if git diff --cached --quiet; then
  echo "==> Nothing to commit."
else
  MSG="${1:-update $(date '+%Y-%m-%d %H:%M:%S')}"
  git commit -m "$MSG"
fi

echo "==> Pulling latest (rebase) before push..."
git pull --rebase origin "$BRANCH"

echo "==> Pushing..."
git push origin "$BRANCH"

echo "==> Done."
