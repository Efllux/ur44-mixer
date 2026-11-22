# Unelevated: one vboxusb-gate run, output appended to results\gate-log.txt, then an event-log check.
# Usage: run-one.ps1 -Name sine-2s -- stream --mode sine --secs 2
param([Parameter(Mandatory)] [string]$Name, [Parameter(ValueFromRemainingArguments)] [string[]]$GateArgs)
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$exe = Join-Path $root 'target\release\vboxusb-gate.exe'
$log = Join-Path $root 'results\gate-log.txt'
Get-Process ur44-mixer -ErrorAction SilentlyContinue | Stop-Process -Force
$t = Get-Date
Add-Content $log "`n##### $Name : vboxusb-gate $($GateArgs -join ' ')  ($($t.ToString('s')))" -Encoding utf8
$out = & $exe @GateArgs 2>&1 | ForEach-Object { "$_" }
$code = $LASTEXITCODE
$out += "exit=$code"
$out += (& (Join-Path $root 'scripts\check-events.ps1') -Since $t.AddSeconds(-5))
Add-Content $log ($out -join "`n") -Encoding utf8
$out
