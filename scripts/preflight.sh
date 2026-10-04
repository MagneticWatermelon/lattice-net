#!/usr/bin/env bash
# Is this machine ready for scale runs? Prints the machine's setup (what a
# baseline records next to its numbers), then one check per item with the
# command that fixes it.
#
#   scripts/preflight.sh [port] [bots]      (defaults: 40500, 10000)
#
# Exit status 1 if something would break a run (no toolchain, port taken, too
# few open files allowed); warnings only make numbers less trustworthy.
# Env: PROFILE=1 also checks that perf can profile the server.
set -uo pipefail
cd "$(dirname "$0")/.."

port=${1:-40500}
bots=${2:-10000}
fails=0
warns=0

ok() { printf '  ok    %s\n' "$1"; }
note() { printf '  note  %s\n' "$1"; }
warn() {
  printf '  WARN  %s\n' "$1"
  [ $# -gt 1 ] && printf '        fix: %s\n' "$2"
  warns=$((warns + 1))
}
fail() {
  printf '  FAIL  %s\n' "$1"
  [ $# -gt 1 ] && printf '        fix: %s\n' "$2"
  fails=$((fails + 1))
}
file() { cat "$1" 2> /dev/null || echo "?"; }
have() { command -v "$1" > /dev/null; }

virt=$(systemd-detect-virt 2> /dev/null || true)
if [ -z "$virt" ]; then
  grep -qi microsoft /proc/version && virt=wsl || virt=unknown
fi
governor=$(file /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)
rmem=$(sysctl -n net.core.rmem_max)
wmem=$(sysctl -n net.core.wmem_max)
read -r port_lo port_hi < <(sysctl -n net.ipv4.ip_local_port_range)
nofile_hard=$(ulimit -Hn)
conntrack=$(file /proc/sys/net/netfilter/nf_conntrack_count)
avail_gb=$(free -g | awk '/^Mem:/ {print $7}')
load=$(cut -d' ' -f1 /proc/loadavg)
threads=$(nproc)

echo "== machine"
echo "  date       $(date -Is)"
echo "  host       $(hostname)"
echo "  commit     $(git rev-parse --short HEAD 2> /dev/null || echo ?)$(git diff --quiet HEAD 2> /dev/null || echo ' (uncommitted changes)')"
echo "  virt       $virt"
echo "  os         $(. /etc/os-release 2> /dev/null && echo "${PRETTY_NAME:-?}")"
echo "  kernel     $(uname -r)"
echo "  cpu        $(lscpu | awk -F: '/^Model name/ {sub(/^ +/, "", $2); print $2; exit}')"
echo "  threads    $threads ($(lscpu | awk -F: '/^Socket\(s\)/ {s=$2} /^Core\(s\) per socket/ {c=$2} /^Thread\(s\) per core/ {t=$2} END {printf "%d socket(s) x %d cores x %d threads", s, c, t}'))"
echo "  memory     $(free -g | awk '/^Mem:/ {print $2 " GB, " $7 " GB available"}')"
echo "  governor   $governor (boost $(file /sys/devices/system/cpu/cpufreq/boost), smt $(file /sys/devices/system/cpu/smt/control))"
echo "  thp        $(file /sys/kernel/mm/transparent_hugepage/enabled)"
echo "  cmdline    $(file /proc/cmdline)"
echo "  sockets    rmem_max $rmem, wmem_max $wmem, udp_mem $(sysctl -n net.ipv4.udp_mem | tr '\t' ' ')"
echo "  ports      $port_lo-$port_hi"
echo "  nofile     soft $(ulimit -Sn), hard $nofile_hard"
echo "  conntrack  $([ "$conntrack" = "?" ] && echo "not loaded" || echo "$conntrack flows tracked (max $(file /proc/sys/net/netfilter/nf_conntrack_max))")"
echo "  loopback   mtu $(file /sys/class/net/lo/mtu)$(have ethtool && ethtool -k lo 2> /dev/null | awk -F': ' '/^(tx-udp-segmentation|generic-segmentation-offload):/ {printf ", %s %s", $1, $2}')"
for nic in $(ip -o link show up 2> /dev/null | awk -F': ' '{sub(/@.*/, "", $2); print $2}' | grep -v '^lo$'); do
  speed=$(file "/sys/class/net/$nic/speed")
  driver=$(have ethtool && ethtool -i "$nic" 2> /dev/null | awk '/^driver:/ {print $2}')
  uso=$(have ethtool && ethtool -k "$nic" 2> /dev/null | awk -F': ' '/^tx-udp-segmentation:/ {print $2}')
  echo "  nic        $nic: $(ip -o -4 addr show "$nic" 2> /dev/null | awk '{print $4}' | paste -sd, -), speed ${speed} Mb/s, driver ${driver:-?}, tx-udp-segmentation ${uso:-?}"
done
echo "  rust       $(rustc -V 2> /dev/null || echo missing)"
echo "  cc         $(cc --version 2> /dev/null | head -1 || echo missing)"
echo "  perf       $(have perf && perf --version 2> /dev/null || echo "not installed"), perf_event_paranoid $(file /proc/sys/kernel/perf_event_paranoid)"
echo "  load       $(cut -d' ' -f1-3 /proc/loadavg)"

echo "== checks"
case $virt in
  none) ok "bare metal" ;;
  wsl) warn "running under WSL2: numbers show behavior, not capacity (bare-metal Linux is what counts)" ;;
  *) warn "virtualized ($virt): fine for the bot machine, but the server's numbers should come from bare metal" ;;
esac

if have cargo && have cc; then
  ok "cargo and a C compiler (ring builds C/asm)"
else
  fail "cargo or a C compiler is missing" \
    "sudo apt install build-essential && curl https://sh.rustup.rs -sSf | sh"
fi

if [ "$nofile_hard" = unlimited ] || [ "$nofile_hard" -ge $((bots + 64)) ]; then
  ok "open files: the bots can raise their limit to $((bots + 64)) (one socket per bot)"
else
  fail "the hard open-file limit ($nofile_hard) is below the $((bots + 64)) the bots need" \
    "add '* hard nofile 1048576' to /etc/security/limits.conf and log in again"
fi

want=$((16 << 20))
if [ "$rmem" -ge $want ] && [ "$wmem" -ge $want ]; then
  ok "socket buffers: the server gets the 16 MiB it asks for"
else
  warn "socket buffers are capped at rmem $rmem / wmem $wmem; the server asks for 16 MiB (bursts may drop)" \
    "sudo sysctl -w net.core.rmem_max=$want net.core.wmem_max=$want"
fi

case $governor in
  performance) ok "cpu governor: performance" ;;
  "?") note "no cpufreq governor exposed (VM, or the firmware controls frequency)" ;;
  *) warn "cpu governor is '$governor': clock ramps add jitter to tick times" \
    "echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor" ;;
esac

if [ $((port_hi - port_lo)) -ge $((bots + 1000)) ]; then
  ok "ephemeral ports: $((port_hi - port_lo)) for $bots bot sockets"
else
  warn "only $((port_hi - port_lo)) ephemeral ports for $bots bot sockets" \
    "sudo sysctl -w net.ipv4.ip_local_port_range='10000 65000'"
fi

if [ "$conntrack" = "?" ] || [ "$conntrack" = 0 ]; then
  ok "conntrack isn't tracking flows"
else
  warn "conntrack is tracking $conntrack flows: every packet then pays a table lookup" \
    "sudo iptables -t raw -I OUTPUT -o lo -j NOTRACK && sudo iptables -t raw -I PREROUTING -i lo -j NOTRACK"
fi

if [ -n "$(ss -Hlun "sport = :$port" 2> /dev/null)" ]; then
  fail "UDP port $port is taken (a server still running?)" "stop it, or pick another: PORT=40600 scripts/baseline.sh"
else
  ok "UDP port $port is free"
fi

if awk -v l="$load" 'BEGIN {exit !(l > 1.0)}'; then
  warn "load average $load over the last minute: something else is using the CPU" \
    "close other programs; after a build, wait a minute for the average to settle"
else
  ok "the machine is idle (load $load)"
fi

if [ "${avail_gb:-0}" -lt 4 ]; then
  warn "only ${avail_gb} GB of memory available" "close other programs"
else
  ok "${avail_gb} GB of memory available"
fi

if [ "$threads" -lt 8 ]; then
  warn "$threads hardware threads: server and bots will compete harder than on the 16-thread dev box"
fi

if [ -n "${PROFILE:-}" ]; then
  if ! have perf; then
    warn "PROFILE is set but perf isn't installed" "sudo apt install linux-tools-common linux-tools-\$(uname -r)"
  elif [ "$(file /proc/sys/kernel/perf_event_paranoid)" -gt 1 ]; then
    warn "perf_event_paranoid > 1: perf can't profile the server with kernel symbols" \
      "sudo sysctl -w kernel.perf_event_paranoid=1 kernel.kptr_restrict=0"
  else
    ok "perf can profile the server"
  fi
fi

echo "== $fails failure(s), $warns warning(s)"
[ "$fails" -eq 0 ]
