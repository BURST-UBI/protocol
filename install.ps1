#Requires -RunAsAdministrator
<#
.SYNOPSIS
    BURST Node installer for Windows.

.DESCRIPTION
    Downloads the BURST daemon, writes a default config, registers a
    Windows Service, and creates a Scheduled Task for auto-updates.

.PARAMETER Uninstall
    Remove the BURST node service, binary, and config.

.EXAMPLE
    # Install (run in elevated PowerShell):
    irm https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.ps1 | iex

    # Uninstall:
    & { irm https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.ps1 } | iex; Install-BurstNode -Uninstall
#>

$ErrorActionPreference = 'Stop'

$Repo        = 'BURST-UBI/protocol'
$ReleaseUrl  = "https://github.com/$Repo/releases/latest/download"
$InstallDir  = "$env:ProgramData\BURST"
$BinPath     = "$InstallDir\burst-daemon.exe"
$ConfigPath  = "$InstallDir\config.toml"
$DataDir     = "$InstallDir\data"
$ServiceName = 'BurstNode'
$TaskName    = 'BurstNodeUpdate'
$Network     = if ($env:BURST_NETWORK) { $env:BURST_NETWORK } else { 'test' }
$Seed        = if ($env:BURST_SEED)    { $env:BURST_SEED }    else { '167.172.83.88:17076' }

function Write-Status($msg)  { Write-Host "[+] $msg" -ForegroundColor Green }
function Write-Warn($msg)    { Write-Host "[!] $msg" -ForegroundColor Yellow }
function Write-Err($msg)     { Write-Host "[x] $msg" -ForegroundColor Red; exit 1 }

# ── Uninstall ────────────────────────────────────────────────────────

function Uninstall-BurstNode {
    Write-Status 'Stopping BURST service...'
    Stop-Service $ServiceName -ErrorAction SilentlyContinue
    sc.exe delete $ServiceName 2>$null | Out-Null

    Write-Status 'Removing scheduled task...'
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue

    Write-Status 'Removing binary and config...'
    Remove-Item $BinPath -Force -ErrorAction SilentlyContinue
    Remove-Item $ConfigPath -Force -ErrorAction SilentlyContinue

    Write-Warn "Data directory $DataDir was NOT removed."
    Write-Host "    Remove manually:  Remove-Item '$InstallDir' -Recurse -Force"
    Write-Status 'BURST node uninstalled.'
    exit 0
}

# ── Check for uninstall flag ─────────────────────────────────────────

if ($args -contains '--uninstall' -or $args -contains '-Uninstall') {
    Uninstall-BurstNode
}

# ── Preflight ────────────────────────────────────────────────────────

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne 'X64') {
    Write-Err "Unsupported architecture: $arch. Only x86_64 is supported."
}

Write-Status "Platform: windows/amd64"
Write-Status "Network: $Network"

# ── Create directories ───────────────────────────────────────────────

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $DataDir    -Force | Out-Null

# ── Download binary ──────────────────────────────────────────────────

$binaryUrl = "$ReleaseUrl/burst-daemon-windows-amd64.exe"
$tmpPath   = "$BinPath.tmp"

Write-Status 'Downloading burst-daemon...'

$wasRunning = $false
if (Get-Service $ServiceName -ErrorAction SilentlyContinue) {
    if ((Get-Service $ServiceName).Status -eq 'Running') {
        $wasRunning = $true
        Stop-Service $ServiceName -Force
        Start-Sleep -Seconds 2
    }
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $wc = New-Object System.Net.WebClient
    $wc.DownloadFile($binaryUrl, $tmpPath)
    Move-Item $tmpPath $BinPath -Force
    Write-Status "Installed $BinPath"
} catch {
    Remove-Item $tmpPath -Force -ErrorAction SilentlyContinue
    Write-Err "Download failed: $_"
}

# ── Write config (skip if exists) ────────────────────────────────────

if (-not (Test-Path $ConfigPath)) {
    $config = @"
network = "Test"
data_dir = "$($DataDir -replace '\\', '/')"
port = 17076
max_peers = 50
enable_rpc = true
rpc_port = 7077
enable_websocket = true
websocket_port = 7078
bootstrap_peers = ["$Seed"]
log_format = "human"
log_level = "info"
work_threads = 2
enable_metrics = true
enable_faucet = false
enable_upnp = true
enable_verification = false
"@
    Set-Content -Path $ConfigPath -Value $config -Encoding UTF8
    Write-Status "Config written to $ConfigPath"
} else {
    Write-Warn "Config already exists at $ConfigPath - not overwriting."
}

# ── Register Windows Service ─────────────────────────────────────────

$existingService = Get-Service $ServiceName -ErrorAction SilentlyContinue
if ($existingService) {
    sc.exe delete $ServiceName 2>$null | Out-Null
    Start-Sleep -Seconds 1
}

$binArgs = "--config `"$ConfigPath`" --data-dir `"$DataDir`" node run"
sc.exe create $ServiceName binPath= "`"$BinPath`" $binArgs" start= auto DisplayName= "BURST Protocol Node" | Out-Null
sc.exe description $ServiceName "BURST Protocol Node - decentralized economic infrastructure" | Out-Null
sc.exe failure $ServiceName reset= 60 actions= restart/5000/restart/10000/restart/30000 | Out-Null

Start-Service $ServiceName
Write-Status "Windows Service '$ServiceName' started."

# ── Scheduled Task for auto-updates ──────────────────────────────────

Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue

$updateScript = @"
`$ErrorActionPreference = 'SilentlyContinue'
`$url = 'https://github.com/$Repo/releases/latest/download/burst-daemon-windows-amd64.exe'
`$sumsUrl = 'https://github.com/$Repo/releases/latest/download/SHA256SUMS'
`$bin = '$BinPath'
try {
    `$sums = (New-Object System.Net.WebClient).DownloadString(`$sumsUrl)
    `$newHash = (`$sums -split "`n" | Where-Object { `$_ -match 'windows-amd64' } | ForEach-Object { (`$_ -split '\s+')[0] })
    if (-not `$newHash) { exit 0 }
    `$oldHash = (Get-FileHash `$bin -Algorithm SHA256).Hash.ToLower()
    if (`$newHash -eq `$oldHash) { exit 0 }
    `$tmp = "`$bin.tmp"
    (New-Object System.Net.WebClient).DownloadFile(`$url, `$tmp)
    `$verify = (Get-FileHash `$tmp -Algorithm SHA256).Hash.ToLower()
    if (`$verify -eq `$newHash) {
        Stop-Service '$ServiceName' -Force
        Start-Sleep -Seconds 2
        Move-Item `$tmp `$bin -Force
        Start-Service '$ServiceName'
    } else {
        Remove-Item `$tmp -Force
    }
} catch {}
"@

$scriptPath = "$InstallDir\update.ps1"
Set-Content -Path $scriptPath -Value $updateScript -Encoding UTF8

$action  = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$scriptPath`""
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date) `
    -RepetitionInterval (New-TimeSpan -Minutes 5)
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Description 'Check for BURST node updates every 5 minutes' | Out-Null

Write-Status 'Auto-update task registered (checks every 5 minutes).'

# ── Summary ──────────────────────────────────────────────────────────

Write-Host ''
Write-Host '================================================' -ForegroundColor White
Write-Host '  BURST node installed successfully' -ForegroundColor White
Write-Host '================================================' -ForegroundColor White
Write-Host ''
Write-Host "  Binary:    $BinPath"
Write-Host "  Config:    $ConfigPath"
Write-Host "  Data:      $DataDir"
Write-Host "  Network:   $Network"
Write-Host "  P2P port:  17076"
Write-Host "  RPC port:  7077"
Write-Host ''
Write-Host '  View logs:    Get-EventLog -LogName Application -Source BurstNode' -ForegroundColor Cyan
Write-Host "  Node status:  Get-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Restart:      Restart-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Stop:         Stop-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Uninstall:    irm https://raw.githubusercontent.com/$Repo/main/install.ps1 | iex; Uninstall-BurstNode" -ForegroundColor Cyan
Write-Host ''
Write-Host '  Auto-updates are enabled (checks every 5 minutes).'
Write-Host "  Edit $ConfigPath to customize your node."
Write-Host ''
