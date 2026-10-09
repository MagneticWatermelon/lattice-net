#!/usr/bin/env bash
# The playtest server on an hourly AWS instance, for a test session: up makes
# it and deploys the server with bots, down deletes everything it made. Nothing
# is left between sessions (no fixed address: invites are minted per session).
#
#   scripts/playtest-aws.sh up [BOTS]   key pair, firewall group, a c7a.large in
#                                       eu-central-1, then playtest.sh deploy + start
#                                       [40 bots]; points the ssh alias lattice-vm at it
#   scripts/playtest-aws.sh status      what's tagged Project=lattice-playtest
#   scripts/playtest-aws.sh down        terminates the instance (its disk goes with it),
#                                       deletes the firewall group and the key pair
#
# Then: ADDR=<the address up prints> scripts/playtest.sh invites lattice-vm NAME...
#
# - The firewall lets UDP 40000 in from anywhere and ssh only from this
#   machine's public address.
# - The instance terminates itself 3 hours after boot (shutdown -h +180,
#   shutdown behavior terminate) in case a session is forgotten: always run
#   down, and check with status.
# - The tag is Project=lattice-playtest, not lattice-net, so aws-down.sh
#   (the test rig) never touches it.
#
# Measured 2026-10-09: tick p99 0.7 ms with 40 bots, guest sleeps exact to
# 0.15 ms, and the Windows game from home: RTT 49 ms, no loss, no
# corrections, near and mid >99.9% interpolated. ~$0.12 an hour.
#
# Env: REGION (eu-central-1), TYPE (c7a.large), KEY (~/.ssh/id_ed25519_lattice_vm).
# Needs the AWS CLI with credentials (aws configure).
set -euo pipefail
cd "$(dirname "$0")/.."

cmd=${1:-}
region=${REGION:-eu-central-1}
type=${TYPE:-c7a.large}
key=${KEY:-$HOME/.ssh/id_ed25519_lattice_vm}
name=lattice-playtest
tag="Key=Project,Value=$name"
aws() { command aws --region "$region" --output text "$@"; }
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }

case $cmd in
up)
  bots=${2:-40}
  if [ -n "$(aws ec2 describe-instances --filters "Name=tag:Project,Values=$name" "Name=instance-state-name,Values=pending,running,stopping,stopped" --query 'Reservations[].Instances[].InstanceId')" ]; then
    echo "a $name instance exists already: $0 status, or $0 down first" >&2
    exit 1
  fi
  [ -f "$key" ] || ssh-keygen -q -t ed25519 -N "" -C "$name" -f "$key"
  home=$(curl -sSf https://checkip.amazonaws.com)
  say "key pair, firewall group (ssh from $home only)"
  aws ec2 import-key-pair --key-name "$name" --public-key-material "fileb://$key.pub" \
    --tag-specifications "ResourceType=key-pair,Tags=[{$tag}]" > /dev/null
  vpc=$(aws ec2 describe-vpcs --filters Name=is-default,Values=true --query 'Vpcs[0].VpcId')
  sg=$(aws ec2 create-security-group --group-name "$name" --vpc-id "$vpc" \
    --description "lattice playtest: game UDP from anywhere, ssh from home" \
    --tag-specifications "ResourceType=security-group,Tags=[{$tag}]" --query GroupId)
  aws ec2 authorize-security-group-ingress --group-id "$sg" --ip-permissions \
    "IpProtocol=udp,FromPort=40000,ToPort=40000,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=$home/32}]" > /dev/null
  ami=$(aws ec2 describe-images --owners 099720109477 \
    --filters "Name=name,Values=ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*" Name=state,Values=available \
    --query 'sort_by(Images, &CreationDate)[-1].ImageId')
  say "launching a $type ($ami)"
  id=$(aws ec2 run-instances --image-id "$ami" --instance-type "$type" --key-name "$name" --security-group-ids "$sg" \
    --instance-initiated-shutdown-behavior terminate \
    --user-data "$(printf '#!/bin/sh\n# Safety net: terminate after 3 hours even if forgotten.\nshutdown -h +180\n')" \
    --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=20,VolumeType=gp3,DeleteOnTermination=true}' \
    --tag-specifications "ResourceType=instance,Tags=[{$tag},{Key=Name,Value=$name}]" "ResourceType=volume,Tags=[{$tag}]" \
    --query 'Instances[0].InstanceId')
  aws ec2 wait instance-running --instance-ids "$id"
  ip=$(aws ec2 describe-instances --instance-ids "$id" --query 'Reservations[0].Instances[0].PublicIpAddress')
  # The ssh alias lattice-vm now points at it.
  if grep -q "^Host lattice-vm$" ~/.ssh/config 2> /dev/null; then
    sed -i "/^Host lattice-vm$/,/^Host /s/^    HostName .*/    HostName $ip/" ~/.ssh/config
  else
    printf '\n# The playtest server (lattice-net scripts/playtest-aws.sh).\nHost lattice-vm\n    HostName %s\n    User ubuntu\n    IdentityFile %s\n    IdentitiesOnly yes\n' "$ip" "$key" >> ~/.ssh/config
    chmod 600 ~/.ssh/config
  fi
  ssh-keygen -R "$ip" > /dev/null 2>&1 || true
  say "$id at $ip; waiting for ssh"
  until ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new lattice-vm true 2> /dev/null; do sleep 5; done
  scripts/playtest.sh deploy lattice-vm
  scripts/playtest.sh start lattice-vm "$bots"
  say "up: invites with ADDR=$ip scripts/playtest.sh invites lattice-vm NAME...; $0 down when done"
  ;;
status)
  aws ec2 describe-instances --filters "Name=tag:Project,Values=$name" \
    --query 'Reservations[].Instances[].[InstanceId,InstanceType,State.Name,PublicIpAddress,LaunchTime]'
  aws ec2 describe-security-groups --filters "Name=tag:Project,Values=$name" --query 'SecurityGroups[].[GroupId,GroupName]'
  aws ec2 describe-key-pairs --filters "Name=tag:Project,Values=$name" --query 'KeyPairs[].KeyName'
  ;;
down)
  ids=$(aws ec2 describe-instances --filters "Name=tag:Project,Values=$name" "Name=instance-state-name,Values=pending,running,stopping,stopped" --query 'Reservations[].Instances[].InstanceId')
  if [ -n "$ids" ]; then
    say "terminating $ids"
    # (A session's summary first, while it can still be fetched.)
    scripts/playtest.sh sessions lattice-vm 2> /dev/null || true
    aws ec2 terminate-instances --instance-ids $ids > /dev/null
    aws ec2 wait instance-terminated --instance-ids $ids
  fi
  for sg in $(aws ec2 describe-security-groups --filters "Name=tag:Project,Values=$name" --query 'SecurityGroups[].GroupId'); do
    aws ec2 delete-security-group --group-id "$sg"
  done
  for k in $(aws ec2 describe-key-pairs --filters "Name=tag:Project,Values=$name" --query 'KeyPairs[].KeyName'); do
    aws ec2 delete-key-pair --key-name "$k"
  done
  say "down; what's left tagged Project=$name:"
  "$0" status
  ;;
*)
  sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
