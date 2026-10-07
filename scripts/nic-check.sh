#!/usr/bin/env bash
# What the network card toward PEER can do, and what it counted: run on a
# rig machine by scripts/cloud-setup.sh and scripts/cloud-run.sh.
#
#   scripts/nic-check.sh info PEER       driver, model, and whether UDP GSO is
#                                        offloaded to the card (USO=hw|sw)
#   scripts/nic-check.sh counters PEER   every `ethtool -S` counter, name=value
#
# GSO (lattice-server --egress gso) is only offloaded when the card does UDP
# segmentation itself (tx-udp-segmentation "on", not "[fixed]"): Mellanox
# ConnectX-5+ (mlx5), Intel E810 (ice). Otherwise the kernel splits the packets
# (still one syscall per client). On AWS, ENA's allowance counters
# (pps_allowance_exceeded, conntrack_allowance_exceeded, ...) show packets the
# platform dropped: a run with those growing measured the cloud, not us.
set -euo pipefail
mode=${1:?info|counters}
peer=${2:?peer address}
dev=$(ip -o route get "$peer" | awk '{for (i = 1; i < NF; i++) if ($i == "dev") print $(i + 1)}')
# A VLAN's offloads come from the card under it.
phys=$dev
for l in /sys/class/net/"$dev"/lower_*; do
  [ -e "$l" ] && phys=${l##*/lower_}
done

case $mode in
  info)
    driver=$(ethtool -i "$phys" 2> /dev/null | awk '/^driver:/ {print $2}')
    model=$(cat "/sys/class/net/$phys/device/vendor" "/sys/class/net/$phys/device/device" 2> /dev/null | tr '\n' ' ')
    uso=$(ethtool -k "$phys" 2> /dev/null | awk -F': ' '/^tx-udp-segmentation:/ {print $2}')
    case $uso in
      on) verdict=hw ;;
      *) verdict=sw ;;
    esac
    echo "NIC=$phys (via $dev) driver=${driver:-?} pci=${model:-?}"
    echo "tx-udp-segmentation: ${uso:-absent}"
    echo "USO=$verdict"
    ;;
  counters)
    ethtool -S "$phys" 2> /dev/null | awk -F': ' 'NF == 2 {gsub(/^ +/, "", $1); print $1 "=" $2}'
    ;;
  *)
    echo "usage: $0 info|counters PEER" >&2
    exit 2
    ;;
esac
