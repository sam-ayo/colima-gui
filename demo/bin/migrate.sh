#!/bin/sh
# A database migration that fails, so the demo has a failed container.

log() { echo "time=$(date -u +%Y-%m-%dT%H:%M:%SZ) level=$1 msg=\"$2\"$3"; }

log info "running migrations" " dir=/app/migrations"
log info "applied" " version=20260914_create_orders"
log info "applied" " version=20260921_add_cart_items"
log warn "lock held for 3s" " table=orders"
log error "migration failed" " version=20260930_add_order_index err=\"relation orders_user_id_idx already exists\""
exit 1
