#!/usr/bin/env bash
#
# Provision maked CI capacity on the shared Hetzner box (raps-ci).
#
# Usage:
#   ./provision-hetzner-runner.sh runner <index> <registration-token>
#
# Registration tokens expire in ~1 hour, so fetch one immediately before use:
#   GH_TOKEN=$(gh auth token --user dmytro-yemelianov) \
#     gh api -X POST repos/dmytro-yemelianov/maked/actions/runners/registration-token --jq .token
#

set -euo pipefail

RUNNER_VERSION="2.337.0"
REPO_URL="https://github.com/dmytro-yemelianov/maked"
RUNNER_LABELS="raps-ci,maked"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

install_runner() {
  local index="$1" token="$2"
  [ "$(uname -s)" = "Linux" ] || die "this script targets Linux (got $(uname -s))"
  [ "$(uname -m)" = "x86_64" ] || die "this script targets x86_64 (got $(uname -m))"

  local name dir
  if [ "$index" = "1" ]; then
    name="maked-ci-x64"
    dir="${HOME}/maked-runner"
  else
    name="maked-ci-x64-${index}"
    dir="${HOME}/maked-runner-${index}"
  fi

  [ -f "${dir}/.runner" ] && die "${dir} is already configured as a runner; remove it first"

  log "Downloading actions-runner ${RUNNER_VERSION} into ${dir}"
  mkdir -p "$dir"
  local tarball="actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"
  curl -fsSL -o "${dir}/${tarball}" \
    "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/${tarball}"
  tar xzf "${dir}/${tarball}" -C "$dir"
  rm -f "${dir}/${tarball}"

  log "Configuring ${name} with labels: ${RUNNER_LABELS}"
  (cd "$dir" && ./config.sh \
    --unattended \
    --url "$REPO_URL" \
    --token "$token" \
    --name "$name" \
    --labels "$RUNNER_LABELS" \
    --work _work \
    --replace)

  log "Starting background runner via run.sh"
  nohup /bin/bash "${dir}/run.sh" > "${dir}/runner.log" 2>&1 &

  log "Runner active. Check status with:"
  echo "  gh api repos/dmytro-yemelianov/maked/actions/runners --jq '.runners[] | .name + \" \" + .status'"
}

main() {
  case "${1:-}" in
    runner)
      [ $# -eq 3 ] || die "usage: $0 runner <index> <registration-token>"
      install_runner "$2" "$3"
      ;;
    *)
      die "usage: $0 runner <index> <registration-token>"
      ;;
  esac
}

main "$@"
