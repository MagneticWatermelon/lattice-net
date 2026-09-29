#!/usr/bin/env bash
# M1 scale-test scenarios: server + bot swarm on this machine, results in results/.
#
#   scripts/m1.sh <uniform|hotspots|blob|joins> <bots> [seconds] [extra lattice-server args...]
#
#   uniform   everyone spread over the 8x8 km continent
#   hotspots  3 hotspots of ~800 (the first 2,400 bots), the rest uniform
#   blob      3,000 bots in a 200 m disk, the rest uniform
#   joins     <bots> uniform, then 500 more join at 50/s starting at t=15 s
#
# Env: SERVER_THREADS (rayon threads, default nproc/2), BOT_THREADS (default nproc/2, max 8),
#      BOT_ARGS (extra lattice-bots flags, e.g. "--full-every 30").
# Only bare-metal Linux numbers count; on WSL2 this checks behavior, not capacity.
set -euo pipefail
cd "$(dirname "$0")/.."

scenario=${1:?scenario}; count=${2:?bots}; secs=${3:-60}
shift $(( $# < 3 ? $# : 3 ))
cores=$(nproc)
server_threads=${SERVER_THREADS:-$(( cores / 2 ))}
bot_threads=${BOT_THREADS:-$(( cores / 2 > 8 ? 8 : cores / 2 ))}

case $scenario in
  uniform|hotspots|blob) spawn=$scenario ;;
  joins) spawn=uniform ;;
  *) echo "unknown scenario $scenario" >&2; exit 2 ;;
esac

out=results/m1-$scenario-$count-$(date +%Y%m%d-%H%M%S)
mkdir -p "$out"
cargo build --release -q -p lattice-sim
bin=target/release

"$bin/lattice-server" --spawn "$spawn" --threads "$server_threads" --until-empty \
  --csv "$out/server.csv" "$@" > "$out/server.log" 2>&1 &
srv=$!
trap 'kill $srv 2>/dev/null || true' EXIT
sleep 0.5

# BOT_ARGS: extra lattice-bots flags, e.g. BOT_ARGS="--full-every 30" for sink bots.
bots() { "$bin/lattice-bots" --threads "$bot_threads" ${BOT_ARGS:-} "$@"; }
if [ "$scenario" = joins ]; then
  bots --count "$count" --duration "$secs" > "$out/bots.log" &
  base=$!
  sleep 15
  bots --count 500 --ramp 50 --duration $(( secs - 15 )) --seed 2 > "$out/joiners.log"
  wait $base
else
  bots --count "$count" --duration "$secs" > "$out/bots.log"
fi
wait $srv || true

echo "== $scenario, $count bots, ${secs}s -> $out"
sed -n '/== summary/,$p' "$out/server.log"
for f in "$out"/bots.log "$out"/joiners.log; do
  [ -f "$f" ] && sed -n '/== bots summary/,$p' "$f"
done
