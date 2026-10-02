#!/bin/sh
# Writes the log of a small HTTP API: colored level words, request lines,
# a slow query now and then, and an error with a stack trace.

green='\033[32m'; yellow='\033[33m'; red='\033[31m'; cyan='\033[36m'; dim='\033[2m'; reset='\033[0m'

info() { printf "${green}INFO${reset}  %s\n" "$*"; }
warn() { printf "${yellow}WARN${reset}  %s\n" "$*"; }
error() { printf "${red}ERROR${reset} %s\n" "$*"; }

info "storefront-api 2.4.1 (node 22.9.0, production)"
info "connected to postgres at db:5432/shop (pool size 10)"
info "connected to redis at cache:6379/0"
info "listening on ${cyan}http://localhost:${PORT:-3000}${reset}"
info "health check at http://localhost:${PORT:-3000}/healthz"

paths="/api/products /api/products/42 /api/cart /api/cart/items /api/orders /api/users/me /api/search?q=mug /healthz"
methods="GET GET GET POST GET GET GET GET"
n=0
while true; do
    n=$((n + 1))
    i=$((RANDOM % 8 + 1))
    path=$(echo $paths | cut -d' ' -f$i)
    method=$(echo $methods | cut -d' ' -f$i)
    ms=$((RANDOM % 40 + 3))
    status=200
    [ "$method" = "POST" ] && status=201
    [ $((RANDOM % 17)) -eq 0 ] && status=404
    printf "${green}INFO${reset}  %s %s ${dim}%s %sms${reset}\n" "$method" "$path" "$status" "$ms"
    if [ $((n % 9)) -eq 0 ]; then
        warn "slow query took $((RANDOM % 900 + 600))ms: SELECT * FROM orders WHERE user_id = \$1"
    fi
    if [ $((n % 23)) -eq 0 ]; then
        error "payment provider timeout after 5000ms (order $((RANDOM % 9000 + 1000)))"
        echo "    at PaymentClient.charge (/app/src/payments/client.js:88:13)"
        echo "    at async OrderService.checkout (/app/src/orders/service.js:142:5)"
        echo "    at async /app/src/routes/orders.js:31:20"
    fi
    sleep 1
done
