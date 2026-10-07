#!/usr/bin/env bash
# The provider-independent half of setting up the two-machine rig, once both
# machines are up and reachable as lattice-srv / lattice-bots through
# .cloud/ssh_config, with SRV_PRIV and BOT_PRIV (their private addresses) in
# .cloud/session.env: install the toolchain, copy this repo and build on both,
# let the server box ssh to the bot box, measure the link with iperf3, add a
# session token key, tune and preflight. Called by scripts/cloud-up.sh
# (Scaleway) and scripts/aws-up.sh (AWS).
set -euo pipefail
cd "$(dirname "$0")/.."

state=.cloud/session.env
# shellcheck source=/dev/null
. "$state"
bot_priv=$BOT_PRIV
json() { python3 -c "import json, sys; d = json.load(sys.stdin); $1"; }
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }
on() { ssh -F .cloud/ssh_config -o BatchMode=yes "$@"; }
sed -i '/^\(LINK_GBPS\|TOKEN_KEY\)=/d' "$state"

# --- toolchain, tuning, repo, build (both machines at once) -------------------
setup() {
  on "$1" "sudo bash -s" << 'EOF'
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update
apt-get -qq install -y build-essential iperf3 rsync > /dev/null
# netem and ifb (the netem and fight modes) live here on some cloud kernels.
apt-get -qq install -y "linux-modules-extra-$(uname -r)" > /dev/null 2>&1 || true
# perf, for PROFILE=1; not every kernel has a matching package.
apt-get -qq install -y linux-tools-common "linux-tools-$(uname -r)" > /dev/null 2>&1 || echo "no perf for this kernel"

# The run-time fixes scripts/preflight.sh asks for (gone after a reboot).
sysctl -q -w net.core.rmem_max=16777216 net.core.wmem_max=16777216 kernel.perf_event_paranoid=1 kernel.kptr_restrict=0
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do [ -w "$g" ] && echo performance > "$g"; done
true
EOF
  on "$1" "command -v cargo > /dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal > /dev/null"
  rsync -a --delete -e "ssh -F .cloud/ssh_config" --exclude target --exclude results --exclude .cloud --exclude client/assets --exclude /meshy_output ./ "$1:lattice-net/"
  on "$1" "source ~/.cargo/env && cd lattice-net && cargo build --release -q -p lattice-sim"
}
say "installing the toolchain, copying the repo and building on both"
setup lattice-srv > .cloud/setup-srv.log 2>&1 &
s1=$!
setup lattice-bots > .cloud/setup-bots.log 2>&1 &
s2=$!
wait $s1 || { echo "setup failed on lattice-srv: .cloud/setup-srv.log" >&2; exit 1; }
wait $s2 || { echo "setup failed on lattice-bots: .cloud/setup-bots.log" >&2; exit 1; }

# The server box drives the bots over ssh on the Private Network.
on lattice-srv "test -f ~/.ssh/id_lattice || ssh-keygen -q -t ed25519 -N '' -f ~/.ssh/id_lattice"
on lattice-srv "cat ~/.ssh/id_lattice.pub" | on lattice-bots "cat >> ~/.ssh/authorized_keys"
on lattice-srv "printf 'Host %s\n  IdentityFile ~/.ssh/id_lattice\n  StrictHostKeyChecking accept-new\n' $bot_priv >> ~/.ssh/config && ssh -o BatchMode=yes ubuntu@$bot_priv true"

# --- the link ------------------------------------------------------------------
say "measuring the link with iperf3"
on lattice-bots "iperf3 -s -D -1 > /dev/null"
sleep 1
gbps() { json "print('%.1f' % (d['end']['sum_received']['bits_per_second'] / 1e9))"; }
up=$(on lattice-srv "iperf3 -J -c $bot_priv -t 5 -P 8" | gbps)
on lattice-bots "iperf3 -s -D -1 > /dev/null"
sleep 1
down=$(on lattice-srv "iperf3 -J -c $bot_priv -t 5 -P 8 -R" | gbps)
echo "LINK_GBPS=$up/$down" >> "$state"
say "link: $up Gbps server->bots, $down Gbps bots->server"
if python3 -c "import sys; sys.exit(0 if min($up, $down) >= 5 else 1)"; then :; else
  echo "WARNING: the link is far below 25 Gbps: 10k and the blob need several Gbps. Check before running." >&2
fi

echo "TOKEN_KEY=$(openssl rand -hex 32)" >> "$state"

# What the server's card does for GSO (and on the bots' side, for the record).
say "network cards"
on lattice-srv "lattice-net/scripts/nic-check.sh info $bot_priv" | tee .cloud/nic-srv.txt
on lattice-bots "lattice-net/scripts/nic-check.sh info $SRV_PRIV" > .cloud/nic-bots.txt
sed -i '/^NIC_USO=/d' "$state"
echo "NIC_USO=$(sed -n 's/^USO=//p' .cloud/nic-srv.txt)" >> "$state"

say "preflight"
on lattice-srv "cd lattice-net && source ~/.cargo/env && scripts/preflight.sh 40500 10000" | tee .cloud/preflight-srv.txt | tail -12
on lattice-bots "cd lattice-net && source ~/.cargo/env && scripts/preflight.sh 40999 10000" | tee .cloud/preflight-bots.txt | tail -12

