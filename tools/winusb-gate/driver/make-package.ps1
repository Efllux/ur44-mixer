# Build a signed driver package for ur44-winusb.inf (no admin rights needed).
#   1. a self-signed code-signing certificate (CN=ur44-mixer WinUSB package), created in
#      CurrentUser\My, exported to out\ur44-winusb.cer;
#   2. a catalog (makecat) over the INF, signed with that certificate (signtool);
#   3. the private key is deleted afterwards, as libwdi does: the certificate becomes trusted
#      machine-wide (install.ps1), so nobody should be able to sign anything else with it.
# Needs makecat.exe and signtool.exe from the Windows 10/11 SDK.
param([string]$Sdk = "")
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$out = Join-Path $here "out"
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item (Join-Path $here "ur44-winusb.inf") $out -Force

if (-not $Sdk) {
    $Sdk = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName "x64\makecat.exe") } |
        Sort-Object Name | Select-Object -Last 1 | ForEach-Object { Join-Path $_.FullName "x64" }
}
$makecat = Join-Path $Sdk "makecat.exe"
$signtool = Join-Path $Sdk "signtool.exe"

$cdf = @"
[CatalogHeader]
Name=ur44-winusb.cat
ResultDir=.\
PublicVersion=0x0000001
EncodingType=0x00010001
CATATTR1=0x10010001:OSAttr:2:6.3,2:10.0

[CatalogFiles]
<hash>ur44-winusb.inf=ur44-winusb.inf
"@
Set-Content -Path (Join-Path $out "ur44-winusb.cdf") -Value $cdf -Encoding ascii

Push-Location $out
try {
    Remove-Item ur44-winusb.cat -ErrorAction SilentlyContinue
    & $makecat -v ur44-winusb.cdf
    if ($LASTEXITCODE -ne 0) { throw "makecat failed ($LASTEXITCODE)" }

    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=ur44-mixer WinUSB package" `
        -CertStoreLocation Cert:\CurrentUser\My -KeyUsage DigitalSignature -KeyExportPolicy NonExportable `
        -NotAfter (Get-Date).AddYears(10)
    Export-Certificate -Cert $cert -FilePath (Join-Path $out "ur44-winusb.cer") | Out-Null
    & $signtool sign /v /fd sha256 /sha1 $cert.Thumbprint /s My ur44-winusb.cat
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    # Delete the certificate and its private key from the user store.
    Remove-Item -Path ("Cert:\CurrentUser\My\" + $cert.Thumbprint) -DeleteKey
    Set-Content -Path (Join-Path $out "thumbprint.txt") -Value $cert.Thumbprint -Encoding ascii
    Write-Host "package ready in $out, certificate thumbprint $($cert.Thumbprint)"
} finally {
    Pop-Location
}
