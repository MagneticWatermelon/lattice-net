#!/usr/bin/env bash
# Steers a network card's interrupts to a set of CPUs, apart from the
# server's rayon workers (lattice-server --worker-cpus, --rx-cpus).
#
#   scripts/irq-affinity.sh <iface> <cpu-list>   e.g. scripts/irq-affinity.sh ens5 0-3
#   scripts/irq-affinity.sh <iface>              show where its interrupts go
#
# The workers keep their cores busy through each tick, so interrupt work
# that lands on their cores waits, or spills into ksoftirqd threads that
# compete with them (lattice-server's summary reports ksoftirqd_cpu_pct and
# the receive threads' rx_runq_wait_*_pct). This stops irqbalance (it would
# move them back) and writes the list to every MSI interrupt of the card.
# Needs root (uses sudo otherwise). Undo: start irqbalance again, or reboot.
set -euo pipefail

iface=${1:?usage: irq-affinity.sh <iface> [cpu-list]}
cpus=${2:-}
# A VLAN's (Scaleway's private network) interrupts are its parent card's.
while [ ! -d "/sys/class/net/$iface/device/msi_irqs" ]; do
  lower=$(ls -d /sys/class/net/"$iface"/lower_* 2> /dev/null | head -1 || true)
  [ -n "$lower" ] || break
  iface=${lower##*/lower_}
done
dir=/sys/class/net/$iface/device/msi_irqs
if [ ! -d "$dir" ]; then
  echo "$iface: no MSI interrupts in $dir (a virtual device?)" >&2
  exit 1
fi
irqs=$(ls "$dir" | sort -n)
as_root=""
[ "$(id -u)" = 0 ] || as_root=sudo

show() {
  for i in $irqs; do
    printf '%s:%s ' "$i" "$(cat "/proc/irq/$i/smp_affinity_list" 2>/dev/null || echo '?')"
  done
  echo
}

if [ -z "$cpus" ]; then
  echo "$iface interrupts (irq:cpus): $(show)"
  exit 0
fi
if systemctl is-active --quiet irqbalance 2>/dev/null; then
  $as_root systemctl stop irqbalance
  echo "stopped irqbalance"
fi
for i in $irqs; do
  # Some drivers' queue interrupts are kernel-managed: their affinity can't be set.
  echo "$cpus" | $as_root tee "/proc/irq/$i/smp_affinity_list" > /dev/null 2>&1 \
    || echo "irq $i: affinity not settable (kernel-managed?)" >&2
done
echo "$iface interrupts (irq:cpus): $(show)"
