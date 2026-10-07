#!/usr/bin/env bash
# Ends a session from scripts/aws-up.sh: copies baselines/ back from the server
# box, terminates every instance tagged Project=lattice-net in the region, waits
# until they're gone, and lists what's left of ours (must be nothing).
#
#   scripts/aws-down.sh [--no-fetch]
#
# It only ever terminates instances tagged Project=lattice-net: those
# aws-up.sh launched. The security group, key pair and placement group are
# free and stay for the next session.
set -euo pipefail
cd "$(dirname "$0")/.."

state=.cloud/session.env
region=${REGION:-eu-central-1}
if [ -s "$state" ]; then
  # shellcheck source=/dev/null
  . "$state"
  [ "${PROVIDER:-scaleway}" = aws ] || { echo "this is a Scaleway session: end it with scripts/cloud-down.sh" >&2; exit 1; }
  region=${REGION:-$region}
  if [ "${1:-}" != --no-fetch ] && [ -n "${SRV_PUB:-}" ]; then
    rsync -a -e "ssh -F .cloud/ssh_config -o BatchMode=yes -o ConnectTimeout=10" \
      lattice-srv:lattice-net/baselines/ baselines/ 2> /dev/null &&
      echo "copied back baselines/ from lattice-srv" ||
      echo "couldn't copy baselines/ back (server not reachable); terminating anyway"
  fi
fi
export AWS_DEFAULT_REGION=$region AWS_PAGER=""

ours() { # "id name state" for every live instance tagged Project=lattice-net
  aws ec2 describe-instances --filters Name=tag:Project,Values=lattice-net \
    Name=instance-state-name,Values=pending,running,stopping,stopped,shutting-down | python3 -c '
import json, sys
for r in json.load(sys.stdin)["Reservations"]:
    for i in r["Instances"]:
        name = next((t["Value"] for t in i.get("Tags", []) if t["Key"] == "Name"), "?")
        print(i["InstanceId"], name, i["State"]["Name"])'
}
ids=$(ours | awk '$3 != "shutting-down" {print $1}')
if [ -n "$ids" ]; then
  echo "terminating: $(echo "$ids" | tr '\n' ' ')"
  # shellcheck disable=SC2086 # a list of ids
  aws ec2 terminate-instances --instance-ids $ids > /dev/null
fi
all=$(ours | awk '{print $1}')
if [ -n "$all" ]; then
  # shellcheck disable=SC2086
  aws ec2 wait instance-terminated --instance-ids $all
fi
# Public IPs get reused by the next instances, with new host keys.
rm -f "$state" .cloud/known_hosts .cloud/known_hosts.old .cloud/ssh_config
echo "== lattice-net instances in $region now (should be none):"
left=$(ours)
echo "${left:-none}"
if [ -n "$left" ]; then
  echo "WARNING: lattice-net instances are still listed (and billing): check the console" >&2
  exit 1
fi
