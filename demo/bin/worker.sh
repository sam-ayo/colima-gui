#!/bin/sh
# Writes the JSON log of a queue worker, as pino does.

now() { date -u +%Y-%m-%dT%H:%M:%S.000Z; }
log() { printf '{"level":"%s","time":"%s","pid":7,"msg":"%s"%s}\n' "$1" "$(now)" "$2" "$3"; }

log info "worker started" ",\"queues\":\"${QUEUE}\",\"concurrency\":${CONCURRENCY:-4}"
job=1000
while true; do
    job=$((job + 1))
    case $((RANDOM % 6)) in
        0|1) log info "sent order confirmation" ",\"job\":$job,\"queue\":\"emails\",\"ms\":$((RANDOM % 300 + 80))" ;;
        2|3) log info "made thumbnail" ",\"job\":$job,\"queue\":\"thumbnails\",\"file\":\"products/item-$((RANDOM % 40)).jpg\"" ;;
        4) log debug "queue depth" ",\"emails\":$((RANDOM % 12)),\"thumbnails\":$((RANDOM % 5))" ;;
        5) log warn "retry scheduled" ",\"job\":$job,\"queue\":\"emails\",\"attempt\":2,\"reason\":\"SMTP 421 try again later\"" ;;
    esac
    sleep 2
done
