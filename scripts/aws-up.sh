#!/usr/bin/env bash
# Rents the two-machine test rig on AWS EC2: a server box and a bot box in one
# cluster placement group (the fastest link between two instances), then sets
# them up like scripts/cloud-up.sh does on Scaleway, so scripts/cloud-run.sh
# drives either.
#
#   scripts/aws-up.sh [--yes]
#   scripts/aws-up.sh --resume     (finish setting up a session whose instances exist;
#                                   if the bot box never launched, e.g. no capacity for
#                                   its type in the server's zone, launch BOT_TYPE there)
#
# BILLING STARTS WHEN THE INSTANCES LAUNCH and runs until they're terminated:
# end every session with scripts/aws-down.sh. Without --yes it shows the
# hourly price and asks first.
#
# Steps: check the credentials, the vCPU quota and that both types are offered
# in one availability zone; import the SSH key; make the security group (ssh
# from this machine's public IP, everything between the two) and the placement
# group; launch both (tagged lattice-net, ids saved to .cloud/session.env at
# once, so aws-down can always clean up); then scripts/cloud-setup.sh.
#
# Env: REGION (eu-central-1), SRV_TYPE (c7a.16xlarge: 64 cores, no SMT, 25
#      Gbps), BOT_TYPE (c7a.8xlarge: 32 cores, 12.5 Gbps; the pair fits a vCPU
#      quota of 96, and 10k bots took at most ~4.3 Gbps), SSH_KEY
#      (~/.ssh/id_ed25519_scaleway), SPOT=1 for spot instances (cheaper; AWS
#      may take them back, and spot has its own vCPU quota).
# Needs the AWS CLI configured (aws configure) with EC2 rights.
set -euo pipefail
cd "$(dirname "$0")/.."

region=${REGION:-eu-central-1}
srv_type=${SRV_TYPE:-c7a.16xlarge}
bot_type=${BOT_TYPE:-c7a.8xlarge}
key=${SSH_KEY:-$HOME/.ssh/id_ed25519_scaleway}
spot=${SPOT:-}
yes=
resume=
[ "${1:-}" = --yes ] && yes=1
[ "${1:-}" = --resume ] && resume=1

state=.cloud/session.env
mkdir -p .cloud
export AWS_DEFAULT_REGION=$region AWS_PAGER=""
json() { python3 -c "import json, sys; d = json.load(sys.stdin); $1"; }
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }
tag="ResourceType=instance,Tags=[{Key=Project,Value=lattice-net}"

# The price list names regions by their long names.
case $region in
  eu-central-1) loc="EU (Frankfurt)" ;;
  eu-west-1) loc="EU (Ireland)" ;;
  eu-west-3) loc="EU (Paris)" ;;
  eu-north-1) loc="EU (Stockholm)" ;;
  us-east-1) loc="US East (N. Virginia)" ;;
  *) loc= ;;
esac
price() { # TYPE -> on-demand USD/h (Linux, shared tenancy), or ? (needs pricing:GetProducts)
  [ -n "$loc" ] || { echo "?"; return; }
  aws pricing get-products --region us-east-1 --service-code AmazonEC2 --filters \
    "Type=TERM_MATCH,Field=instanceType,Value=$1" "Type=TERM_MATCH,Field=location,Value=$loc" \
    "Type=TERM_MATCH,Field=operatingSystem,Value=Linux" "Type=TERM_MATCH,Field=tenancy,Value=Shared" \
    "Type=TERM_MATCH,Field=preInstalledSw,Value=NA" "Type=TERM_MATCH,Field=capacitystatus,Value=Used" 2> /dev/null | json "
p = json.loads(d['PriceList'][0])
for t in p['terms']['OnDemand'].values():
    for dim in t['priceDimensions'].values():
        print('%.3f' % float(dim['pricePerUnit']['USD']))" 2> /dev/null || echo "?"
}
# Uses $ami, $subnet and $sg, set before it's called.
launch() { # NAME TYPE -> instance id, or the error on stderr
  local out market=()
  [ -n "$spot" ] && market=(--instance-market-options "MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}")
  out=$(aws ec2 run-instances --image-id "$ami" --instance-type "$2" --key-name lattice \
    --subnet-id "$subnet" --security-group-ids "$sg" --placement "GroupName=lattice-net" \
    --associate-public-ip-address --instance-initiated-shutdown-behavior terminate "${market[@]}" \
    --block-device-mappings "DeviceName=/dev/sda1,Ebs={VolumeSize=40,VolumeType=gp3,DeleteOnTermination=true}" \
    --tag-specifications "$tag,{Key=Name,Value=$1}]" 2>&1) || {
    echo "$out" >&2
    return 1
  }
  echo "$out" | json "print(d['Instances'][0]['InstanceId'])"
}

if [ -n "$resume" ]; then
  # shellcheck source=/dev/null
  . "$state"
  [ "${PROVIDER:-}" = aws ] || { echo "the session in $state isn't an AWS one" >&2; exit 1; }
  srv_id=$SRV_ID
  if [ -z "${BOT_ID:-}" ]; then
    # The bot box never launched: launch BOT_TYPE next to the server, with the
    # server's own zone, image, subnet and security group (its placement group
    # is one zone).
    eval "$(aws ec2 describe-instances --instance-ids "$srv_id" | json "
i = d['Reservations'][0]['Instances'][0]
print('ami=%s subnet=%s sg=%s srv_type=%s az=%s' % (i['ImageId'], i['SubnetId'], i['SecurityGroups'][0]['GroupId'], i['InstanceType'], i['Placement']['AvailabilityZone']))")"
    say "launching lattice-bots ($bot_type) next to lattice-srv ($srv_type) in $az"
    if ! bot_id=$(launch lattice-bots "$bot_type"); then
      echo "launching lattice-bots failed (see above); lattice-srv is still billing: --resume with another BOT_TYPE, or scripts/aws-down.sh" >&2
      exit 1
    fi
    echo "BOT_ID=$bot_id" >> "$state"
    total=$(python3 -c "print('%.2f' % ($(price "$srv_type") + $(price "$bot_type")))" 2> /dev/null || echo "?")
    sed -i "s/^PRICE_PER_HOUR=.*/PRICE_PER_HOUR=$total/" "$state"
    BOT_ID=$bot_id
    PRICE_PER_HOUR=$total
  fi
  bot_id=$BOT_ID
  total=$PRICE_PER_HOUR
  sed -i '/^\(SRV_PUB\|BOT_PUB\|SRV_PRIV\|BOT_PRIV\|LINK_GBPS\|TOKEN_KEY\)=/d' "$state"
  say "resuming the session of lattice-srv $srv_id and lattice-bots $bot_id"
elif [ -s "$state" ]; then
  echo "a session is already up ($state): use it, --resume its setup, or end it with scripts/aws-down.sh" >&2
  exit 1
else

aws sts get-caller-identity > /dev/null || { echo "AWS credentials don't work: run aws configure" >&2; exit 1; }

# --- what we'd rent ------------------------------------------------------------
vcpus() { aws ec2 describe-instance-types --instance-types "$1" | json "print(d['InstanceTypes'][0]['VCpuInfo']['DefaultVCpus'])"; }
need=$(( $(vcpus "$srv_type") + $(vcpus "$bot_type") ))
quota_code=L-1216C47A # Running On-Demand Standard (A, C, D, H, I, M, R, T, Z) instances
[ -n "$spot" ] && quota_code=L-34B43A08 # All Standard Spot Instance Requests
quota=$(aws service-quotas get-service-quota --service-code ec2 --quota-code "$quota_code" | json "print(int(d['Quota']['Value']))")
if [ "$quota" -lt "$need" ]; then
  echo "the vCPU quota ($quota_code) is $quota; $srv_type + $bot_type need $need. Request an increase in Service Quotas." >&2
  exit 1
fi

# One availability zone that offers both types (a placement group is one AZ).
az=$(aws ec2 describe-instance-type-offerings --location-type availability-zone \
  --filters "Name=instance-type,Values=$srv_type,$bot_type" | json "
from collections import defaultdict
types = defaultdict(set)
for o in d['InstanceTypeOfferings']:
    types[o['Location']].add(o['InstanceType'])
print(sorted(z for z, t in types.items() if t == {'$srv_type', '$bot_type'})[0])")

srv_price=$(price "$srv_type")
bot_price=$(price "$bot_type")
total=$(python3 -c "print('%.2f' % ($srv_price + $bot_price))" 2> /dev/null || echo "?")
# Canonical's newest Ubuntu 24.04 (x86-64, gp3) image.
ami=$(aws ec2 describe-images --owners 099720109477 \
  --filters "Name=name,Values=ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*" Name=state,Values=available |
  json "print(max(d['Images'], key=lambda i: i['CreationDate'])['ImageId'])")
echo "server: $srv_type, USD $srv_price/h on demand"
echo "bots:   $bot_type, USD $bot_price/h on demand"
echo "$region ($az), Ubuntu 24.04 ($ami), cluster placement group, SSH key $key${spot:+, SPOT (cheaper than the above)}"
echo "=> USD $total per hour${spot:+ at most}, billed from launch until scripts/aws-down.sh terminates them"
if [ -z "$yes" ]; then
  read -r -p "launch both instances? [y/N] " answer
  [ "$answer" = y ] || [ "$answer" = Y ] || { echo "nothing launched"; exit 1; }
fi

# --- key, security group, placement group (free; kept between sessions) ------
aws ec2 describe-key-pairs --key-names lattice > /dev/null 2>&1 ||
  aws ec2 import-key-pair --key-name lattice --public-key-material "fileb://$key.pub" > /dev/null
vpc=$(aws ec2 describe-vpcs --filters Name=isDefault,Values=true | json "print(d['Vpcs'][0]['VpcId'])")
subnet=$(aws ec2 describe-subnets --filters "Name=vpc-id,Values=$vpc" "Name=availability-zone,Values=$az" "Name=default-for-az,Values=true" |
  json "print(d['Subnets'][0]['SubnetId'])")
sg=$(aws ec2 describe-security-groups --filters "Name=vpc-id,Values=$vpc" Name=group-name,Values=lattice-net |
  json "print(d['SecurityGroups'][0]['GroupId'] if d['SecurityGroups'] else '')")
if [ -z "$sg" ]; then
  sg=$(aws ec2 create-security-group --group-name lattice-net --description "lattice-net test rig" --vpc-id "$vpc" | json "print(d['GroupId'])")
  aws ec2 authorize-security-group-ingress --group-id "$sg" --ip-permissions "IpProtocol=-1,UserIdGroupPairs=[{GroupId=$sg}]" > /dev/null
fi
# ssh from wherever this runs now (the rule is replaced each session).
me=$(curl -sSf https://checkip.amazonaws.com | tr -d '[:space:]')
aws ec2 describe-security-groups --group-ids "$sg" | json "
for p in d['SecurityGroups'][0]['IpPermissions']:
    if p.get('FromPort') == 22:
        for r in p.get('IpRanges', []): print(r['CidrIp'])" | while read -r cidr; do
  aws ec2 revoke-security-group-ingress --group-id "$sg" --protocol tcp --port 22 --cidr "$cidr" > /dev/null
done
aws ec2 authorize-security-group-ingress --group-id "$sg" --protocol tcp --port 22 --cidr "$me/32" > /dev/null
aws ec2 describe-placement-groups --group-names lattice-net > /dev/null 2>&1 ||
  aws ec2 create-placement-group --group-name lattice-net --strategy cluster > /dev/null

# --- launch (ids saved immediately) --------------------------------------------
{
  echo "PROVIDER=aws"
  echo "REGION=$region"
  echo "SSH_KEY=$key"
  echo "PRICE_PER_HOUR=$total"
  echo "CREATED=$(date -Is)"
} > "$state"
if ! srv_id=$(launch lattice-srv "$srv_type"); then
  rm -f "$state"
  echo "launching lattice-srv failed (see above); nothing was launched" >&2
  exit 1
fi
echo "SRV_ID=$srv_id" >> "$state"
if ! bot_id=$(launch lattice-bots "$bot_type"); then
  echo "launching lattice-bots failed (see above); lattice-srv exists and is billing." >&2
  echo "Launch a bot box of another type next to it: BOT_TYPE=<type> scripts/aws-up.sh --resume (32 vCPUs or fewer" >&2
  echo "fits a quota of 96 with the server; c7a.8xlarge worked when c6in.8xlarge had no capacity). Or end it: scripts/aws-down.sh" >&2
  exit 1
fi
echo "BOT_ID=$bot_id" >> "$state"
say "launched lattice-srv $srv_id and lattice-bots $bot_id; billing has started"

fi # launch

say "waiting for both to run"
aws ec2 wait instance-running --instance-ids "$srv_id" "$bot_id"
addr() { # ID FIELD -> its address
  aws ec2 describe-instances --instance-ids "$1" | json "print(d['Reservations'][0]['Instances'][0]['$2'])"
}
srv_pub=$(addr "$srv_id" PublicIpAddress)
bot_pub=$(addr "$bot_id" PublicIpAddress)
srv_priv=$(addr "$srv_id" PrivateIpAddress)
bot_priv=$(addr "$bot_id" PrivateIpAddress)
{
  echo "SRV_PUB=$srv_pub"
  echo "BOT_PUB=$bot_pub"
  echo "SRV_PRIV=$srv_priv"
  echo "BOT_PRIV=$bot_priv"
} >> "$state"
say "running: lattice-srv $srv_pub ($srv_priv), lattice-bots $bot_pub ($bot_priv)"

cat > .cloud/ssh_config << EOF
Host lattice-srv
  HostName $srv_pub
Host lattice-bots
  HostName $bot_pub
Host lattice-*
  User ubuntu
  IdentityFile $key
  IdentitiesOnly yes
  UserKnownHostsFile $PWD/.cloud/known_hosts
  StrictHostKeyChecking accept-new
  ServerAliveInterval 30
EOF
on() { ssh -F .cloud/ssh_config -o BatchMode=yes "$@"; }
for h in lattice-srv lattice-bots; do
  for _ in $(seq 60); do on -o ConnectTimeout=5 "$h" true 2> /dev/null && break; sleep 5; done
  on "$h" true
done

scripts/cloud-setup.sh

cat << EOF

Up, billing USD $total/h. ssh -F .cloud/ssh_config lattice-srv | lattice-bots
Run:  scripts/cloud-run.sh m3e aws                      (and fight, limits, netem ...)
End:  scripts/aws-down.sh                               (copies baselines back, terminates both)
EOF
