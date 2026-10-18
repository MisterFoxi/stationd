#!/bin/sh
# Orchestration only; all test assertions, builds and runtime run in containers.
set -eu
project="stationdeck-config-test-$$"
dc() { docker compose -p "$project" -f docker/stationdeck/compose.yaml -f docker/stationdeck/compose.config-test.yaml --profile config-demo "$@"; }
trap 'dc --profile discovery-test down --volumes --remove-orphans' EXIT
dc build
dc up -d --wait deck config-demo-agent
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=seed config-browser
dc stop control
minimum=$(date +%s)
dc restart config-demo-agent
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=offline -e STATIONDECK_CONFIG_MIN_TIME="$minimum" config-browser
dc up -d --no-deps --wait control
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=resume config-browser

dc --profile discovery-test up -d --wait discovery-duplicate
dc restart config-demo-agent
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=ambiguous config-browser
dc logs --tail 10 config-demo-agent | grep -F 'Multiple configuration authorities discovered'
dc --profile discovery-test stop discovery-duplicate
docker compose -p "$project" -f docker/stationdeck/compose.yaml -f docker/stationdeck/compose.config-test.yaml -f docker/stationdeck/compose.config-explicit-test.yaml --profile config-demo up -d --no-deps --force-recreate config-demo-agent
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=explicit config-browser
dc run --rm --no-deps -e STATIONDECK_CONFIG_PHASE=ui config-browser
