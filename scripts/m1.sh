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
#      BOT_ARGS (extra lattice-bots flags, e.g. "--full-every 30"),
#      OUT (results directory, default results/m1-<scenario>-<bots>-<time>),
#      PROFILE=1 (perf-record the server from PROFILE_DELAY=15 s for PROFILE_SECS=15 s
#      into $OUT/perf.txt; needs perf and kernel.perf_event_paranoid <= 1).
# Each run leaves server.log, server.csv (one row per report window), server.summary and
# bots.summary (key=value, what scripts/baseline.sh reads).
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

out=${OUT:-results/m1-$scenario-$count-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$out"
cargo build --release -q -p lattice-sim
bin=target/release

# --duration caps the run: if the bots never connected (wrong port or key),
# --until-empty alone would wait forever.
"$bin/lattice-server" --spawn "$spawn" --threads "$server_threads" --until-empty --duration $(( secs + 60 )) \
  --csv "$out/server.csv" --summary "$out/server.summary" "$@" > "$out/server.log" 2>&1 &
srv=$!
trap 'kill $srv 2>/dev/null || true' EXIT
sleep 0.5

# A flat profile of the server in the middle of the run (self time per function,
# kernel included when perf may read kernel symbols). Never fails the run.
prof=
if [ -n "${PROFILE:-}" ]; then
  if command -v perf > /dev/null; then
    ( sleep "${PROFILE_DELAY:-15}"
      perf record -q -F 999 -p "$srv" -o "$out/perf.data" -- sleep "${PROFILE_SECS:-15}" ) > "$out/perf.log" 2>&1 &
    prof=$!
  else
    echo "PROFILE is set but perf isn't installed: no profile" >&2
  fi
fi

# BOT_ARGS: extra lattice-bots flags, e.g. BOT_ARGS="--full-every 30" for sink bots.
bots() { "$bin/lattice-bots" --threads "$bot_threads" ${BOT_ARGS:-} "$@"; }
if [ "$scenario" = joins ]; then
  bots --count "$count" --duration "$secs" --summary "$out/bots.summary" > "$out/bots.log" &
  base=$!
  sleep 15
  bots --count 500 --ramp 50 --duration $(( secs - 15 )) --seed 2 --summary "$out/joiners.summary" > "$out/joiners.log"
  wait $base
else
  bots --count "$count" --duration "$secs" --summary "$out/bots.summary" > "$out/bots.log"
fi
wait $srv || true

if [ -n "$prof" ]; then
  wait "$prof" || true
  if [ -s "$out/perf.data" ]; then
    perf report -i "$out/perf.data" --stdio --no-children --sort dso,sym --percent-limit 0.3 \
      > "$out/perf.txt" 2>> "$out/perf.log" || true
    rm -f "$out/perf.data"
  else
    echo "no profile: see $out/perf.log" >&2
  fi
fi

echo "== $scenario, $count bots, ${secs}s -> $out"
sed -n '/== summary/,$p' "$out/server.log"
for f in "$out"/bots.log "$out"/joiners.log; do
  if [ -f "$f" ]; then sed -n '/== bots summary/,$p' "$f"; fi
done
