# Install the package built by make-package.ps1. Run elevated (one UAC prompt).
# Trusts the package certificate (LocalMachine Root and TrustedPublisher), then adds the INF to
# the driver store and installs it on the UR44. Everything is logged to out\install.log.
$ErrorActionPreference = "Continue"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$out = Join-Path $here "out"
$log = Join-Path $out "install.log"
Start-Transcript -Path $log -Force | Out-Null
try {
    $cer = Join-Path $out "ur44-winusb.cer"
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Format-List Thumbprint, Subject
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Format-List Thumbprint, Subject
    & pnputil.exe /add-driver (Join-Path $out "ur44-winusb.inf") /install
    Write-Host "pnputil exit code: $LASTEXITCODE"
    Start-Sleep -Seconds 3
    & pnputil.exe /enum-devices /connected /class USBDevice
    Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like "*VID_0499*" } |
        Format-Table Status, Class, FriendlyName, InstanceId -AutoSize | Out-String -Width 250 | Write-Host
} finally {
    Stop-Transcript | Out-Null
}
