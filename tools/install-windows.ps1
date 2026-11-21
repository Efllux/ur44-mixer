<#
.SYNOPSIS
  Build UR44 Mixer and install it for the current user (no admin rights, no drivers).

.DESCRIPTION
  - cargo build --release (MSVC toolchain)
  - copies ur44-mixer.exe, ur44-open.exe and ur44ctl.exe to %LOCALAPPDATA%\Programs\UR44 Mixer
  - creates the Start menu shortcut "UR44 Mixer" (runs ur44-open.exe)
  - optionally registers start at login (HKCU\...\Run, the same value the app's Settings toggle uses)
  - optionally starts the app

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\install-windows.ps1 -StartAtLogin -Launch

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\install-windows.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [switch]$StartAtLogin,
    [switch]$Launch,
    [switch]$NoBuild,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA 'Programs\UR44 Mixer'
$exes = 'ur44-mixer.exe', 'ur44-open.exe', 'ur44ctl.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'UR44 Mixer.lnk'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runValue = 'UR44 Mixer'

function Stop-Agent {
    # Ask a running agent to quit (it saves its state), then make sure it is gone.
    $open = Join-Path $dest 'ur44-open.exe'
    $running = Get-Process -Name 'ur44-mixer' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($dest, [StringComparison]::OrdinalIgnoreCase) }
    if (-not $running) { return }
    if (Test-Path $open) {
        Start-Process -FilePath $open -ArgumentList 'quit' -Wait
        $running | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
    }
    Get-Process -Name 'ur44-mixer' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($dest, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force -Confirm:$false
}

if ($Uninstall) {
    Stop-Agent
    Remove-ItemProperty -Path $runKey -Name $runValue -ErrorAction SilentlyContinue
    if (Test-Path $shortcut) { Remove-Item $shortcut -Confirm:$false }
    if (Test-Path $dest) { Remove-Item $dest -Recurse -Force -Confirm:$false }
    Write-Host "Removed UR44 Mixer from $dest (settings and scenes in $env:APPDATA\UR44 Mixer are kept)."
    return
}

if (-not $NoBuild) {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (-not (Test-Path $cargo)) { throw 'cargo not found: install Rust from https://rustup.rs (MSVC toolchain).' }
    } else {
        $cargo = $cargo.Source
    }
    Push-Location $repo
    try {
        # Prefer the MSVC toolchain when rustup has it: the default may be the GNU one.
        $tc = @()
        $rustup = Join-Path (Split-Path $cargo) 'rustup.exe'
        if ((Test-Path $rustup) -and ((& $rustup toolchain list) -match '^stable-x86_64-pc-windows-msvc')) {
            $tc = @('+stable-x86_64-pc-windows-msvc')
        }
        & $cargo @tc build --release --bins
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

$built = Join-Path $repo 'target\release'
foreach ($exe in $exes) {
    if (-not (Test-Path (Join-Path $built $exe))) { throw "missing $built\$exe (build first, or drop -NoBuild)" }
}

Stop-Agent
New-Item -ItemType Directory -Force -Path $dest | Out-Null
foreach ($exe in $exes) {
    Copy-Item -Path (Join-Path $built $exe) -Destination $dest -Force
}
Write-Host "Installed $($exes -join ', ') to $dest"

# Start menu shortcut -> the launcher (shows the window, starting the agent if needed).
$shell = New-Object -ComObject WScript.Shell
$lnk = $shell.CreateShortcut($shortcut)
$lnk.TargetPath = Join-Path $dest 'ur44-open.exe'
$lnk.WorkingDirectory = $dest
$lnk.IconLocation = (Join-Path $dest 'ur44-mixer.exe') + ',0'
$lnk.Description = 'UR44 Mixer: DSP mixer for the Steinberg UR44'
$lnk.Save()
Write-Host "Start menu shortcut: $shortcut"

if ($StartAtLogin) {
    $cmd = '"' + (Join-Path $dest 'ur44-mixer.exe') + '" --background'
    New-ItemProperty -Path $runKey -Name $runValue -Value $cmd -PropertyType String -Force | Out-Null
    Write-Host "Start at login: $runKey\$runValue = $cmd"
}

if ($Launch) {
    Start-Process -FilePath (Join-Path $dest 'ur44-open.exe')
    Write-Host 'UR44 Mixer started.'
}
