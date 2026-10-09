#!/usr/bin/env bash
# The playtest server as a Hyper-V VM on this Windows machine, driven from
# WSL: Ubuntu 24.04's cloud image on an external virtual switch (so it has its
# own LAN address, for the router to forward the players' UDP port to), set up
# by cloud-init with an ssh key of its own. Then scripts/playtest.sh deploys
# onto it as lattice-vm, an ssh alias this keeps pointing at it.
#
#   scripts/playtest-hyperv.sh enable    turn on the Hyper-V role (then restart Windows)
#   scripts/playtest-hyperv.sh create    disk, seed, switch and VM; boots it, waits for its
#                                        address and points the ssh alias at it
#   scripts/playtest-hyperv.sh status    state, uptime and address
#   scripts/playtest-hyperv.sh start | stop
#   scripts/playtest-hyperv.sh delete    the VM and its disk (the switch stays: the host's
#                                        network runs over it; Remove-VMSwitch to undo)
#
# Why Hyper-V and not VirtualBox: with WSL2 installed, VirtualBox runs on top
# of Windows' hypervisor, and its guests' timers stall. A 20 ms sleep took
# 53 ms at p99 and 380 ms at worst (WSL2: 20.1 ms), which players would feel
# as stutter. A Hyper-V guest runs on the same hypervisor as WSL2.
#
# Needs Windows 10/11 Pro or Enterprise and an elevated WSL (its terminal run
# as administrator): the Hyper-V cmdlets require it. It never changes the
# network: SWITCH must exist already (an external switch on Wi-Fi, made by an
# earlier version, took the network down), and create snapshots the network
# first (scripts/netstate.sh). Converting the image's
# disk takes qemu-img, or VirtualBox's VBoxManage if that's installed.
#
# Env: VM (lattice-playtest), CPUS (4), MEMORY_GB (4), DISK_GB (20), SWITCH
# (lattice-lan), DIR (a Windows folder for the image, disk and seed:
# %USERPROFILE%\lattice-vm), KEY (~/.ssh/id_ed25519_lattice_vm, made if missing).
set -euo pipefail
cd "$(dirname "$0")/.."

cmd=${1:-}
vm=${VM:-lattice-playtest}
cpus=${CPUS:-4}
memory_gb=${MEMORY_GB:-4}
disk_gb=${DISK_GB:-20}
switch=${SWITCH:-lattice-lan}
key=${KEY:-$HOME/.ssh/id_ed25519_lattice_vm}
host_alias=lattice-vm
image=noble-server-cloudimg-amd64.ova
image_url=https://cloud-images.ubuntu.com/noble/current
say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }

profile=$(cd /mnt/c && cmd.exe /c "echo %USERPROFILE%" 2> /dev/null | tr -d '\r')
dir_win=${DIR:-$profile\\lattice-vm}
dir=$(wslpath -u "$dir_win")
mkdir -p "$dir"

# Runs the PowerShell script on stdin with Windows' own PowerShell (elevated
# when this WSL is), saved as $dir/<name>.ps1; its settings come in LATTICE_*
# variables. ASCII only: PowerShell 5 reads scripts in the system code page.
psh() {
  local f="$dir/$1.ps1"
  cat > "$f"
  LATTICE_VM=$vm LATTICE_CPUS=$cpus LATTICE_MEMORY_GB=$memory_gb LATTICE_DISK_GB=$disk_gb LATTICE_SWITCH=$switch LATTICE_DIR=$dir_win \
    WSLENV="${WSLENV:+$WSLENV:}LATTICE_VM:LATTICE_CPUS:LATTICE_MEMORY_GB:LATTICE_DISK_GB:LATTICE_SWITCH:LATTICE_DIR" \
    powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(wslpath -w "$f")" | tr -d '\r'
  return "${PIPESTATUS[0]}"
}

# The start of each script that needs administrator rights.
admin='$ErrorActionPreference = "Stop"
$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output "needs administrator rights: start the WSL terminal as administrator"; exit 2
}'

# The image (checked against Ubuntu's signed checksums), its disk as a VHD,
# the ssh key, and the cloud-init seed as an ISO.
prepare() {
  mkdir -p "$dir/disk" "$dir/seed"
  [ -f "$key" ] || ssh-keygen -q -t ed25519 -N "" -C "$vm Hyper-V VM" -f "$key"
  (
    cd "$dir"
    curl -sSfL -o SHA256SUMS "$image_url/SHA256SUMS"
    curl -sSfL -o SHA256SUMS.gpg "$image_url/SHA256SUMS.gpg"
    gpgv --keyring /usr/share/keyrings/ubuntu-cloudimage-keyring.gpg SHA256SUMS.gpg SHA256SUMS 2> /dev/null \
      || { echo "SHA256SUMS: bad signature" >&2; exit 1; }
    ok() { [ -f "$image" ] && sha256sum -c --ignore-missing SHA256SUMS 2> /dev/null | grep -q "^$image: OK"; }
    if ! ok; then
      say "downloading Ubuntu 24.04's cloud image (~600 MB)"
      curl -sSfL -o "$image" "$image_url/$image"
      ok || { echo "$image: checksum mismatch" >&2; exit 1; }
    fi
  )
  if [ ! -f "$dir/disk/base.vhd" ]; then
    say "converting its disk for Hyper-V"
    local vmdk vbm="/mnt/c/Program Files/Oracle/VirtualBox/VBoxManage.exe"
    vmdk=$(tar -tf "$dir/$image" | grep '\.vmdk$' | head -1)
    tar -xf "$dir/$image" -C "$dir/disk" "$vmdk"
    if command -v qemu-img > /dev/null; then
      qemu-img convert -O vpc "$dir/disk/$vmdk" "$dir/disk/base.vhd"
    elif [ -x "$vbm" ]; then
      "$vbm" clonemedium disk "$(wslpath -w "$dir/disk/$vmdk")" "$(wslpath -w "$dir/disk/base.vhd")" --format VHD > /dev/null
      "$vbm" closemedium disk "$(wslpath -w "$dir/disk/base.vhd")" > /dev/null
      "$vbm" closemedium disk "$(wslpath -w "$dir/disk/$vmdk")" > /dev/null
    else
      echo "converting the disk takes qemu-img (apt install qemu-utils) or VirtualBox's VBoxManage" >&2
      exit 1
    fi
    rm -f "$dir/disk/$vmdk"
  fi
  cat > "$dir/seed/user-data" << EOF
#cloud-config
hostname: $vm
ssh_authorized_keys:
  - $(cat "$key.pub")
ssh_pwauth: false
package_update: true
packages:
  - rsync
  - build-essential
swap:
  filename: /swap.img
  size: 2G
runcmd:
  # Hyper-V's key-value exchange, so the host can see the VM's address.
  - [sh, -c, "DEBIAN_FRONTEND=noninteractive apt-get install -y linux-cloud-tools-\$(uname -r) && systemctl enable --now hv-kvp-daemon"]
EOF
  printf 'instance-id: %s-%s\nlocal-hostname: %s\n' "$vm" "$(date +%Y%m%d%H%M%S)" "$vm" > "$dir/seed/meta-data"
  psh seed-iso << 'PS'
$ErrorActionPreference = "Stop"
$dir = $env:LATTICE_DIR
# ISO 9660 + Joliet (lowercase names), labeled cidata: cloud-init's NoCloud seed.
$fsi = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
$fsi.FileSystemsToCreate = 3
$fsi.VolumeName = "cidata"
$fsi.Root.AddTree((Join-Path $dir "seed"), $false)
$stream = $fsi.CreateResultImage().ImageStream
Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
public static class LatticeIso {
    public static void Save(object comStream, string path) {
        IStream s = (IStream)comStream;
        byte[] buf = new byte[65536];
        IntPtr read = Marshal.AllocHGlobal(4);
        try {
            using (FileStream f = File.Create(path)) {
                while (true) {
                    s.Read(buf, buf.Length, read);
                    int n = Marshal.ReadInt32(read);
                    if (n <= 0) break;
                    f.Write(buf, 0, n);
                }
            }
        } finally { Marshal.FreeHGlobal(read); }
    }
}
"@
[LatticeIso]::Save($stream, (Join-Path $dir "seed.iso"))
PS
}

# The VM's IPv4 address as Hyper-V sees it (the guest's KVP daemon reports it).
vm_ip() {
  psh ip << 'PS'
$a = Get-VMNetworkAdapter -VMName $env:LATTICE_VM -ErrorAction SilentlyContinue
$a.IPAddresses | Where-Object { $_ -match "^\d+\.\d+\.\d+\.\d+$" } | Select-Object -First 1
PS
}

# Points Host lattice-vm in ~/.ssh/config at address $1, and forgets any old
# host key for that address.
point_alias() {
  local cfg=~/.ssh/config
  touch "$cfg"
  chmod 600 "$cfg"
  python3 - "$cfg" "$host_alias" "$1" "$key" << 'PY'
import sys
path, alias, address, key = sys.argv[1:]
lines = open(path).read().splitlines()
block = [f"Host {alias}", f"    HostName {address}", "    User ubuntu", f"    IdentityFile {key}", "    IdentitiesOnly yes"]
out, i = [], 0
while i < len(lines):
    if lines[i].strip() == f"Host {alias}":
        # Replace the block, up to the next Host line.
        i += 1
        while i < len(lines) and not lines[i].lstrip().startswith("Host "):
            i += 1
        out += block
        block = None
        continue
    out.append(lines[i])
    i += 1
if block:
    out += ["", "# The playtest server: a Hyper-V VM on this machine (lattice-net scripts/playtest-hyperv.sh)."] + block
open(path, "w").write("\n".join(out) + "\n")
PY
  ssh-keygen -R "$1" > /dev/null 2>&1 || true
}

case $cmd in
enable)
  psh enable << PS
$admin
\$f = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All
if (\$f.State -eq "Enabled") { Write-Output "Hyper-V is on already."; exit 0 }
\$r = Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -All -NoRestart
if (\$r.RestartNeeded) { Write-Output "Hyper-V is turned on: restart Windows to finish, then run scripts/playtest-hyperv.sh create." }
else { Write-Output "Hyper-V is on." }
PS
  ;;
prepare)
  prepare
  say "ready in $dir_win: disk\\base.vhd, seed.iso"
  ;;
create)
  prepare
  say "snapshot of the network first (scripts/netstate.sh restore undoes changes)"
  scripts/netstate.sh save "before-$vm-$(date +%Y%m%d-%H%M%S)"
  say "creating $vm on switch $switch"
  psh create << PS
$admin
\$vm = \$env:LATTICE_VM; \$switch = \$env:LATTICE_SWITCH; \$dir = \$env:LATTICE_DIR
if (-not (Get-Command New-VM -ErrorAction SilentlyContinue)) {
    Write-Output "Hyper-V isn't on: scripts/playtest-hyperv.sh enable, then restart Windows"; exit 2
}
if (Get-VM -Name \$vm -ErrorAction SilentlyContinue) {
    Write-Output "\$vm exists already (scripts/playtest-hyperv.sh delete to start over)"; exit 2
}
# The network is never changed here: the switch must exist already.
if (-not (Get-VMSwitch -Name \$switch -ErrorAction SilentlyContinue)) {
    Write-Output "no Hyper-V switch \$switch: this script doesn't make one (making an external switch on Wi-Fi took the network down once). Set SWITCH to an existing one."; exit 2
}
\$vhdx = Join-Path \$dir "disk\\\$vm.vhdx"
if (Test-Path \$vhdx) { Remove-Item \$vhdx }
Convert-VHD -Path (Join-Path \$dir "disk\\base.vhd") -DestinationPath \$vhdx -VHDType Dynamic
Resize-VHD -Path \$vhdx -SizeBytes ([int64]\$env:LATTICE_DISK_GB * 1GB)
New-VM -Name \$vm -Generation 2 -MemoryStartupBytes ([int64]\$env:LATTICE_MEMORY_GB * 1GB) -VHDPath \$vhdx -SwitchName \$switch | Out-Null
# Fixed memory and no checkpoints; it starts with Windows and shuts down cleanly.
Set-VM -Name \$vm -ProcessorCount ([int]\$env:LATTICE_CPUS) -StaticMemory -AutomaticCheckpointsEnabled \$false -AutomaticStartAction Start -AutomaticStartDelay 10 -AutomaticStopAction ShutDown
Add-VMDvdDrive -VMName \$vm -Path (Join-Path \$dir "seed.iso")
# Ubuntu boots under Secure Boot with Microsoft's third-party CA.
Set-VMFirmware -VMName \$vm -EnableSecureBoot On -SecureBootTemplate MicrosoftUEFICertificateAuthority -FirstBootDevice (Get-VMHardDiskDrive -VMName \$vm)
# The serial console, for when it doesn't come up: \\\\.\\pipe\\<vm>-com1.
Set-VMComPort -VMName \$vm -Number 1 -Path "\\\\.\\pipe\\\$vm-com1"
Start-VM -Name \$vm
Write-Output "started \$vm"
PS
  say "waiting for its address (its first boot installs packages: a few minutes)"
  ip=
  for _ in $(seq 120); do
    ip=$(vm_ip || true)
    [ -n "$ip" ] && break
    sleep 5
  done
  [ -n "$ip" ] || { echo "no address after 10 minutes: look at the VM in Hyper-V Manager" >&2; exit 1; }
  point_alias "$ip"
  say "$vm is at $ip (ssh $host_alias); waiting for its first-boot setup"
  until ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new "$host_alias" true 2> /dev/null; do sleep 5; done
  ssh -o BatchMode=yes "$host_alias" "sudo cloud-init status --wait > /dev/null || true; cloud-init status"
  psh eject << 'PS'
Set-VMDvdDrive -VMName $env:LATTICE_VM -Path $null
PS
  say "ready: scripts/playtest.sh deploy $host_alias"
  ;;
status)
  psh status << 'PS'
$v = Get-VM -Name $env:LATTICE_VM -ErrorAction SilentlyContinue
if (-not $v) { Write-Output "no VM $($env:LATTICE_VM)"; exit 0 }
$ip = (Get-VMNetworkAdapter -VMName $v.Name).IPAddresses -join " "
"{0}: {1}, up {2}, {3} CPUs, {4} GB, CPU {5}%, addresses {6}" -f $v.Name, $v.State, $v.Uptime, $v.ProcessorCount, ($v.MemoryStartup / 1GB), $v.CPUUsage, $ip
PS
  ;;
start)
  psh start << PS
$admin
Start-VM -Name \$env:LATTICE_VM
PS
  ;;
stop)
  psh stop << PS
$admin
Stop-VM -Name \$env:LATTICE_VM
PS
  ;;
delete)
  psh delete << PS
$admin
\$v = Get-VM -Name \$env:LATTICE_VM -ErrorAction SilentlyContinue
if (-not \$v) { Write-Output "no VM \$(\$env:LATTICE_VM)"; exit 0 }
\$disks = (Get-VMHardDiskDrive -VMName \$v.Name).Path
Stop-VM -Name \$v.Name -TurnOff -Force -ErrorAction SilentlyContinue
Remove-VM -Name \$v.Name -Force
\$disks | ForEach-Object { Remove-Item \$_ }
Write-Output "deleted \$(\$v.Name) and its disk"
PS
  ;;
*)
  sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
