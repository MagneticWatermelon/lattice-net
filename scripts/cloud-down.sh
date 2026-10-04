#!/usr/bin/env bash
# Ends a session from scripts/cloud-up.sh: copies baselines/ back from the
# server box, deletes every Elastic Metal server tagged lattice-net (in the
# session's zone and any other), and shows the server list, which must be empty.
#
#   scripts/cloud-down.sh [--no-fetch]
#
# It only ever deletes servers tagged lattice-net: those cloud-up.sh created.
set -euo pipefail
cd "$(dirname "$0")/.."

state=.cloud/session.env
if [ -s "$state" ]; then
  # shellcheck source=/dev/null
  . "$state"
  if [ "${1:-}" != --no-fetch ] && [ -n "${SRV_PUB:-}" ]; then
    rsync -a -e "ssh -F .cloud/ssh_config -o BatchMode=yes -o ConnectTimeout=10" \
      lattice-srv:lattice-net/baselines/ baselines/ 2> /dev/null &&
      echo "copied back baselines/ from lattice-srv" ||
      echo "couldn't copy baselines/ back (server not reachable); deleting anyway"
  fi
fi

ours() { # "zone id name" for every server tagged lattice-net
  scw baremetal server list zone=all tags.0=lattice-net -o json | python3 -c '
import json, sys
for s in json.load(sys.stdin):
    if "lattice-net" in (s.get("tags") or []):
        print(s["zone"], s["id"], s["name"])'
}
# Detach from Private Networks first: deleting a server that's still
# attached left its attachment (and private IPs) stuck on the network, which
# then made later attaches fail with HTTP 500 (2026-10-04).
detach() { # zone server-id
  scw baremetal private-network list server-id="$2" zone="$1" -o json | python3 -c '
import json, sys
for p in json.load(sys.stdin):
    print(p["private_network_id"])' | while read -r pn; do
    scw baremetal private-network delete server-id="$2" private-network-id="$pn" zone="$1" > /dev/null 2>&1 || true
  done
  for _ in $(seq 24); do
    [ "$(scw baremetal private-network list server-id="$2" zone="$1" -o json)" = "[]" ] && return
    sleep 5
  done
  echo "  $2 still lists a Private Network attachment; deleting anyway" >&2
}
while read -r zone id name; do
  [ -n "$id" ] || continue
  echo "detaching and deleting $name $id ($zone)"
  detach "$zone" "$id"
  scw baremetal server delete "$id" zone="$zone" > /dev/null
done < <(ours)

for _ in $(seq 60); do
  [ -z "$(ours)" ] && break
  sleep 5
done
# Public IPs get reused by the next servers, with new host keys.
rm -f "$state" .cloud/known_hosts .cloud/known_hosts.old .cloud/ssh_config
echo "== servers in the account now (should be none of ours):"
scw baremetal server list zone=all
if [ -n "$(ours)" ]; then
  echo "WARNING: lattice-net servers are still listed (and billing): check the console" >&2
  exit 1
fi
