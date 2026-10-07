#!/usr/bin/env bash
# One comparable baseline: the scenario matrix, run the same way on any Linux
# box, with the machine's setup recorded next to the numbers.
#
#   scripts/baseline.sh [full|quick|limits|limits-quick|netem|netem-quick|fight|fight-quick] [name]
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
#   fight   combat in latency classes (~4 min): bots in 3 classes, 10 / 50 / 75 ms
#           one way (20 / 100 / 150 ms RTT), each class on its own port range
#           (lattice-bots --classes) and shaped by its own netem band. Fighters
#           aim at what they draw; the rest hold the trigger. A 1k blob immortal
#           (lattice-server --immortal: hit rates measure aim and lag
#           compensation: within the cap, alike), the same blob lethal (what
#           latency costs in a real fight: shots into the already dead), and a
#           lethal uniform 5k (load), 60 s each. summary.md gets a Fights table.
#   fight-quick   one short immortal fight of 300 in a 60 m disk
#   m3e     M3's pass bars at scale (~12 min, for bare metal): uniform 10k with
#           20% firing (and 200 aiming fighters), a 3k blob all firing (300
#           aiming), a lethal 3k blob in latency classes (20 / 100 / 150 ms
#           RTT, fairness under real load), and uniform 10k with --sockets 1 /
#           4 / 8 / 16 and with --ingress recvfrom (no fire, comparable with
#           the 2026-10-04 baselines)
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
# Fights (and m3e, which has a latency-class run): below the classes' port
# ranges (16384 and up), so the server's own port never matches a class filter.
if [[ $mode == fight* || $mode == m3e ]]; then port=${PORT:-14500}; else port=${PORT:-40500}; fi
case $mode in
  full) repeat=${REPEAT:-2}; secs=60 ;;
  quick) repeat=${REPEAT:-1}; secs=20 ;;
  limits | limits-quick) repeat=${REPEAT:-1}; secs=60 ;;
  netem) repeat=${REPEAT:-1}; secs=60 ;;
  netem-quick) repeat=${REPEAT:-1}; secs=20 ;;
  fight) repeat=${REPEAT:-1}; secs=60 ;;
  fight-quick) repeat=${REPEAT:-1}; secs=25 ;;
  m3e) repeat=${REPEAT:-1}; secs=60 ;;
  *) echo "usage: $0 [full|quick|limits|limits-quick|netem|netem-quick|fight|fight-quick|m3e] [name]" >&2; exit 2 ;;
esac

# Local netem: rerun inside a private network namespace, where we may shape
# its loopback without root and nothing outside it is affected.
if [[ $mode == netem* || $mode == fight* ]] && [ -z "${BOTS_SSH:-}" ] && [ -z "${LATTICE_NETNS:-}" ]; then
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
elif [ "$mode" = fight ]; then
  # id | scenario | bots | server args | profile | ramp | link | bot args
  classes="classes:10,50,75"
  runs=(
    "fight-blob-1k-immortal|blob|1000|--immortal|||$classes|--classes 3 --fight-every 2 --fire-share 0.3"
    "fight-blob-1k|blob|1000||||$classes|--classes 3 --fight-every 2 --fire-share 0.3"
    "fight-uniform-5k|uniform|5000||||$classes|--classes 3 --fight-every 10 --fire-share 0.2"
  )
  max_bots=5000
elif [ "$mode" = m3e ]; then
  runs=(
    "m3e-uniform-10k-fire20|uniform|10000||profile|||--fire-share 0.2 --fight-every 50"
    "m3e-blob-3k-fight|blob|3000||profile|||--fire-share 1.0 --fight-every 10"
    "m3e-blob-3k-classes|blob|3000||||classes:10,50,75|--classes 3 --fight-every 2 --fire-share 0.3"
    "m3e-uniform-10k-sockets-1|uniform|10000|--sockets 1|"
    "m3e-uniform-10k-sockets-4|uniform|10000|--sockets 4|"
    "m3e-uniform-10k-sockets-8|uniform|10000|--sockets 8|"
    "m3e-uniform-10k-sockets-16|uniform|10000|--sockets 16|"
    "m3e-uniform-10k-recvfrom|uniform|10000|--sockets 1 --ingress recvfrom|"
  )
  max_bots=10000
elif [ "$mode" = fight-quick ]; then
  runs=("fight-disk-300-immortal|disk:60|300|--immortal|||classes:10,50,75|--classes 3 --fight-every 2 --fire-share 0.3")
  max_bots=300
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
# Latency classes ("classes:10,50,75": one-way ms per class): a prio qdisc
# with a netem band per class, matched by the bots' class port ranges (port
# 16384 x (class + 1) and up: one u32 match, mask 0xc000). Traffic matching
# no class goes to the last band, unshaped.
class_qdisc() { # dev, "sport"/"dport"/"both", "10,50,75"
  local dev=$1 match=$2 ms=$3 c=0 bands map
  IFS=, read -r -a delays <<< "$ms"
  bands=$(( ${#delays[@]} + 1 ))
  map=$(for _ in $(seq 16); do printf '%d ' $(( bands - 1 )); done)
  # shellcheck disable=SC2086 # the priomap is a list
  tc qdisc add dev "$dev" root handle 1: prio bands "$bands" priomap $map
  for d in "${delays[@]}"; do
    tc qdisc add dev "$dev" parent "1:$((c + 1))" handle "$((c + 10)):" netem limit 1000000 delay "${d}ms" 1ms distribution normal
    for m in sport dport; do
      if [ "$match" = both ] || [ "$match" = "$m" ]; then
        tc filter add dev "$dev" parent 1: protocol ip prio 1 u32 match ip "$m" "$(( 16384 * (c + 1) ))" 0xc000 flowid "1:$((c + 1))"
      fi
    done
    c=$((c + 1))
  done
}

netem() { # "" (clean), netem arguments, or "classes:<ms,ms,...>"
  local args="$1"
  if [[ $args == classes:* ]]; then
    local ms=${args#classes:}
    if [ -n "$remote" ]; then
      # Bot machine: egress (bots -> server) by source port, and ingress
      # through ifb0 (server -> bots) by destination port.
      {
        echo "set -e"
        declare -f class_qdisc
        echo "tc qdisc del dev $bot_dev root 2> /dev/null || true"
        echo "tc qdisc del dev $bot_dev ingress 2> /dev/null || true"
        echo "tc qdisc del dev ifb0 root 2> /dev/null || true"
        echo "modprobe ifb numifbs=1"
        echo "ip link set ifb0 up"
        echo "class_qdisc $bot_dev sport $ms"
        echo "tc qdisc add dev $bot_dev handle ffff: ingress"
        echo "tc filter add dev $bot_dev parent ffff: protocol all prio 1 u32 match u32 0 0 action mirred egress redirect dev ifb0"
        echo "class_qdisc ifb0 dport $ms"
      } | ssh -o BatchMode=yes "$remote" "sudo bash -s"
    elif [ -n "${LATTICE_NETNS:-}" ]; then
      # Loopback carries both directions: a class's port as source (bots ->
      # server) or as destination (server -> bots).
      tc qdisc del dev lo root 2> /dev/null || true
      class_qdisc lo both "$ms"
    else
      echo "latency classes need a fight mode (local) or BOTS_SSH" >&2
      exit 1
    fi
    return
  fi
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
    IFS='|' read -r id scenario count extra prof ramp link botargs <<< "$spec"
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
      BOTS_SSH=$remote BOT_ARGS="--server $server_ip:$port $key_args $ramp_args ${botargs:-} ${BOT_ARGS:-}" \
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
  echo "## Ingress"
  echo
  echo "One receive thread per socket (\`--sockets\`). recvmmsg gathers for up to \`--rx-gather-us\` after a short batch, stopping 200 us before the next tick; busy is each thread's CPU time over the steady state (near 100%: that socket can't keep up)."
  echo
  echo "| run | ingress | sockets | in kpps | datagrams per call | receive thread busy max / mean |"
  echo "|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      s=$dir/$id-$r/server.summary
      [ -f "$s" ] || continue
      ing=$(kv "$s" ingress)
      [ "$ing" = recvmmsg ] && ing="$ing ($(kv "$s" rx_gather_us) us)"
      echo "| $id #$r | $ing | $(kv "$s" sockets) | $(kilo "$(kv "$s" in_pps)") | $(kv "$s" recv_per_call) | $(kv "$s" ingress_thread_busy_max_pct)% / $(kv "$s" ingress_thread_busy_mean_pct)% |"
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
  # Smoothness: how tracked bots drew everyone else, and the server's rewind.
  echo
  echo "## Smoothness"
  echo
  echo "Tracked bots draw a frame every tick: near players 67 ms behind the newest server step, mid and far 200 ms (lattice-bots --near-ms, --mid-ms). Shares of entity-frames interpolated / extrapolated / held (updates stopped); pops are what an arriving update moved on screen before smoothing. Rewind is applied step - the input's render step for that tier: what lag compensation would rewind a near or a mid/far target by."
  echo
  echo "| run | near % | mid % | far % | pops p99 near / mid / far (mm) | near render delay | clock snaps | rewind near p50 / p99 | rewind mid p50 / p99 |"
  echo "|---|---|---|---|---|---|---|---|---|"
  for r in $(seq 1 "$repeat"); do
    for spec in "${runs[@]}"; do
      IFS='|' read -r id _ _ _ _ <<< "$spec"
      b=$dir/$id-$r/bots.summary
      s=$dir/$id-$r/server.summary
      [ -f "$b" ] || continue
      tier() { echo "$(kv "$b" "${1}_interpolated_pct") / $(kv "$b" "${1}_extrapolated_pct") / $(kv "$b" "${1}_held_pct")"; }
      echo "| $id #$r | $(tier near) | $(tier mid) | $(tier far) | $(kv "$b" near_pop_p99_mm) / $(kv "$b" mid_pop_p99_mm) / $(kv "$b" far_pop_p99_mm) | $(kv "$b" render_delay_ms) | $(kv "$b" render_snaps) | $(kv "$s" rewind_near_p50_ms) / $(kv "$s" rewind_near_p99_ms) | $(kv "$s" rewind_mid_p50_ms) / $(kv "$s" rewind_mid_p99_ms) |"
    done
  done

  # Fights: hit rate per latency class (fight and m3e modes).
  if [[ $mode == fight* || $mode == m3e ]]; then
    echo
    echo "## Fights"
    echo
    echo "Fighters aim at what they draw (lattice-bots --fight-every, the same aim error for all); classes are one-way delays on their own port ranges. Within the rewind cap (300 ms near, 367 ms mid) hit rates must match; past it they drop."
    echo
    echo "| run | class | link (one way) | fighters | RTT | shots | hits | hit % | kills |"
    echo "|---|---|---|---|---|---|---|---|---|"
    for r in $(seq 1 "$repeat"); do
      for spec in "${runs[@]}"; do
        IFS='|' read -r id _ _ _ _ _ link _ <<< "$spec"
        b=$dir/$id-$r/bots.summary
        [ -f "$b" ] || continue
        IFS=, read -r -a delays <<< "${link#classes:}"
        for c in 0 1 2; do
          [ "$(kv "$b" "class${c}_fighters")" = - ] && continue
          echo "| $id #$r | $c | ${delays[$c]:-?} ms | $(kv "$b" "class${c}_fighters") | $(kv "$b" "class${c}_rtt_ms") | $(kv "$b" "class${c}_shots") | $(kv "$b" "class${c}_hits") | $(kv "$b" "class${c}_hit_pct") | $(kv "$b" "class${c}_kills") |"
        done
      done
    done
    echo
    echo "| run | shots | hits head / body | after cover | too late | rewinds capped | kills | shots phase p50 / p99 | tick p50 / p99 | corrections |"
    echo "|---|---|---|---|---|---|---|---|---|---|"
    for r in $(seq 1 "$repeat"); do
      for spec in "${runs[@]}"; do
        IFS='|' read -r id _ <<< "$spec"
        s=$dir/$id-$r/server.summary
        b=$dir/$id-$r/bots.summary
        [ -f "$s" ] || continue
        echo "| $id #$r | $(kv "$s" shots) | $(kv "$s" hits_head) / $(kv "$s" hits_body) | $(kv "$s" hits_after_cover) | $(kv "$s" hits_too_late) | $(kv "$s" rewinds_capped) | $(kv "$s" kills) | $(kv "$s" shots_p50_ms) / $(kv "$s" shots_p99_ms) | $(kv "$s" tick_p50_ms) / $(kv "$s" tick_p99_ms) | $(kv "$b" corrections) |"
      done
    done
  fi

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
