# One elevated run for the VBoxUSB gate: install usbipd-win, force-bind the UR44 to VBoxUSB, stop the
# usbipd service (the test opens the device itself), and register an on-demand restore task so the
# later unbind needs no second admin prompt. Everything is logged to results\setup-elevated.log.
#
# Usage (from an unelevated shell; this script is started with -Verb RunAs):
#   setup-elevated.ps1 -User <DOMAIN\user> -Msi <path to usbipd-win msi> -Marker <path>
# Marker protocol: after the bind the script waits up to 3 min for the unelevated side to write
# "ok" (non-admin open works) or "denied" into -Marker. On "denied" it registers fixed-argument run
# tasks that execute an admin-only copy of vboxusb-gate.exe. The restore task removes all of it.
param(
    [Parameter(Mandatory)] [string]$User,
    [Parameter(Mandatory)] [string]$Msi,
    [Parameter(Mandatory)] [string]$Marker
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)   # tools\vboxusb-gate
$res = Join-Path $here 'results'
$log = Join-Path $res 'setup-elevated.log'
$pd = 'C:\ProgramData\ur44-vboxgate'
function Log([string]$m) { $l = "$(Get-Date -Format o)  $m"; Add-Content -Path $log -Value $l -Encoding utf8; Write-Host $l }
function Path-Refresh { $env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User') }

Log "=== setup-elevated start (user $User)"
try {
    # 1. install usbipd-win
    $existing = Get-Command usbipd -ErrorAction SilentlyContinue
    if ($existing) {
        Log "usbipd already installed: $($existing.Source)"
    } else {
        $msiLog = Join-Path $res 'usbipd-msi.log'
        Log "msiexec /i $Msi"
        $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$Msi`"", '/qn', '/norestart', '/l*v', "`"$msiLog`"") -Wait -PassThru
        Log "msiexec exit code $($p.ExitCode) (0 = ok, 3010 = ok, reboot suggested)"
        if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { throw "msiexec failed" }
    }
    Path-Refresh
    Log "usbipd --version: $(& usbipd --version 2>&1)"
    foreach ($f in 'C:\Program Files\usbipd-win\Drivers\VBoxUSB.sys', 'C:\Program Files\usbipd-win\Drivers\VBoxUSBMon.sys') {
        if (Test-Path $f) { $s = Get-AuthenticodeSignature $f; Log "$f signature $($s.Status) $($s.SignerCertificate.Subject)" }
    }
    Get-WindowsDriver -Online | Where-Object { $_.OriginalFileName -match 'vboxusb' } | ForEach-Object {
        Log "driver store: $($_.Driver) $($_.OriginalFileName) $($_.ProviderName) $($_.Version)"
    }
    Log "usbipd list:`n$((& usbipd list 2>&1) -join "`n")"

    # 2. force-bind the UR44 to VBoxUSB (takes over the whole device, all interfaces)
    Log "usbipd bind --force --hardware-id 0499:150e"
    $out = & usbipd bind --force --hardware-id 0499:150e 2>&1
    Log "bind exit $LASTEXITCODE : $($out -join ' | ')"
    Start-Sleep -Seconds 3
    Log "usbipd list after bind:`n$((& usbipd list 2>&1) -join "`n")"
    Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -match 'VID_0499&PID_150E' } | ForEach-Object {
        $svc = (Get-PnpDeviceProperty -InstanceId $_.InstanceId DEVPKEY_Device_Service -ErrorAction SilentlyContinue).Data
        Log "pnp $($_.Status) $($_.InstanceId) service=$svc name=$($_.FriendlyName)"
    }

    # 3. the test talks to VBoxUSB directly; keep the usbipd service from touching the device
    Stop-Service usbipd -Force -ErrorAction SilentlyContinue
    Log "usbipd service: $((Get-Service usbipd -ErrorAction SilentlyContinue).Status)"

    # 4. admin-only folder with the restore script, and the on-demand restore task
    New-Item -ItemType Directory -Force $pd | Out-Null
    icacls $pd /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' 'Users:(OI)(CI)RX' | Out-Null
    Copy-Item (Join-Path $here 'scripts\restore-elevated.ps1') "$pd\restore-elevated.ps1" -Force
    $principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
    $act = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$pd\restore-elevated.ps1`" -Results `"$res`""
    Register-ScheduledTask -TaskName 'UR44-VBoxGate-Restore' -Action $act -Principal $principal -Settings $settings -Force | Out-Null
    Log "registered task UR44-VBoxGate-Restore (on demand, run as $User, highest)"

    # 5. wait for the unelevated open test
    Log "BIND_DONE - waiting for marker $Marker"
    $deadline = (Get-Date).AddMinutes(3)
    $m = $null
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $Marker) { $m = (Get-Content $Marker -Raw).Trim(); break }
        Start-Sleep -Milliseconds 500
    }
    Log "marker: '$m'"
    if ($m -eq 'denied') {
        # Fixed-argument run tasks on an admin-only copy of the binary (no general elevation channel).
        Copy-Item (Join-Path $here 'target\release\vboxusb-gate.exe') "$pd\vboxusb-gate.exe" -Force
        $runs = [ordered]@{
            'probe'      = 'probe'
            'sine-2s'    = 'stream --mode sine --secs 2'
            'sine-10s'   = 'stream --mode sine --secs 10'
            'sine-60s'   = 'stream --mode sine --secs 60'
            'sine-300s'  = 'stream --mode sine --secs 300'
            'click-d8'   = 'stream --mode click --secs 10 --out-depth 8'
            'click-d6'   = 'stream --mode click --secs 10 --out-depth 6'
            'click-d4'   = 'stream --mode click --secs 10 --out-depth 4 --in-depth 4'
        }
        foreach ($k in $runs.Keys) {
            $o = Join-Path $res "task-$k.txt"
            $a = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c `"`"$pd\vboxusb-gate.exe`" $($runs[$k]) > `"$o`" 2>&1`""
            Register-ScheduledTask -TaskName "UR44-VBoxGate-$k" -Action $a -Principal $principal -Settings $settings -Force | Out-Null
            Log "registered task UR44-VBoxGate-$k : $($runs[$k])"
        }
    }
    Log "=== setup-elevated done OK"
} catch {
    Log "FAILED: $_"
}
