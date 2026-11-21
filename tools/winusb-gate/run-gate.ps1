# Runs the G2 measurement series and appends every result block to results\gate-log.txt.
# Usage: powershell -ExecutionPolicy Bypass -File run-gate.ps1 [-Set short|long|all]
param([string]$Set = "all")
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $here "target\release\winusb-gate.exe"
$out = Join-Path $here "results"
New-Item -ItemType Directory -Force $out | Out-Null
$log = Join-Path $out "gate-log.txt"

function Run([string]$name, [string[]]$a) {
    # The mixer app (libusb) opens every interface; it must not run during a test.
    Get-Process ur44-mixer -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    Add-Content $log "`n##### $name : winusb-gate stream $($a -join ' ')  ($(Get-Date -Format s))"
    $r = & $exe stream @a --quiet 2>&1 | ForEach-Object { "$_" }
    Add-Content $log ($r -join "`n")
}

$short = @(
    @("click-48k-d16", @("--rate","48000","--d","16","--signal","click","--secs","15")),
    @("click-48k-d24", @("--rate","48000","--d","24","--signal","click","--secs","15")),
    @("click-48k-2ms-d24", @("--rate","48000","--in-pk","16","--in","2","--d","24","--signal","click","--secs","15")),
    @("click-96k-d8", @("--rate","96000","--d","8","--signal","click","--secs","10")),
    @("click-96k-d16", @("--rate","96000","--d","16","--signal","click","--secs","15")),
    @("click-192k-d8", @("--rate","192000","--d","8","--signal","click","--secs","10")),
    @("click-192k-d16", @("--rate","192000","--d","16","--signal","click","--secs","15")),
    @("click-44k-d16", @("--rate","44100","--d","16","--signal","click","--secs","15")),
    @("sine-48k-nominal-fb", @("--rate","48000","--d","16","--fb","nominal","--signal","sine","--secs","120")),
    @("sine-44k-d16", @("--rate","44100","--d","16","--signal","sine","--secs","60"))
)
$long = @(
    @("sine-48k-1ms-in4-d16-5min", @("--rate","48000","--in-pk","8","--in","4","--d","16","--signal","sine","--secs","300")),
    @("sine-48k-1ms-in2-d16-5min", @("--rate","48000","--in-pk","8","--in","2","--d","16","--signal","sine","--secs","300")),
    @("sine-48k-2ms-in2-d24-5min", @("--rate","48000","--in-pk","16","--in","2","--d","24","--signal","sine","--secs","300")),
    @("sine-96k-1ms-in4-d16-5min", @("--rate","96000","--in-pk","8","--in","4","--d","16","--signal","sine","--secs","300")),
    @("sine-192k-1ms-in4-d16-5min", @("--rate","192000","--in-pk","8","--in","4","--d","16","--signal","sine","--secs","300"))
)
$runs = @()
if ($Set -in "short","all") { $runs += $short }
if ($Set -in "long","all") { $runs += $long }
foreach ($r in $runs) { Run $r[0] $r[1] }
Add-Content $log "`n##### done $(Get-Date -Format s)"
