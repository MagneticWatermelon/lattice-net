#!/usr/bin/env bash
# A small human playtest: the server (and bots to fight) on one rented Linux
# VM with a public address, invites for the players, their session log, and a
# zip of the Windows game to send them. Run from this machine; HOST is
# user@address, reachable with your ssh key: root, or a user with passwordless
# sudo (Ubuntu or Debian). The server runs as its own system user, lattice,
# with its key and logs in /var/lib/lattice-playtest.
#
#   scripts/playtest.sh deploy HOST         copy this repo, build, make a token key,
#                                           install the services (systemd)
#   scripts/playtest.sh start HOST [BOTS]   start the server and BOTS fighting bots [40]
#   scripts/playtest.sh stop HOST
#   scripts/playtest.sh status HOST         services, the server's last report, players
#   scripts/playtest.sh invites HOST NAME...    an invite per player, minted on HOST
#                                           (where the key is) into playtest/invites/
#   scripts/playtest.sh sessions HOST       fetch the session log into playtest/ and
#                                           sum it up per player
#   scripts/playtest.sh pack                the Windows game, a launcher and instructions
#                                           as playtest/lattice-playtest.zip
#
# Players connect to the address in HOST (or ADDR=...) on UDP port PORT
# (40000): open it in the provider's firewall too, if the VM has one.
#
# The token key is made on HOST and never leaves it, and the server never takes
# the public dev key. Invites are private (playtest/ is gitignored): each holds
# its player's connect tokens, one per launch of the game. To revoke them all,
# delete /var/lib/lattice-playtest/token.key on HOST and deploy again.
#
# What the playtest is for: whether real players on real links see what the
# bots do (smoothness, corrections, hit registration), and calibrating the
# render floor's flag on honest players (sessions shows any flagged).
set -euo pipefail
cd "$(dirname "$0")/.."

cmd=${1:-}
host=${2:-}
port=${PORT:-40000}
addr=${ADDR:-${host#*@}}
dir=playtest
data=/var/lib/lattice-playtest
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }
need_host() { [ -n "$host" ] || { echo "usage: $0 $cmd user@address ..." >&2; exit 2; }; }
on() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$host" "$@"; }
# Files only the lattice user may read, copied back.
fetch() { rsync -a --rsync-path="sudo rsync" -e "ssh -o BatchMode=yes" "$host:$1" "$2"; }

# The setup on HOST: a system user, the binaries, a token key, the services.
# Takes the port; runs as the ssh user, with sudo.
remote_setup='
set -euo pipefail
port=$1
data=/var/lib/lattice-playtest
# The server faces the internet: it runs as its own user, which owns only
# the key and the logs.
id lattice > /dev/null 2>&1 || sudo useradd --system --home-dir "$data" --shell /usr/sbin/nologin lattice
sudo install -d -o lattice -g lattice -m 750 "$data"
bin=~/lattice-net/target/release
sudo install -m 755 "$bin/lattice-server" "$bin/lattice-bots" "$bin/lattice-invite" /usr/local/bin/
if ! sudo test -s "$data/token.key"; then
  sudo -u lattice sh -c "umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d \" \n\" > $data/token.key"
  echo "made a new token key (invites from an earlier one no longer work)"
fi
sudo test -f "$data/bots.env" || echo BOTS=40 | sudo -u lattice tee "$data/bots.env" > /dev/null
sandbox="NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=$data"
sudo tee /etc/systemd/system/lattice-playtest.service > /dev/null << UNIT
[Unit]
Description=lattice playtest server
After=network-online.target

[Service]
User=lattice
WorkingDirectory=$data
ExecStart=/usr/local/bin/lattice-server --bind 0.0.0.0:$port --token-key @$data/token.key --session-log $data/sessions.jsonl --spawn disk:250 --max-clients 500 --report 60
Restart=on-failure
StandardOutput=append:$data/server.log
StandardError=append:$data/server.log
$sandbox

[Install]
WantedBy=multi-user.target
UNIT
sudo tee /etc/systemd/system/lattice-playtest-bots.service > /dev/null << UNIT
[Unit]
Description=lattice playtest bots
After=lattice-playtest.service
Requires=lattice-playtest.service

[Service]
User=lattice
WorkingDirectory=$data
EnvironmentFile=$data/bots.env
# Half of them fight (aim at the enemies they see). They leave after two
# weeks, and come back.
ExecStart=/usr/local/bin/lattice-bots --server 127.0.0.1:$port --token-key @$data/token.key --count \${BOTS} --fight-every 2 --threads 2 --duration 1209600 --report 60
Restart=always
RestartSec=5
StandardOutput=append:$data/bots.log
StandardError=append:$data/bots.log
$sandbox
UNIT
sudo systemctl daemon-reload
# A host firewall, if one is on: let the players in.
if command -v ufw > /dev/null && sudo ufw status | grep -q "Status: active"; then
  sudo ufw allow "$port/udp" > /dev/null && echo "ufw: allowed $port/udp"
fi
'

# Sums the session log up per player (names from the invites' users.txt).
summarize='
import json, sys
from collections import defaultdict
names = {}
try:
    for line in open(sys.argv[2]):
        if line.strip() and not line.startswith("#"):
            uid, name = line.split(maxsplit=1)
            names[int(uid)] = name.strip()
except FileNotFoundError:
    pass
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
# A session ends with a "left" or "open" line; "flagged" lines repeat its figures so far.
ends = [r for r in rows if r["event"] in ("left", "open")]
flags = [r for r in rows if r["event"] == "flagged"]
keys = ("secs", "inputs", "stand_ins", "late", "shots", "held", "trimmed", "refused", "hits", "kills", "deaths", "windows", "windows_flagged")
per = defaultdict(lambda: defaultdict(float))
for r in ends:
    user = r["user"]
    who = names.get(user) or ("bots" if user >= 1000000 else "user %d" % user)
    p = per[who]
    p["sessions"] += 1
    for k in keys:
        p[k] += r[k]
    p["rtt_sum"] += r["rtt_mean_ms"] * r["inputs"]
    p["rtt_max"] = max(p["rtt_max"], r["rtt_max_ms"])
print("%-14s %8s %8s %7s %6s %6s %6s %5s %7s %10s %7s %7s %9s" % ("player", "sessions", "minutes", "shots", "hit%", "kills", "deaths", "held", "trimmed", "stand-ins%", "rtt ms", "rtt max", "flagged"))
for who, p in sorted(per.items(), key=lambda kv: (kv[0] == "bots", kv[0])):
    hit = 100 * p["hits"] / p["shots"] if p["shots"] else 0
    stand_ins = 100 * p["stand_ins"] / max(1, p["inputs"] + p["stand_ins"])
    rtt = p["rtt_sum"] / max(1, p["inputs"])
    flagged = "%d/%d" % (p["windows_flagged"], p["windows"])
    print("%-14s %8d %8.1f %7d %6.1f %6d %6d %5d %7d %10.2f %7.1f %7.1f %9s" % (who, p["sessions"], p["secs"] / 60, p["shots"], hit, p["kills"], p["deaths"], p["held"], p["trimmed"], stand_ins, rtt, p["rtt_max"], flagged))
if flags:
    print()
    print("flagged (render floor allowances spent on shots):")
    for r in flags:
        w = r["window"]
        print("  %s: window excess %.2f steps, z %.1f, after %d shots" % (names.get(r["user"], "user %d" % r["user"]), w["excess"], w["z"], r["shots"]))
'

readme='lattice playtest
================

1. Unzip this folder anywhere.
2. Save the invite you were sent next to lattice-client.exe as invite.txt.
   It is yours alone: whoever has it can play as you.
3. Double-click play.bat. (Windows may warn that the app is unrecognized:
   More info, then Run anyway.)

Click in the window to grab the mouse; Esc lets it go.

  WASD, Shift, Space    move, sprint, jump
  left mouse (held)     fire
  right mouse (held)    aim down the sights
  V                     first person, chase camera, free flight
  N                     net graph (frame time, ping, loss, smoothness)

Each time you quit, the game adds a summary of your session (frame times,
ping, corrections, how smooth other players looked, hits) to
lattice-report.txt in this folder. Please send that file back, with anything
you noticed: stutters, players jumping around, your own movement snapping
back, shots that should have hit or should not have, anything that felt off.'

case $cmd in
deploy)
  need_host
  say "copying the repo to $host"
  rsync -a --delete -e "ssh -o BatchMode=yes" --exclude target --exclude results --exclude .cloud \
    --exclude client/assets --exclude /meshy_output --exclude /playtest --exclude client/target ./ "$host:lattice-net/"
  say "toolchain and build (a few minutes the first time)"
  on "sudo DEBIAN_FRONTEND=noninteractive apt-get -qq update && sudo DEBIAN_FRONTEND=noninteractive apt-get -qq install -y build-essential rsync > /dev/null"
  on "command -v cargo > /dev/null || [ -x ~/.cargo/bin/cargo ] || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal > /dev/null"
  on "source ~/.cargo/env && cd lattice-net && cargo build --release -q -p lattice-sim --bin lattice-server --bin lattice-bots --bin lattice-invite"
  say "a system user, the token key and the services"
  on bash -s "$port" <<< "$remote_setup"
  say "deployed; next: $0 start $host, then $0 invites $host NAME..."
  ;;
start)
  need_host
  bots=${3:-40}
  on "echo BOTS=$bots | sudo -u lattice tee $data/bots.env > /dev/null && sudo systemctl enable -q --now lattice-playtest"
  if [ "$bots" -gt 0 ]; then
    on "sudo systemctl enable -q lattice-playtest-bots && sudo systemctl restart lattice-playtest-bots"
  else
    on "sudo systemctl disable -q --now lattice-playtest-bots || true"
  fi
  sleep 3
  on "systemctl is-active lattice-playtest lattice-playtest-bots || true"
  say "players connect to $addr:$port (UDP)"
  ;;
stop)
  need_host
  on "sudo systemctl disable -q --now lattice-playtest-bots lattice-playtest || true"
  on "systemctl is-active lattice-playtest lattice-playtest-bots || true"
  ;;
status)
  need_host
  on "systemctl is-active lattice-playtest lattice-playtest-bots || true"
  on "sudo tail -n 4 $data/server.log 2> /dev/null || true; echo; sudo cat $data/sessions.jsonl 2> /dev/null | wc -l | sed 's/$/ session log lines/'"
  ;;
invites)
  need_host
  shift 2
  [ $# -gt 0 ] || { echo "usage: $0 invites HOST NAME..." >&2; exit 2; }
  on "cd $data && sudo -u lattice /usr/local/bin/lattice-invite --token-key @token.key --server $addr:$port --out invites $*"
  mkdir -p "$dir/invites"
  fetch "$data/invites/" "$dir/invites/"
  chmod 600 "$dir"/invites/*.txt
  say "invites in $dir/invites/: send each player theirs, privately, with the zip ($0 pack)"
  ;;
sessions)
  need_host
  mkdir -p "$dir/invites"
  fetch "$data/sessions.jsonl" "$dir/sessions.jsonl"
  fetch "$data/invites/users.txt" "$dir/invites/users.txt" 2> /dev/null || true
  python3 -c "$summarize" "$dir/sessions.jsonl" "$dir/invites/users.txt"
  ;;
pack)
  out=$dir/win/lattice-playtest
  rm -rf "$dir/win"
  mkdir -p "$out"
  say "building the Windows game"
  # (client-windows.sh builds in client/: give it a full path.)
  scripts/client-windows.sh "$PWD/$out" > /dev/null
  printf '@echo off\r\ncd /d "%%~dp0"\r\nif not exist invite.txt (\r\n  echo Put the invite you were sent next to lattice-client.exe, named invite.txt.\r\n  pause\r\n  exit /b 1\r\n)\r\nlattice-client.exe --invite invite.txt\r\nif errorlevel 1 pause\r\n' > "$out/play.bat"
  printf '%s\n' "$readme" | sed 's/$/\r/' > "$out/README.txt"
  (cd "$dir/win" && python3 -m zipfile -c ../lattice-playtest.zip lattice-playtest)
  say "$dir/lattice-playtest.zip ($(du -h "$dir/lattice-playtest.zip" | cut -f1)): send it with each player's invite"
  ;;
*)
  sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
