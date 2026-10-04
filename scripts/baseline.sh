#!/usr/bin/env bash
# One comparable baseline: the scenario matrix, run the same way on any Linux
# box, with the machine's setup recorded next to the numbers.
#
#   scripts/baseline.sh [full|quick] [name]
#
#   full   8 scenarios x REPEAT (2) runs of 60 s, interleaved: ~16 min
#   quick  2 small scenarios x 1 run of 20 s: checks the harness, not the machine
#
# Writes baselines/<date>-<name>/ (name defaults to the host name):
#   env.txt      the machine and the preflight checks (scripts/preflight.sh)
#   summary.md   server, phase and client tables, one row per run
#   <run>/       each run's logs, per-window CSV and key=value summaries
#
# Server and bots share this machine and talk over loopback, like scripts/m1.sh,
# unless BOTS_SSH names a second machine (see Two machines below).
# Env: REPEAT, PORT (40500), PROFILE=1 (perf profiles of the first 10k and GSO
# blob runs), FORCE=1 (run despite failed checks), SERVER_THREADS, BOT_THREADS.
#
# Two machines: BOTS_SSH=user@host runs the bots there (from ~/lattice-net,
# built), SERVER_IP=<this machine's address on the link between them> is where
# the server listens, and TOKEN_KEY=<64 hex digits> replaces the public dev key
# on both sides (a server on a reachable address must not take dev tokens).
# scripts/cloud-run.sh sets all three.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-full}
name=${2:-$(hostname -s)}
port=${PORT:-40500}
case $mode in
  full) repeat=${REPEAT:-2}; secs=60 ;;
  quick) repeat=${REPEAT:-1}; secs=20 ;;
  *) echo "usage: $0 [full|quick] [name]" >&2; exit 2 ;;
esac

# id | scenario | bots | extra lattice-server args | profile it
if [ "$mode" = full ]; then
  runs=(
    "uniform-1k|uniform|1000||"
    "uniform-5k|uniform|5000||"
    "uniform-10k|uniform|10000||profile"
    "uniform-10k-noladder|uniform|10000|--ladder off|"
    "hotspots-5k|hotspots|5000||"
    "blob-3k-sendmmsg|blob|3000|--egress sendmmsg|"
    "blob-3k-gso|blob|3000|--egress gso|profile"
    "joins-5k|joins|5000||"
  )
  max_bots=10000
else
  runs=(
    "uniform-1k|uniform|1000||"
    "blob-1k-gso|blob|1000|--egress gso|"
  )
  max_bots=1000
fi

dir=baselines/$(date +%Y-%m-%d)-$name
[ -e "$dir" ] && dir=$dir-$(date +%H%M)
mkdir -p "$dir"

server_ip=${SERVER_IP:-127.0.0.1}
remote=${BOTS_SSH:-}
key_args=
[ -n "${TOKEN_KEY:-}" ] && key_args="--token-key $TOKEN_KEY"

if ! scripts/preflight.sh "$port" "$max_bots" | tee "$dir/env.txt"; then
  if [ -z "${FORCE:-}" ]; then
    echo "preflight failed (see above); fix it, or FORCE=1 to run anyway" >&2
    exit 1
  fi
fi
if [ -n "$remote" ]; then
  echo "== bot machine ($remote)" | tee -a "$dir/env.txt"
  # The port check is about the server's machine; any free port will do here.
  ssh -o BatchMode=yes "$remote" "source ~/.cargo/env 2> /dev/null; cd lattice-net && scripts/preflight.sh 40999 $max_bots" | tee -a "$dir/env.txt" || true
fi
cargo build --release -q -p lattice-sim

cores=$(nproc)
if [ -n "$remote" ]; then
  # Alone on the machine: the server takes all of it, the bots half of theirs.
  server_threads=${SERVER_THREADS:-$cores}
  bot_threads=${BOT_THREADS:-$(( $(ssh -o BatchMode=yes "$remote" nproc) / 2 ))}
else
  server_threads=${SERVER_THREADS:-$(( cores / 2 ))}
  bot_threads=${BOT_THREADS:-$(( cores / 2 > 8 ? 8 : cores / 2 ))}
fi
total=$(( repeat * ${#runs[@]} ))
started=$(date +%s)
n=0
# Interleaved (every scenario once, then again) so drift over the session
# doesn't land on one scenario.
for r in $(seq 1 "$repeat"); do
  for spec in "${runs[@]}"; do
    IFS='|' read -r id scenario count extra prof <<< "$spec"
    n=$((n + 1))
    out=$dir/$id-$r
    mkdir -p "$out"
    printf '[%s] %d/%d %s run %d\n' "$(date +%T)" "$n" "$total" "$id" "$r"
    profile=
    [ -n "${PROFILE:-}" ] && [ -n "$prof" ] && [ "$r" = 1 ] && profile=1
    # shellcheck disable=SC2086 # $extra is a list of server flags
    # shellcheck disable=SC2086 # $key_args is empty or a flag and its value
    if ! OUT=$out PROFILE=$profile SERVER_THREADS=$server_threads BOT_THREADS=$bot_threads BOTS_SSH=$remote \
      BOT_ARGS="--server $server_ip:$port $key_args ${BOT_ARGS:-}" \
      scripts/m1.sh "$scenario" "$count" "$secs" --bind "$server_ip:$port" $key_args $extra > "$out/console.log" 2>&1; then
      echo "  failed: see $out/console.log"
    fi
    sleep 2 # let the last datagrams and sockets drain
  done
done

# key=value lookup in a --summary file; "-" when missing.
kv() {
  [ -f "$1" ] || { echo -; return; }
  awk -F= -v k="$2" '$1 == k {print substr($0, length(k) + 2); found = 1; exit} END {if (!found) print "-"}' "$1"
}
env_line() { awk -v k="$1" '$1 == k {sub(/^ +[^ ]+ +/, ""); print; exit}' "$dir/env.txt"; }
kilo() { awk -v v="$1" 'BEGIN {if (v == "-") print "-"; else printf "%.0f", v / 1000}'; }

{
  echo "# Baseline: $name, $(date +%Y-%m-%d)"
  echo
  echo "- **Machine:** $(env_line cpu), $(env_line threads); $(env_line os), kernel $(env_line kernel), virt: $(env_line virt)"
  echo "- **Commit:** $(env_line commit)"
  if [ -n "$remote" ]; then
    where="bots on a second machine ($(awk '/^== bot machine/ {f=1} f && $1 == "cpu" {sub(/^ +[^ ]+ +/, ""); print; exit}' "$dir/env.txt")), server listening on $server_ip"
  else
    where="on the same machine over loopback"
  fi
  echo "- **Runs:** ${#runs[@]} scenarios x $repeat, ${secs} s each, interleaved. Server $server_threads threads, bots $bot_threads threads, $where. Took $(( ($(date +%s) - started) / 60 )) min."
  echo "- **Setup and checks:** \`env.txt\`. Raw logs, per-window CSVs and key=value summaries: one directory per run."
  echo
  echo "Steady state: from 3 s after the first client until clients start leaving. Times in ms."
  echo
  echo "## Server"
  echo
  echo "| run | clients | level (tick rate) | tick p50 / p99 / max | overruns | out kpps | packets per client-tick | wire B per client-tick | down kbps per client | kernel drops rcv / snd |"
  echo "|---|---|---|---|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      s=$dir/$id-$r/server.summary
      if [ ! -f "$s" ]; then
        echo "| $id #$r | failed | | | | | | | | |"
        continue
      fi
      echo "| $id #$r | $(kv "$s" peak_clients) | L$(kv "$s" level_mode) ($(kv "$s" tick_hz) Hz; $(kv "$s" levels)) | $(kv "$s" tick_p50_ms) / $(kv "$s" tick_p99_ms) / $(kv "$s" tick_max_ms) | $(kv "$s" steady_overruns) | $(kilo "$(kv "$s" out_pps)") | $(kv "$s" packets_per_client_tick) | $(kv "$s" wire_bytes_per_client_tick) | $(kv "$s" down_kbps_per_client) | $(kv "$s" kernel_rcvbuf_drops) / $(kv "$s" kernel_sndbuf_drops) |"
    done
  done
  echo
  echo "## Phases (p50 / p99)"
  echo
  phases=(ingress events movement grid serialize assembly transport egress)
  printf '| run |'; printf ' %s |' "${phases[@]}"; echo
  printf '|---|'; printf '%s' "$(printf -- '---|%.0s' "${phases[@]}")"; echo
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      s=$dir/$id-$r/server.summary
      [ -f "$s" ] || continue
      printf '| %s #%s |' "$id" "$r"
      for p in "${phases[@]}"; do printf ' %s / %s |' "$(kv "$s" "${p}_p50_ms")" "$(kv "$s" "${p}_p99_ms")"; done
      echo
    done
  done
  echo
  echo "## Clients"
  echo
  echo "| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | near decode errors | swarm busy |"
  echo "|---|---|---|---|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      b=$dir/$id-$r/bots.summary
      s=$dir/$id-$r/server.summary
      j=$dir/$id-$r/joiners.summary
      [ -f "$b" ] || { echo "| $id #$r | failed | | | | | | | | |"; continue; }
      joined="$(kv "$b" welcomed) / $(kv "$b" started)"
      join_p99=$(kv "$b" join_p99_ms)
      if [ -f "$j" ]; then
        joined="$joined + $(kv "$j" welcomed) / $(kv "$j" started) joiners"
        join_p99="$join_p99 (joiners $(kv "$j" join_p99_ms))"
      fi
      echo "| $id #$r | $joined | $join_p99 | $(kv "$b" input_applied_p50_ms) / $(kv "$b" input_applied_p99_ms) | $(kv "$b" server_wait_p50_ms) | $(kv "$s" repeated) / $(kv "$s" frozen) | $(kv "$s" late_inputs) / $(kv "$s" discarded_inputs) | $(kv "$b" corrections) | $(kv "$b" near_decode_errors) | $(kv "$b" swarm_busy_pct)% |"
    done
  done
  profiles=$(find "$dir" -name perf.txt | sort)
  if [ -n "$profiles" ]; then
    echo
    echo "## Profiles"
    echo
    for p in $profiles; do echo "- \`${p#"$dir"/}\`"; done
  fi
} > "$dir/summary.md"

echo
echo "done: $dir/summary.md"
echo "keep it with: git add $dir && git commit -m 'Baseline: $name'"
