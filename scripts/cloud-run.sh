#!/usr/bin/env bash
# Runs scripts/baseline.sh on the rig from scripts/cloud-up.sh: the server on
# lattice-srv, the bots on lattice-bots over the Private Network, a session
# token key on both. Then copies baselines/ back here.
#
#   scripts/cloud-run.sh [full|quick] [name] [VAR=value ...]
#
# VAR=value pairs go to baseline.sh, e.g. SERVER_THREADS=8 to match the
# 16-thread dev box (the default is every core of the server box), PROFILE=1.
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

echo "running baseline.sh $mode $name on lattice-srv ($SRV_PRIV), bots on lattice-bots ($BOT_PRIV)"
on lattice-srv "source ~/.cargo/env && cd lattice-net && env SERVER_IP=$SRV_PRIV BOTS_SSH=ubuntu@$BOT_PRIV TOKEN_KEY=$TOKEN_KEY $* scripts/baseline.sh $mode $name"
rsync -a -e "ssh -F .cloud/ssh_config" lattice-srv:lattice-net/baselines/ baselines/
echo "copied back: baselines/ (commit what you want to keep)"
