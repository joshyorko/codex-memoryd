#!/usr/bin/env bash
set -euo pipefail

# Use one project selector for the heartbeat and its later shutdown.
exec docker compose --project-name "${DOGFOOD_COMPOSE_PROJECT:-codex-memoryd}" "$@"
