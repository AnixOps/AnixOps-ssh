#!/usr/bin/env bash
# TEST ONLY. Shapes the network of a running container for the benchmarks
# (docs/benchmarks.md): netem on the container's interface for what it sends
# and, through an ifb device, for what it receives, so each direction gets
# half the RTT, the rate and the loss.
#
#   e2e/netem.sh <container> normal   RTT 20 ms, 100 Mbit/s each way, no loss
#   e2e/netem.sh <container> weak     RTT 150 ms, 1% loss each way
#   e2e/netem.sh <container> down     100% loss (the outage of M5)
#   e2e/netem.sh <container> off      remove everything
#
# Runs tc inside the container's network namespace with the host's tc
# (nsenter), so it needs root (sudo is used when not root), `ip`, `tc` and the
# `ifb` kernel module (`sudo modprobe ifb`; GitHub's ubuntu runners have it).
# Traffic must reach the container directly (its bridge address), not through
# Docker's published port, whose userspace proxy would hide the loss.
set -euo pipefail

container=${1:?usage: netem.sh <container> normal|weak|down|off}
profile=${2:?usage: netem.sh <container> normal|weak|down|off}

as_root() { if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi; }
pid=$(docker inspect -f '{{.State.Pid}}' "$container")
[ -n "$pid" ] && [ "$pid" != 0 ] || { echo "container $container is not running" >&2; exit 1; }
ns() { as_root nsenter -t "$pid" -n "$@"; }

dev=$(ns ip -o -4 route show default | awk '{print $5; exit}')
[ -n "$dev" ] || { echo "no default interface in $container" >&2; exit 1; }

case "$profile" in
  normal) args="delay 10ms rate 100mbit limit 20000" ;;
  weak)   args="delay 75ms loss 1% limit 20000" ;;
  down)   args="loss 100%" ;;
  off)
    ns tc qdisc del dev "$dev" root 2>/dev/null || true
    ns tc qdisc del dev "$dev" ingress 2>/dev/null || true
    ns ip link del ifb0 2>/dev/null || true
    exit 0
    ;;
  *) echo "unknown profile $profile" >&2; exit 2 ;;
esac

as_root modprobe ifb 2>/dev/null || true
ns ip link add ifb0 type ifb 2>/dev/null || true
ns ip link set ifb0 up
ns tc qdisc replace dev "$dev" root netem $args
ns tc qdisc add dev "$dev" handle ffff: ingress 2>/dev/null || true
ns tc filter replace dev "$dev" parent ffff: protocol all prio 1 u32 match u32 0 0 \
  action mirred egress redirect dev ifb0
ns tc qdisc replace dev ifb0 root netem $args
