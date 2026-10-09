#!/usr/bin/env bash
# Snapshots and restores this Windows machine's network configuration, around
# anything the playtest scripts change (Hyper-V switches, NAT, adapter
# bindings). Run from an elevated WSL.
#
#   scripts/netstate.sh save [NAME]       snapshot to %USERPROFILE%\lattice-vm\netstate\NAME.json
#                                         (default: a timestamp; also kept as latest.json)
#   scripts/netstate.sh check [NAME]      what differs now from a snapshot (default latest)
#   scripts/netstate.sh restore [NAME] [--bindings] [--yes]
#                                         undo, back to the snapshot: removes Hyper-V switches
#                                         and NATs named lattice* that it doesn't have; with
#                                         --bindings also sets every adapter binding back as
#                                         it was. Prints the plan; changes nothing without --yes.
#
# Switches and NATs not named lattice* are only reported, never removed:
# they're someone else's (Windows', WSL's, yours).
set -euo pipefail

cmd=${1:-}
shift || true
name=latest bindings=0 yes=0
for a in "$@"; do
  case $a in
    --bindings) bindings=1 ;;
    --yes) yes=1 ;;
    -*) echo "unknown option $a" >&2; exit 2 ;;
    *) name=$a ;;
  esac
done

profile=$(cd /mnt/c && cmd.exe /c "echo %USERPROFILE%" 2> /dev/null | tr -d '\r')
dir_win="$profile\\lattice-vm\\netstate"
dir=$(wslpath -u "$dir_win")
mkdir -p "$dir"

# Runs the PowerShell script on stdin (ASCII only) with NETSTATE_* settings.
psh() {
  local f="$dir/$1.ps1"
  cat > "$f"
  NETSTATE_FILE="$dir_win\\$name.json" NETSTATE_DIR=$dir_win NETSTATE_BINDINGS=$bindings NETSTATE_YES=$yes \
    WSLENV="${WSLENV:+$WSLENV:}NETSTATE_FILE:NETSTATE_DIR:NETSTATE_BINDINGS:NETSTATE_YES" \
    powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(wslpath -w "$f")" | tr -d '\r'
  return "${PIPESTATUS[0]}"
}

# The state as one object; shared by every command.
state='function Get-LatticeNetState {
    [pscustomobject]@{
        Taken    = (Get-Date).ToString("s")
        Switches = @(Get-VMSwitch -ErrorAction SilentlyContinue | ForEach-Object {
            [pscustomobject]@{ Name = $_.Name; Type = "$($_.SwitchType)"; Adapter = $_.NetAdapterInterfaceDescription } })
        Nats     = @(Get-NetNat -ErrorAction SilentlyContinue | ForEach-Object {
            [pscustomobject]@{ Name = $_.Name; Prefix = $_.InternalIPInterfaceAddressPrefix } })
        Adapters = @(Get-NetAdapter | ForEach-Object {
            [pscustomobject]@{ Name = $_.Name; Description = $_.InterfaceDescription; Status = "$($_.Status)"; Mac = $_.MacAddress } })
        Bindings = @(Get-NetAdapterBinding -ErrorAction SilentlyContinue | ForEach-Object {
            [pscustomobject]@{ Adapter = $_.Name; Component = $_.ComponentID; Enabled = [bool]$_.Enabled } })
        Routes   = @(Get-NetRoute -DestinationPrefix 0.0.0.0/0 -ErrorAction SilentlyContinue | ForEach-Object {
            [pscustomobject]@{ Interface = $_.InterfaceAlias; NextHop = $_.NextHop } })
    }
}
function Read-LatticeNetState($path) {
    if (-not (Test-Path $path)) { Write-Output "no snapshot $path (scripts/netstate.sh save first)"; exit 2 }
    Get-Content -Raw -Encoding UTF8 $path | ConvertFrom-Json
}
function Compare-LatticeNetState($old, $new) {
    $d = @()
    foreach ($s in $new.Switches) { if (-not ($old.Switches | Where-Object Name -eq $s.Name)) { $d += "switch added: $($s.Name) ($($s.Type) $($s.Adapter))" } }
    foreach ($s in $old.Switches) { if (-not ($new.Switches | Where-Object Name -eq $s.Name)) { $d += "switch gone: $($s.Name)" } }
    foreach ($n in $new.Nats) { if (-not ($old.Nats | Where-Object Name -eq $n.Name)) { $d += "NAT added: $($n.Name) ($($n.Prefix))" } }
    foreach ($n in $old.Nats) { if (-not ($new.Nats | Where-Object Name -eq $n.Name)) { $d += "NAT gone: $($n.Name)" } }
    foreach ($a in $new.Adapters) { if (-not ($old.Adapters | Where-Object Name -eq $a.Name)) { $d += "adapter added: $($a.Name) ($($a.Description))" } }
    foreach ($a in $old.Adapters) { if (-not ($new.Adapters | Where-Object Name -eq $a.Name)) { $d += "adapter gone: $($a.Name) ($($a.Description))" } }
    foreach ($b in $old.Bindings) {
        $now = $new.Bindings | Where-Object { $_.Adapter -eq $b.Adapter -and $_.Component -eq $b.Component }
        if ($now -and [bool]$now.Enabled -ne [bool]$b.Enabled) { $d += "binding changed: $($b.Adapter) $($b.Component) $($b.Enabled) -> $($now.Enabled)" }
    }
    $or = ($old.Routes | ForEach-Object { "$($_.Interface) via $($_.NextHop)" }) -join ", "
    $nr = ($new.Routes | ForEach-Object { "$($_.Interface) via $($_.NextHop)" }) -join ", "
    if ($or -ne $nr) { $d += "default route: [$or] -> [$nr]" }
    $d
}'

case $cmd in
save)
  [ "$name" = latest ] && name=$(date +%Y%m%d-%H%M%S)
  psh save << PS
$state
\$s = Get-LatticeNetState
\$json = \$s | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText(\$env:NETSTATE_FILE, \$json, [Text.Encoding]::UTF8)
[IO.File]::WriteAllText((Join-Path \$env:NETSTATE_DIR "latest.json"), \$json, [Text.Encoding]::UTF8)
"saved \$(\$env:NETSTATE_FILE): \$(\$s.Switches.Count) switches, \$(\$s.Nats.Count) NATs, \$(\$s.Adapters.Count) adapters, \$(\$s.Bindings.Count) bindings"
PS
  ;;
check)
  psh check << PS
$state
\$old = Read-LatticeNetState \$env:NETSTATE_FILE
\$d = Compare-LatticeNetState \$old (Get-LatticeNetState)
if (\$d) { "changed since \$(\$old.Taken):"; \$d | ForEach-Object { "  \$_" } } else { "unchanged since \$(\$old.Taken)" }
PS
  ;;
restore)
  psh restore << PS
$state
\$old = Read-LatticeNetState \$env:NETSTATE_FILE
\$new = Get-LatticeNetState
\$apply = \$env:NETSTATE_YES -eq "1"
\$plan = @()
foreach (\$s in \$new.Switches) {
    if (\$old.Switches | Where-Object Name -eq \$s.Name) { continue }
    if (\$s.Name -like "lattice*") { \$plan += ,@("remove switch \$(\$s.Name)", [scriptblock]::Create("Remove-VMSwitch -Name '\$(\$s.Name)' -Force")) }
    else { "not ours, left alone: switch \$(\$s.Name)" }
}
foreach (\$n in \$new.Nats) {
    if (\$old.Nats | Where-Object Name -eq \$n.Name) { continue }
    if (\$n.Name -like "lattice*") { \$plan += ,@("remove NAT \$(\$n.Name)", [scriptblock]::Create("Remove-NetNat -Name '\$(\$n.Name)' -Confirm:\`\$false")) }
    else { "not ours, left alone: NAT \$(\$n.Name)" }
}
if (\$env:NETSTATE_BINDINGS -eq "1") {
    foreach (\$b in \$old.Bindings) {
        \$now = \$new.Bindings | Where-Object { \$_.Adapter -eq \$b.Adapter -and \$_.Component -eq \$b.Component }
        if (-not \$now -or [bool]\$now.Enabled -eq [bool]\$b.Enabled) { continue }
        \$verb = if (\$b.Enabled) { "Enable" } else { "Disable" }
        \$plan += ,@("\$verb binding \$(\$b.Component) on \$(\$b.Adapter)", [scriptblock]::Create("\$verb-NetAdapterBinding -Name '\$(\$b.Adapter)' -ComponentID '\$(\$b.Component)'"))
    }
}
if (-not \$plan) { "nothing to undo against \$(\$old.Taken)"; exit 0 }
"to restore \$(\$old.Taken):"
\$plan | ForEach-Object { "  " + \$_[0] }
if (-not \$apply) { "(nothing changed: add --yes to do it)"; exit 0 }
\$plan | ForEach-Object { "doing: " + \$_[0]; & \$_[1] }
\$d = Compare-LatticeNetState \$old (Get-LatticeNetState)
if (\$d) { "still differs:"; \$d | ForEach-Object { "  \$_" } } else { "restored" }
PS
  ;;
*)
  sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
