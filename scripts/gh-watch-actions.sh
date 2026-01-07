#!/usr/bin/env bash
set -euo pipefail

INTERVAL=30
LIMIT=20
WORKFLOW=""
BRANCH=""
ONCE=false
REPO="${REPO:-}"

usage() {
  cat <<'EOF'
Usage: gh-watch-actions.sh [options]

Options:
  -r, --repo <owner/repo>     Repository to monitor (default: auto-detect)
  -w, --workflow <name>       Filter by workflow name
  -b, --branch <name>         Filter by branch
  -i, --interval <seconds>    Poll interval (default: 30)
  -l, --limit <n>             Runs to show (default: 20)
  --once                      Run once and exit
  -h, --help                  Show help
EOF
}

require_gh() {
  if ! command -v gh >/dev/null 2>&1; then
    echo "gh CLI not found. Install from https://cli.github.com/" >&2
    exit 1
  fi
}

parse_repo_from_git() {
  if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    return 1
  fi

  local url
  url=$(git remote get-url origin 2>/dev/null || true)
  if [[ -z "$url" ]]; then
    return 1
  fi

  case "$url" in
    git@github.com:*.git)
      echo "${url#git@github.com:}" | sed 's/\.git$//'
      return 0
      ;;
    https://github.com/*)
      echo "${url#https://github.com/}" | sed 's/\.git$//'
      return 0
      ;;
  esac

  return 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -r|--repo)
      REPO="$2"
      shift 2
      ;;
    -w|--workflow)
      WORKFLOW="$2"
      shift 2
      ;;
    -b|--branch)
      BRANCH="$2"
      shift 2
      ;;
    -i|--interval)
      INTERVAL="$2"
      shift 2
      ;;
    -l|--limit)
      LIMIT="$2"
      shift 2
      ;;
    --once)
      ONCE=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

require_gh

if [[ -z "$REPO" ]]; then
  if REPO=$(parse_repo_from_git); then
    :
  else
    REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)
  fi
fi

if [[ -z "$REPO" ]]; then
  echo "Unable to determine repo. Use --repo owner/repo." >&2
  exit 1
fi

if ! [[ "$INTERVAL" =~ ^[0-9]+$ ]] || [[ "$INTERVAL" -lt 1 ]]; then
  echo "Invalid --interval: $INTERVAL" >&2
  exit 1
fi

if ! [[ "$LIMIT" =~ ^[0-9]+$ ]] || [[ "$LIMIT" -lt 1 ]]; then
  echo "Invalid --limit: $LIMIT" >&2
  exit 1
fi

while true; do
  echo "==> $(date -u +"%Y-%m-%dT%H:%M:%SZ")  repo=$REPO"
  args=(run list -R "$REPO" --limit "$LIMIT")
  if [[ -n "$WORKFLOW" ]]; then
    args+=(--workflow "$WORKFLOW")
  fi
  if [[ -n "$BRANCH" ]]; then
    args+=(--branch "$BRANCH")
  fi
  gh "${args[@]}"

  if $ONCE; then
    break
  fi

  sleep "$INTERVAL"
done
