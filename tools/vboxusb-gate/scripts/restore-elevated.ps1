# Restore after the VBoxUSB gate (runs elevated from the on-demand task UR44-VBoxGate-Restore):
# unbind the UR44 (Windows reinstalls the best driver = our oem18.inf: usbccgp + WinUSB on MI_00..04),
# restart the usbipd service, and remove every task and file the setup created. usbipd-win stays.
param([Parameter(Mandatory)] [string]$Results)
$log = Join-Path $Results 'restore-elevated.log'
function Log([string]$m) { $l = "$(Get-Date -Format o)  $m"; Add-Content -Path $log -Value $l -Encoding utf8 }
$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')

Log "=== restore start"
$out = & usbipd unbind --hardware-id 0499:150e 2>&1
Log "unbind exit $LASTEXITCODE : $($out -join ' | ')"
Start-Sleep -Seconds 5
Start-Service usbipd -ErrorAction SilentlyContinue
Log "usbipd service: $((Get-Service usbipd -ErrorAction SilentlyContinue).Status)"
Log "usbipd list:`n$((& usbipd list 2>&1) -join "`n")"
Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -match 'VID_0499&PID_150E' } | ForEach-Object {
    $svc = (Get-PnpDeviceProperty -InstanceId $_.InstanceId DEVPKEY_Device_Service -ErrorAction SilentlyContinue).Data
    Log "pnp $($_.Status) $($_.InstanceId) service=$svc"
}
Get-ScheduledTask -TaskName 'UR44-VBoxGate-*' -ErrorAction SilentlyContinue | Where-Object TaskName -ne 'UR44-VBoxGate-Restore' |
    ForEach-Object { Unregister-ScheduledTask -TaskName $_.TaskName -Confirm:$false; Log "removed task $($_.TaskName)" }
Remove-Item 'C:\ProgramData\ur44-vboxgate\vboxusb-gate.exe' -Force -ErrorAction SilentlyContinue
Log "=== restore done (task and folder removed next)"
# Last: remove this task and folder. The running script file can be deleted on Windows after load.
Unregister-ScheduledTask -TaskName 'UR44-VBoxGate-Restore' -Confirm:$false -ErrorAction SilentlyContinue
Remove-Item 'C:\ProgramData\ur44-vboxgate' -Recurse -Force -ErrorAction SilentlyContinue
