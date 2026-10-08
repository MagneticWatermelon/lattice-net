#!/usr/bin/env bash
# Runs scripts/baseline.sh on the rig from scripts/cloud-up.sh: the server on
# lattice-srv, the bots on lattice-bots over the Private Network, a session
# token key on both. Then copies baselines/ back here.
#
#   scripts/cloud-run.sh [full|quick] [name] [VAR=value ...]
#
# VAR=value pairs go to baseline.sh, e.g. SERVER_THREADS=8 to match the
# 16-thread dev box (the default is every core of the server box), PROFILE=1.
# IRQ_CPUS=0-3 in this script's environment first steers the server card's
# interrupts to those CPUs (scripts/irq-affinity.sh): pair it with
# SERVER_ARGS="--rx-cpus 0-3 --worker-cpus 4-63".
# Syncs this working tree to both machines first (and rebuilds), so local
# changes are what runs.
set -euo pipefail
cd "$(dirname "$0")/.."

state=.cloud/session.env
[ -s "$state" ] || { echo "no session: scripts/cloud-up.sh first" >&2; exit 1; }
# shellcheck source=/dev/null
. "$state"
mode=${1:-full}
name=${2:-scaleway}
shift $(( $# < 2 ? $# : 2 ))

on() { ssh -F .cloud/ssh_config -o BatchMode=yes "$@"; }
for h in lattice-srv lattice-bots; do
  rsync -a --delete -e "ssh -F .cloud/ssh_config" --exclude target --exclude results --exclude .cloud --exclude client/assets --exclude /meshy_output \
    --exclude baselines ./ "$h:lattice-net/"
  on "$h" "source ~/.cargo/env && cd lattice-net && cargo build --release -q -p lattice-sim"
done

# The cards' counters around the run: drops, errors and (on AWS) the
# platform's allowance limits must not grow, or the run measured the cloud.
counters() { on lattice-srv "lattice-net/scripts/nic-check.sh counters $BOT_PRIV" > ".cloud/nic-$1-srv.txt"; on lattice-bots "lattice-net/scripts/nic-check.sh counters $SRV_PRIV" > ".cloud/nic-$1-bots.txt"; }
counters before

if [ -n "${IRQ_CPUS:-}" ]; then
  on lattice-srv "iface=\$(ip -o route get $BOT_PRIV | grep -o 'dev [^ ]*' | cut -d' ' -f2); lattice-net/scripts/irq-affinity.sh \"\$iface\" $IRQ_CPUS"
fi

# Quoted for the remote shell, so SERVER_ARGS="--sockets 4 --ingress recvfrom" stays one value.
vars=$( [ $# -eq 0 ] || printf '%q ' "$@")
echo "running baseline.sh $mode $name on lattice-srv ($SRV_PRIV), bots on lattice-bots ($BOT_PRIV)"
on lattice-srv "source ~/.cargo/env && cd lattice-net && env SERVER_IP=$SRV_PRIV BOTS_SSH=ubuntu@$BOT_PRIV TOKEN_KEY=$TOKEN_KEY $vars scripts/baseline.sh $mode $name"
rsync -a -e "ssh -F .cloud/ssh_config" lattice-srv:lattice-net/baselines/ baselines/
echo "copied back: baselines/ (commit what you want to keep)"

counters after
out=$(ls -dt baselines/*-"$name" 2> /dev/null | head -1)
report=$(python3 - << 'EOF'
import re
def load(p):
    return {k: int(v) for k, v in (l.strip().split('=', 1) for l in open(p) if '=' in l) if v.strip().lstrip('-').isdigit()}
# Whole name parts: "interrupt" holds "err" but isn't one.
bad = re.compile(r'allowance_exceeded|(^|_)(drops?|dropped|discards?|errs?|errors|fail|failed|missed|timeouts?|fifo)(_|$)', re.I)
for box in ('srv', 'bots'):
    a, b = load(f'.cloud/nic-before-{box}.txt'), load(f'.cloud/nic-after-{box}.txt')
    grew = [(k, b[k] - a.get(k, 0)) for k in sorted(b) if bad.search(k) and b[k] - a.get(k, 0) > 0]
    print(f'{box}: ' + (', '.join(f'{k} +{d}' for k, d in grew) if grew else 'no drops, errors or allowance limits'))
EOF
)
echo "network cards over the run:"
echo "$report"
[ -n "$out" ] && { echo "$report"; cat .cloud/nic-srv.txt 2> /dev/null; } > "$out/nic.txt"
if echo "$report" | grep -q '+'; then
  echo "WARNING: counters grew (above): check $out/nic.txt before trusting this run's loss and corrections" >&2
fi
