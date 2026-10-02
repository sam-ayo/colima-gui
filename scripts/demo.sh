#!/bin/bash
# Fills a Colima profile with sample data for a demo or for screenshots:
# the Compose stack in demo/compose.yaml, one container that is not in a
# project, and some images that no container uses.
#
# The data goes into a separate profile (default "demo"), so your own
# containers stay out of the screenshots. Colima changes the current Docker
# context when it starts a profile. This script sets it back after the start.
#
# Usage:
#   scripts/demo.sh up [profile]       Start the profile and the sample data.
#   scripts/demo.sh down [profile]     Remove the sample data.
#   scripts/demo.sh delete [profile]   Delete the profile and all its data.
#   scripts/demo.sh app [profile]      Run the app on the profile (cargo run).

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cmd="${1:-}"
profile="${2:-demo}"
socket="unix://$HOME/.colima/$profile/docker.sock"

dock() { DOCKER_HOST="$socket" docker "$@"; }

start_profile() {
    if colima status "$profile" >/dev/null 2>&1; then
        return
    fi
    local context
    context="$(docker context show 2>/dev/null || true)"
    colima start "$profile" --cpu 2 --memory 2 --disk 20
    if [ -n "$context" ]; then
        docker context use "$context" >/dev/null || true
    fi
}

case "$cmd" in
    up)
        start_profile
        dock compose -f "$root/demo/compose.yaml" up -d
        if ! dock container inspect whoami >/dev/null 2>&1; then
            dock run -d --name whoami --restart unless-stopped -p 8081:80 traefik/whoami:v1.10 >/dev/null
        fi
        for image in hello-world:latest python:3.13-slim; do
            dock pull -q "$image" >/dev/null
        done
        echo "demo: sample data is ready in profile \"$profile\""
        echo "demo: run the app with: scripts/demo.sh app $profile"
        ;;
    down)
        dock compose -f "$root/demo/compose.yaml" down -v
        dock rm -f whoami >/dev/null 2>&1 || true
        ;;
    delete)
        colima delete --force "$profile"
        ;;
    app)
        cd "$root"
        DOCKER_HOST="$socket" cargo run
        ;;
    *)
        sed -n '2,15p' "$0"
        exit 1
        ;;
esac
