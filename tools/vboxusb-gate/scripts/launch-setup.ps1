# Unelevated: close the UR44 Mixer (it holds MI_04 and would block the driver swap), then start
# setup-elevated.ps1 with ONE UAC prompt. Does not wait; follow results\setup-elevated.log.
param([Parameter(Mandatory)] [string]$Msi, [Parameter(Mandatory)] [string]$Marker)
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Get-Process ur44-mixer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Remove-Item $Marker -ErrorAction SilentlyContinue
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$a = "-NoProfile -ExecutionPolicy Bypass -File `"$here\setup-elevated.ps1`" -User `"$user`" -Msi `"$Msi`" -Marker `"$Marker`""
Start-Process powershell.exe -Verb RunAs -ArgumentList $a -WindowStyle Normal
"launched elevated setup for $user"
