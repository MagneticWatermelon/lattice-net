#!/usr/bin/env bash
# Rents the two-machine test rig on Scaleway Elastic Metal: a server box and a
# bot box, bare metal, hourly billing, joined by a Private Network.
#
#   scripts/cloud-up.sh [--yes]
#   scripts/cloud-up.sh --resume     (finish setting up a session whose servers exist)
#
# BILLING STARTS WHEN THE SERVERS ARE CREATED and runs until they're deleted
# (stopping isn't enough): end every session with scripts/cloud-down.sh.
# Without --yes it shows the hourly price and asks first.
#
# Steps: check both offers are hourly and in stock; create both servers (tagged
# lattice-net, ids saved to .cloud/session.env at once, so cloud-down can always
# clean up); wait for the OS install; join the Private Network and configure
# its VLAN; install the toolchain, copy this repo and build; let the server box
# ssh to the bot box; measure the link with iperf3; generate a session token
# key; tune and preflight both machines.
#
# Env: ZONE (fr-par-2), SRV_TYPE (EM-I620E-NVMe), BOT_TYPE (EM-I320E-NVMe),
#      PN_NAME (lattice-test), SSH_KEY (~/.ssh/id_ed25519_scaleway).
# Needs scw configured (scw init) with the key's public half in Scaleway IAM.
set -euo pipefail
cd "$(dirname "$0")/.."

zone=${ZONE:-fr-par-2}
region=${zone%-*}
srv_type=${SRV_TYPE:-EM-I620E-NVMe}
bot_type=${BOT_TYPE:-EM-I320E-NVMe}
pn_name=${PN_NAME:-lattice-test}
key=${SSH_KEY:-$HOME/.ssh/id_ed25519_scaleway}
yes=
resume=
[ "${1:-}" = --yes ] && yes=1
[ "${1:-}" = --resume ] && resume=1

state=.cloud/session.env
mkdir -p .cloud
json() { python3 -c "import json, sys; d = json.load(sys.stdin); $1"; }
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }
pn_id=$(scw vpc private-network list name="$pn_name" region="$region" -o json | json "
print(next(p['id'] for p in d if p['name'] == '$pn_name'))")

if [ -n "$resume" ]; then
  # shellcheck source=/dev/null
  . "$state"
  zone=$ZONE
  srv_id=$SRV_ID
  bot_id=$BOT_ID
  total=$PRICE_PER_HOUR
  sed -i '/^\(SRV_PUB\|BOT_PUB\|SRV_PRIV\|BOT_PRIV\|LINK_GBPS\|TOKEN_KEY\)=/d' "$state"
  say "resuming the session of lattice-srv $srv_id and lattice-bots $bot_id"
elif [ -s "$state" ]; then
  echo "a session is already up ($state): use it, --resume its setup, or end it with scripts/cloud-down.sh" >&2
  exit 1
else

# --- what we'd rent -----------------------------------------------------------
offer() { # TYPE -> "id price-per-hour stock name" of its hourly offer, or nothing
  scw baremetal offer list zone="$zone" subscription-period=hourly -o json | json "
for o in d:
    if o['name'].upper() == '$1'.upper() and o.get('subscription_period') == 'hourly' and o.get('enable'):
        p = o.get('price_per_hour') or {}
        print(o['id'], '%.3f' % (p.get('units', 0) + p.get('nanos', 0) / 1e9), o.get('stock'), o['name'])
        break"
}
# server create resolves the type by its exact (case-sensitive) offer name.
read -r srv_offer srv_price srv_stock srv_name < <(offer "$srv_type") || true
read -r bot_offer bot_price bot_stock bot_name < <(offer "$bot_type") || true
for t in srv bot; do
  o=${t}_offer s=${t}_stock ty=${t}_type
  if [ -z "${!o:-}" ]; then
    echo "no hourly offer for ${!ty} in $zone" >&2
    exit 1
  fi
  if [ "${!s}" = empty ]; then
    echo "${!ty} is out of stock in $zone (try ZONE=fr-par-1, or SRV_TYPE=EM-I320E-NVMe)" >&2
    exit 1
  fi
done
srv_type=$srv_name
bot_type=$bot_name
os_id=$(scw baremetal os list zone="$zone" -o json | json "
print(next(o['id'] for o in d if o['name'] == 'Ubuntu' and o['version'].startswith('24.04')))")
pub=$(cut -d' ' -f2 "$key.pub")
key_id=$(scw iam ssh-key list -o json | json "
print(next(k['id'] for k in d if k['public_key'].split()[1] == '$pub'))")
total=$(python3 -c "print('%.2f' % ($srv_price + $bot_price))")
echo "server: $srv_type, EUR $srv_price/h (stock: $srv_stock)"
echo "bots:   $bot_type, EUR $bot_price/h (stock: $bot_stock)"
echo "zone $zone, Ubuntu 24.04, Private Network $pn_name, SSH key $key"
echo "=> EUR $total per hour, billed from creation until scripts/cloud-down.sh deletes them"
if [ -z "$yes" ]; then
  read -r -p "create both servers? [y/N] " answer
  [ "$answer" = y ] || [ "$answer" = Y ] || { echo "nothing created"; exit 1; }
fi

# --- create (ids saved immediately) -------------------------------------------
create() { # NAME TYPE -> server id, or scw's error on stderr
  local out
  out=$(scw baremetal server create zone="$zone" name="$1" type="$2" tags.0=lattice-net \
    install.os-id="$os_id" install.hostname="$1" install.ssh-key-ids.0="$key_id" -o json 2>&1) || {
    echo "$out" >&2
    return 1
  }
  echo "$out" | json "print(d['id'])"
}
{
  echo "ZONE=$zone"
  echo "SSH_KEY=$key"
  echo "PRICE_PER_HOUR=$total"
  echo "CREATED=$(date -Is)"
} > "$state"
if ! srv_id=$(create lattice-srv "$srv_type"); then
  rm -f "$state"
  echo "creating lattice-srv failed (see above); nothing was created" >&2
  exit 1
fi
echo "SRV_ID=$srv_id" >> "$state"
if ! bot_id=$(create lattice-bots "$bot_type"); then
  echo "creating lattice-bots failed (see above); lattice-srv exists and is billing: run scripts/cloud-down.sh" >&2
  exit 1
fi
echo "BOT_ID=$bot_id" >> "$state"
say "created lattice-srv $srv_id and lattice-bots $bot_id; billing has started"

# Verify what was created really is the hourly offer we priced.
for id in "$srv_id" "$bot_id"; do
  got=$(scw baremetal server get "$id" zone="$zone" -o json | json "print(d['offer_id'])")
  if [ "$got" != "$srv_offer" ] && [ "$got" != "$bot_offer" ]; then
    echo "server $id has offer $got, not the hourly offer priced above: run scripts/cloud-down.sh" >&2
    exit 1
  fi
done

fi # creation

say "waiting for the OS installs (often 10-20 min)"
scw baremetal server wait "$srv_id" zone="$zone" timeout=60m > /dev/null &
w1=$!
scw baremetal server wait "$bot_id" zone="$zone" timeout=60m > /dev/null &
w2=$!
wait $w1
wait $w2

ipv4() { scw baremetal server get "$1" zone="$zone" -o json | json "
print(next(i['address'] for i in d['ips'] if i['version'] == 'IPv4'))"; }
srv_pub=$(ipv4 "$srv_id")
bot_pub=$(ipv4 "$bot_id")
echo "SRV_PUB=$srv_pub" >> "$state"
echo "BOT_PUB=$bot_pub" >> "$state"
say "installed: lattice-srv $srv_pub, lattice-bots $bot_pub"

# --- ssh ----------------------------------------------------------------------
cat > .cloud/ssh_config << EOF
Host lattice-srv
  HostName $srv_pub
Host lattice-bots
  HostName $bot_pub
Host lattice-*
  User ubuntu
  IdentityFile $key
  IdentitiesOnly yes
  UserKnownHostsFile $PWD/.cloud/known_hosts
  StrictHostKeyChecking accept-new
  ServerAliveInterval 30
EOF
on() { ssh -F .cloud/ssh_config -o BatchMode=yes "$@"; }
for h in lattice-srv lattice-bots; do
  for _ in $(seq 60); do on -o ConnectTimeout=5 "$h" true 2> /dev/null && break; sleep 5; done
  on "$h" true
done

# --- Private Network: attach, then the VLAN interface inside Linux ------------
for id in "$srv_id" "$bot_id"; do
  attached=$(scw baremetal private-network list server-id="$id" zone="$zone" -o json | json "
print(any(p['private_network_id'] == '$pn_id' for p in d))")
  [ "$attached" = True ] || scw baremetal private-network add server-id="$id" private-network-id="$pn_id" zone="$zone" > /dev/null
done
vlan() { # server id -> its VLAN on the Private Network, once attached
  for _ in $(seq 60); do
    v=$(scw baremetal private-network list server-id="$1" zone="$zone" -o json | json "
print(next((str(p['vlan']) for p in d if p['private_network_id'] == '$pn_id' and p.get('status') == 'attached'), ''))")
    [ -n "$v" ] && { echo "$v"; return; }
    sleep 5
  done
  echo "server $1 never got attached to $pn_name" >&2
  return 1
}
join_pn() { # HOST VLAN -> the host's IPv4 on the Private Network (DHCP)
  on "$1" "sudo bash -s $2" << 'EOF'
set -e
vlan=$1
# The VLAN links to the public NIC by its netplan id (cloud-init calls it eth0
# and matches it by MAC), and its name must fit in 15 characters.
nic=$(ip -o route get 1.1.1.1 | awk '{for (i = 1; i < NF; i++) if ($i == "dev") print $(i + 1)}')
mac=$(cat "/sys/class/net/$nic/address")
parent=$(python3 -c "
import glob, yaml
for f in sorted(glob.glob('/etc/netplan/*.yaml')):
    eths = ((yaml.safe_load(open(f)) or {}).get('network') or {}).get('ethernets') or {}
    for name, e in eths.items():
        if name == '$nic' or str(((e or {}).get('match') or {}).get('macaddress', '')).lower() == '$mac':
            print(name)
            raise SystemExit
" 2> /dev/null || true)
parent=${parent:-$nic}
cat > /etc/netplan/60-lattice-pn.yaml << YAML
network:
  version: 2
  vlans:
    vlan$vlan:
      id: $vlan
      link: $parent
      dhcp4: true
YAML
chmod 600 /etc/netplan/60-lattice-pn.yaml
netplan apply
for _ in $(seq 30); do
  ip=$(ip -o -4 addr show "vlan$vlan" | awk '{split($4, a, "/"); print a[1]}')
  [ -n "$ip" ] && { echo "$ip"; exit 0; }
  sleep 2
done
echo "no DHCP address on vlan$vlan" >&2
exit 1
EOF
}
srv_priv=$(join_pn lattice-srv "$(vlan "$srv_id")")
bot_priv=$(join_pn lattice-bots "$(vlan "$bot_id")")
echo "SRV_PRIV=$srv_priv" >> "$state"
echo "BOT_PRIV=$bot_priv" >> "$state"
say "Private Network: lattice-srv $srv_priv, lattice-bots $bot_priv"

# --- toolchain, tuning, repo, build (both machines at once) -------------------
setup() {
  on "$1" "sudo bash -s" << 'EOF'
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update
apt-get -qq install -y build-essential iperf3 rsync > /dev/null
# perf, for PROFILE=1; not every kernel has a matching package.
apt-get -qq install -y linux-tools-common "linux-tools-$(uname -r)" > /dev/null 2>&1 || echo "no perf for this kernel"

# The run-time fixes scripts/preflight.sh asks for (gone after a reboot).
sysctl -q -w net.core.rmem_max=16777216 net.core.wmem_max=16777216 kernel.perf_event_paranoid=1 kernel.kptr_restrict=0
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do [ -w "$g" ] && echo performance > "$g"; done
true
EOF
  on "$1" "command -v cargo > /dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal > /dev/null"
  rsync -a --delete -e "ssh -F .cloud/ssh_config" --exclude target --exclude results --exclude .cloud ./ "$1:lattice-net/"
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
say "measuring the Private Network with iperf3"
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

say "preflight"
on lattice-srv "cd lattice-net && source ~/.cargo/env && scripts/preflight.sh 40500 10000" | tee .cloud/preflight-srv.txt | tail -12
on lattice-bots "cd lattice-net && source ~/.cargo/env && scripts/preflight.sh 40999 10000" | tee .cloud/preflight-bots.txt | tail -12

cat << EOF

Up, billing EUR $total/h. ssh -F .cloud/ssh_config lattice-srv | lattice-bots
Run:  scripts/cloud-run.sh full scaleway                (the matrix, ~16 min)
End:  scripts/cloud-down.sh                             (copies baselines back, deletes both)
EOF
