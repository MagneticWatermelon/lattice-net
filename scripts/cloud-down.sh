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
while read -r zone id name; do
  [ -n "$id" ] || continue
  echo "deleting $name $id ($zone)"
  scw baremetal server delete "$id" zone="$zone" > /dev/null
done < <(ours)

for _ in $(seq 60); do
  [ -z "$(ours)" ] && break
  sleep 5
done
rm -f "$state"
echo "== servers in the account now (should be none of ours):"
scw baremetal server list zone=all
if [ -n "$(ours)" ]; then
  echo "WARNING: lattice-net servers are still listed (and billing): check the console" >&2
  exit 1
fi
