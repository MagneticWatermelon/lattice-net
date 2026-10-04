#!/usr/bin/env bash
# One comparable baseline: the scenario matrix, run the same way on any Linux
# box, with the machine's setup recorded next to the numbers.
#
#   scripts/baseline.sh [full|quick|limits|limits-quick] [name]
#
#   full    8 scenarios x REPEAT (2) runs of 60 s, interleaved: ~16 min
#   quick   2 small scenarios x 1 run of 20 s: checks the harness, not the machine
#   limits  ramps to find where it breaks: players join at a steady rate up to a
#           peak, then hold 40 s, and summary.md shows the cost at each step
#           (~18 min): everyone in a 25 m disk to 10k (ladder off: raw cost; and
#           on), a 200 m disk to 10k, and uniform to 20k
#   limits-quick  one small ramp: checks the harness
#   netem   the network matrix (~13 min): uniform 1k and a 1k blob under six
#           network profiles (clean, LAN, typical, far, lossy, jittery), for the
#           spare input, input -> applied, stand-ins and prediction under
#           latency, jitter and loss. Locally it runs in a private network
#           namespace (unshare -rn: no sudo, nothing else on the machine is
#           shaped) with netem on its loopback, which delays each direction once;
#           with BOTS_SSH, only the bot machine is shaped (sudo tc): netem on its
#           egress delays bots -> server, and its ingress is redirected through
#           an ifb device with the same netem for server -> bots. Never on the
#           server: at 10k a netem queue on its interface throttled its sends.
#           NETEM_BOTS=10000 runs it at full load instead of 1k.
#   netem-quick   one short run on the typical profile: checks the harness
#
# Writes baselines/<date>-<name>/ (name defaults to the host name):
#   env.txt      the machine and the preflight checks (scripts/preflight.sh)
#   summary.md   server, phase and client tables, one row per run
#   <run>/       each run's logs, per-window CSV and key=value summaries
#
# Server and bots share this machine and talk over loopback, like scripts/m1.sh,
# unless BOTS_SSH names a second machine (see Two machines below).
# Env: REPEAT, PORT (40500), PROFILE=1 (perf profiles of the first 10k and GSO
# blob runs), FORCE=1 (run despite failed checks), SERVER_THREADS, BOT_THREADS,
# SERVER_ARGS (extra lattice-server flags for every run, e.g. "--sockets 8").
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
  limits | limits-quick) repeat=${REPEAT:-1}; secs=60 ;;
  netem) repeat=${REPEAT:-1}; secs=60 ;;
  netem-quick) repeat=${REPEAT:-1}; secs=20 ;;
  *) echo "usage: $0 [full|quick|limits|limits-quick|netem|netem-quick] [name]" >&2; exit 2 ;;
esac

# Local netem: rerun inside a private network namespace, where we may shape
# its loopback without root and nothing outside it is affected.
if [[ $mode == netem* ]] && [ -z "${BOTS_SSH:-}" ] && [ -z "${LATTICE_NETNS:-}" ]; then
  exec unshare -rn env LATTICE_NETNS=1 "$0" "$@"
fi
[ -n "${LATTICE_NETNS:-}" ] && ip link set lo up

# id | scenario | bots | extra lattice-server args | profile it | joins per second | netem
# A run with a join rate is a ramp: bots join at that rate, then hold 40 s.
# A run with netem arguments runs with them on the link (one-way delay; see above).
netem_profiles=(
  "clean|"
  "lan|delay 15ms 2ms distribution normal"
  "typical|delay 40ms 5ms distribution normal loss 0.5%"
  "far|delay 75ms 10ms distribution normal loss 1%"
  "lossy|delay 40ms 5ms distribution normal loss 5%"
  "jittery|delay 40ms 20ms distribution normal"
)
if [ "$mode" = netem ]; then
  # NETEM_BOTS (1000) sets the load: 10000 checks behavior under the full load.
  nb=${NETEM_BOTS:-1000}
  tag=$(( nb / 1000 ))k
  runs=()
  for np in "${netem_profiles[@]}"; do
    IFS='|' read -r pname pargs <<< "$np"
    runs+=("$pname-uniform-$tag|uniform|$nb||||$pargs" "$pname-blob-$tag|blob|$nb||||$pargs")
  done
  max_bots=$nb
elif [ "$mode" = netem-quick ]; then
  runs=("typical-uniform-300|uniform|300||||delay 40ms 5ms distribution normal loss 0.5%")
  max_bots=300
elif [ "$mode" = limits ]; then
  runs=(
    "pile-25m-10k-noladder|disk:25|10000|--ladder off|profile|50"
    "pile-25m-10k|disk:25|10000||profile|50"
    "disk-200m-10k-noladder|disk:200|10000|--ladder off||50"
    "uniform-20k-noladder|uniform|20000|--ladder off --max-clients 20000||100"
  )
  max_bots=20000
elif [ "$mode" = limits-quick ]; then
  runs=("pile-25m-1500-noladder|disk:25|1500|--ladder off|profile|100")
  max_bots=1500
elif [ "$mode" = full ]; then
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

# netem on the link: loopback here, or both machines' interfaces between them.
# Its queue limit is raised: the default 1,000 packets would itself drop at
# these rates (packets per second x the delay).
if [ -n "$remote" ]; then
  bot_dev=$(ssh -o BatchMode=yes "$remote" "ip -o route get $server_ip" | awk '{for (i = 1; i < NF; i++) if ($i == "dev") print $(i + 1)}')
fi
netem() { # "" (clean) or netem arguments
  local args="$1"
  if [ -n "$remote" ]; then
    # The bot machine only: egress for bots -> server, ingress via ifb0 for
    # server -> bots.
    ssh -o BatchMode=yes "$remote" "sudo bash -s" << EOF
tc qdisc del dev $bot_dev root 2> /dev/null
tc qdisc del dev $bot_dev ingress 2> /dev/null
tc qdisc del dev ifb0 root 2> /dev/null
if [ -n "$args" ]; then
  set -e
  modprobe ifb numifbs=1
  ip link set ifb0 up
  tc qdisc add dev $bot_dev root netem limit 1000000 $args
  tc qdisc add dev $bot_dev handle ffff: ingress
  tc filter add dev $bot_dev parent ffff: protocol all prio 1 u32 match u32 0 0 action mirred egress redirect dev ifb0
  tc qdisc add dev ifb0 root netem limit 1000000 $args
fi
true
EOF
  elif [ -n "${LATTICE_NETNS:-}" ]; then
    if [ -z "$args" ]; then
      tc qdisc del dev lo root 2> /dev/null || true
    else
      tc qdisc replace dev lo root netem limit 1000000 $args
    fi
  elif [ -n "$args" ]; then
    echo "netem needs a netem mode (local) or BOTS_SSH" >&2
    exit 1
  fi
}
# shellcheck disable=SC2329 # used by the trap
clear_netem() { netem "" > /dev/null 2>&1 || true; }
trap clear_netem EXIT

cores=$(nproc)
if [ -n "$remote" ]; then
  # Alone on the machine: the server takes a thread per physical core (SMT
  # siblings only add overhead), the bots half of their machine's threads.
  server_threads=${SERVER_THREADS:-$(lscpu -p=core,socket | grep -v '^#' | sort -u | wc -l)}
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
    IFS='|' read -r id scenario count extra prof ramp link <<< "$spec"
    n=$((n + 1))
    netem "$link"
    run_secs=$secs
    ramp_args=
    profile_delay=15
    if [ -n "$ramp" ]; then
      run_secs=$(( count / ramp + 40 ))
      ramp_args="--ramp $ramp"
      profile_delay=$(( run_secs - 35 )) # during the hold at the peak
    fi
    out=$dir/$id-$r
    mkdir -p "$out"
    printf '[%s] %d/%d %s run %d\n' "$(date +%T)" "$n" "$total" "$id" "$r"
    profile=
    [ -n "${PROFILE:-}" ] && [ -n "$prof" ] && [ "$r" = 1 ] && profile=1
    # shellcheck disable=SC2086 # $extra is a list of server flags
    # shellcheck disable=SC2086 # $key_args is empty or a flag and its value
    if ! OUT=$out PROFILE=$profile PROFILE_DELAY=$profile_delay SERVER_THREADS=$server_threads BOT_THREADS=$bot_threads \
      BOTS_SSH=$remote BOT_ARGS="--server $server_ip:$port $key_args $ramp_args ${BOT_ARGS:-}" \
      scripts/m1.sh "$scenario" "$count" "$run_secs" --bind "$server_ip:$port" $key_args $extra ${SERVER_ARGS:-} > "$out/console.log" 2>&1; then
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
  phases=(ingress events movement grid separate serialize assembly transport egress)
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
  echo "## Phase breakdown (p50)"
  echo
  echo "For each phase split by shard: **wall / longest shard task / total work ÷ threads**, in ms. With perfect scheduling a phase would take max(longest, work ÷ threads); **overhead** sums wall minus that over the four phases (rayon dispatch, waiting, imbalance). Serial is events + grid + history, which run on one thread."
  echo
  echo "| run | threads | tick | ingress | assembly | transport | egress | overhead | serial |"
  echo "|---|---|---|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      s=$dir/$id-$r/server.summary
      [ -f "$s" ] || continue
      th=$(kv "$s" threads)
      cell() { awk -v w="$(kv "$s" "${1}_p50_ms")" -v l="$(kv "$s" "${1}_longest_p50_ms")" -v k="$(kv "$s" "${1}_work_p50_ms")" -v t="$th" \
        'BEGIN {if (l == "-") print w; else printf "%s / %s / %.2f", w, l, k / t}'; }
      serial=$(awk -v a="$(kv "$s" events_p50_ms)" -v b="$(kv "$s" grid_p50_ms)" -v c="$(kv "$s" history_p50_ms)" 'BEGIN {printf "%.2f", a + b + c}')
      over=0
      for p in ingress assembly transport egress; do
        over=$(awk -v o="$over" -v w="$(kv "$s" "${p}_p50_ms")" -v l="$(kv "$s" "${p}_longest_p50_ms")" -v k="$(kv "$s" "${p}_work_p50_ms")" -v t="$th" \
          'BEGIN {if (l == "-") {print o; exit} i = (l > k / t) ? l : k / t; printf "%.2f", o + w - i}')
      done
      echo "| $id #$r | $th | $(kv "$s" tick_p50_ms) | $(cell ingress) | $(cell assembly) | $(cell transport) | $(cell egress) | $over | $serial |"
    done
  done
  echo
  echo "## Clients"
  echo
  echo "| run | welcomed / started | join p99 | input -> applied p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections | push corrections | near decode errors | swarm busy |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      b=$dir/$id-$r/bots.summary
      s=$dir/$id-$r/server.summary
      j=$dir/$id-$r/joiners.summary
      [ -f "$b" ] || { echo "| $id #$r | failed | | | | | | | | | |"; continue; }
      joined="$(kv "$b" welcomed) / $(kv "$b" started)"
      join_p99=$(kv "$b" join_p99_ms)
      if [ -f "$j" ]; then
        joined="$joined + $(kv "$j" welcomed) / $(kv "$j" started) joiners"
        join_p99="$join_p99 (joiners $(kv "$j" join_p99_ms))"
      fi
      echo "| $id #$r | $joined | $join_p99 | $(kv "$b" input_applied_p50_ms) / $(kv "$b" input_applied_p99_ms) | $(kv "$b" server_wait_p50_ms) | $(kv "$s" repeated) / $(kv "$s" frozen) | $(kv "$s" late_inputs) / $(kv "$s" discarded_inputs) | $(kv "$b" corrections) | $(kv "$b" push_corrections) | $(kv "$b" near_decode_errors) | $(kv "$b" swarm_busy_pct)% |"
    done
  done
  # Network: how play holds up on each link (netem modes).
  if [[ $mode == netem* ]]; then
    echo
    echo "## Network"
    echo
    echo "netem delays each direction once, so the round trip is about twice the delay. Times in ms."
    echo
    echo "| run | link (one way) | input -> applied p50 / p99 | round trip p50 / p99 | server wait p50 | stand-ins repeated / frozen | late / discarded inputs | corrections (per bot-minute) | near decode errors | resyncs | input clock extra / skipped |"
    echo "|---|---|---|---|---|---|---|---|---|---|---|"
    for r in $(seq 1 "$repeat"); do
      for spec in "${runs[@]}"; do
        IFS='|' read -r id _ _ _ _ _ link <<< "$spec"
        b=$dir/$id-$r/bots.summary
        s=$dir/$id-$r/server.summary
        [ -f "$b" ] || { echo "| $id #$r | failed | | | | | | | | | |"; continue; }
        echo "| $id #$r | ${link:-clean} | $(kv "$b" input_applied_p50_ms) / $(kv "$b" input_applied_p99_ms) | $(kv "$b" round_trip_p50_ms) / $(kv "$b" round_trip_p99_ms) | $(kv "$b" server_wait_p50_ms) | $(kv "$s" repeated) / $(kv "$s" frozen) | $(kv "$s" late_inputs) / $(kv "$s" discarded_inputs) | $(kv "$b" corrections) ($(kv "$b" corrections_per_bot_minute)) | $(kv "$b" near_decode_errors) | $(kv "$b" resyncs) | $(kv "$b" clock_extra) / $(kv "$b" clock_skipped) |"
      done
    done
  fi

  # Ramps: the cost at each step of players, from the per-window CSV.
  ramps=0
  for spec in "${runs[@]}"; do
    IFS='|' read -r id _ count _ _ ramp _ <<< "$spec"
    [ -n "$ramp" ] || continue
    for r in $(seq 1 "$repeat"); do
      csv=$dir/$id-$r/server.csv
      [ -f "$csv" ] || continue
      if [ "$ramps" = 0 ]; then
        echo
        echo "## Ramps"
        echo
        echo "Players join at a steady rate, then hold. One row per step of players (the first 5 s window that reaches it). Times in ms; phases are p50."
        ramps=1
      fi
      echo
      echo "### $id #$r ($count players, $ramp joins/s)"
      echo
      awk -F, -v count="$count" '
        NR == 1 { for (i = 1; i <= NF; i++) col[$i] = i; next }
        function ms(name) { return sprintf("%.1f", $col[name] / 1000) }
        $col["clients"] > 0 {
          c = $col["clients"]; lvl = $col["level"]; hz = $col["tick_hz"]
          period = 1000 / hz
          if (!over && $col["tick_p99_us"] / 1000 > period) { over = c; over_hz = hz }
          if (!left && lvl > 0) left = c
          if (c >= next_step) {
            rows = rows sprintf("| %s | L%s (%s Hz) | %s / %s | %s | %s | %s | %s | %s |\n", c, lvl, hz, ms("tick_p50_us"), ms("tick_p99_us"), ms("ingress_p50_us"), ms("events_p50_us"), ms("assembly_p50_us"), ms("transport_p50_us"), ms("egress_p50_us"))
            while (next_step <= c) next_step += step
          }
          if (c >= peak) { peak = c; peak_lvl = lvl; peak_p99 = ms("tick_p99_us"); peak_hz = hz }
        }
        BEGIN { step = (count > 10000) ? 2000 : 1000; next_step = step }
        END {
          print "| players | level | tick p50 / p99 | ingress | events | assembly | transport | egress |"
          print "|---|---|---|---|---|---|---|---|"
          printf "%s", rows
          print ""
          if (over) printf "- **Over budget:** tick p99 first exceeded its period (%.1f ms at %s Hz) with %s players.\n", 1000 / over_hz, over_hz, over
          else printf "- **Within budget** all the way to %s players (p99 %s ms at the peak).\n", peak, peak_p99
          if (left) printf "- **The ladder** left level 0 at %s players and was at level %s at the peak.\n", left, peak_lvl
        }' "$csv"
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
