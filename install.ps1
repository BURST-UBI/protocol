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
$Seed        = if ($env:BURST_SEED)    { $env:BURST_SEED }    else { '' }

function Write-Status($msg)  { Write-Host "[+] $msg" -ForegroundColor Green }
function Write-Warn($msg)    { Write-Host "[!] $msg" -ForegroundColor Yellow }
function Write-Err($msg)     { Write-Host "[x] $msg" -ForegroundColor Red; exit 1 }

function ConvertTo-BurstTomlString([string]$Value) {
    foreach ($character in $Value.ToCharArray()) {
        if ([char]::IsControl($character)) {
            throw 'Config values cannot contain control characters'
        }
    }
    return '"' + $Value.Replace('\', '\\').Replace('"', '\"') + '"'
}

function Get-BurstReleaseChecksum {
    param([string]$Manifest, [string]$Asset)
    $entries = @($Manifest -split "`n" | Where-Object {
        $fields = $_.Trim() -split '\s+'
        $fields.Length -ge 2 -and $fields[1] -ceq $Asset
    })
    if ($entries.Count -ne 1) { throw 'Missing or duplicate release checksum' }
    $fields = $entries[0].Trim() -split '\s+'
    if ($fields.Length -ne 2 -or $fields[0] -notmatch '^[0-9a-fA-F]{64}$') {
        throw 'Invalid release checksum'
    }
    return $fields[0].ToLowerInvariant()
}

function Update-BurstNode {
    param([string]$Binary, [string]$Release, [string]$Service)
    $ErrorActionPreference = 'Stop'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $stage = Join-Path (Split-Path $Binary) ('.burst-update-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    $client = New-Object System.Net.WebClient
    $stopped = $false
    $replaced = $false
    try {
        $asset = 'burst-daemon-windows-amd64.exe'
        $manifest = $client.DownloadString("$Release/SHA256SUMS")
        $expected = Get-BurstReleaseChecksum $manifest $asset
        if ((Get-FileHash $Binary -Algorithm SHA256).Hash -ieq $expected) { return }
        $candidate = Join-Path $stage 'daemon.exe'
        $client.DownloadFile("$Release/$asset", $candidate)
        if ((Get-FileHash $candidate -Algorithm SHA256).Hash -ine $expected) {
            throw 'Update checksum mismatch; installed binary was not changed'
        }
        $wasRunning = (Get-Service $Service).Status -eq 'Running'
        if ($wasRunning) {
            Stop-Service $Service
            (Get-Service $Service).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
            $stopped = $true
        }
        # Retain the old executable, but never roll back a possibly upgraded ledger.
        $backup = "$Binary.previous-" + [guid]::NewGuid().ToString('N')
        [IO.File]::Replace($candidate, $Binary, $backup)
        $replaced = $true
        if ($wasRunning) {
            Start-Service $Service
            (Get-Service $Service).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
        }
        Write-Output "Installed verified update. Previous executable retained at $backup"
    } catch {
        if ($stopped -and -not $replaced) {
            try { Start-Service $Service } catch { Write-Warning "Could not restart unchanged service: $_" }
        }
        throw
    } finally {
        $client.Dispose()
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}

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

switch ($Network.ToLowerInvariant()) {
    'live' { $NetworkName = 'Live'; $NetworkPort = 7076 }
    'test' { $NetworkName = 'Test'; $NetworkPort = 17076 }
    'dev'  { $NetworkName = 'Dev'; $NetworkPort = 27076 }
    default { Write-Err 'BURST_NETWORK must be live, test or dev' }
}
$BootstrapPeers = if ($Seed) { '[' + (ConvertTo-BurstTomlString $Seed) + ']' } else { '[]' }

Write-Status "Platform: windows/amd64"
Write-Status "Network: $Network"

# ── Create directories ───────────────────────────────────────────────

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $DataDir    -Force | Out-Null

# ── Download binary ──────────────────────────────────────────────────

$binaryUrl = "$ReleaseUrl/burst-daemon-windows-amd64.exe"
$tmpPath   = Join-Path $InstallDir ('.burst-install-' + [guid]::NewGuid().ToString('N') + '.exe')

Write-Status 'Downloading burst-daemon...'

$wasRunning = $false
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $wc = New-Object System.Net.WebClient
    $manifest = $wc.DownloadString("$ReleaseUrl/SHA256SUMS")
    $expected = Get-BurstReleaseChecksum $manifest 'burst-daemon-windows-amd64.exe'
    $wc.DownloadFile($binaryUrl, $tmpPath)
    if ((Get-FileHash $tmpPath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
        throw 'Release checksum mismatch; installed binary was not changed'
    }
    $service = Get-Service $ServiceName -ErrorAction SilentlyContinue
    if ($service -and $service.Status -eq 'Running') {
        $wasRunning = $true
        Stop-Service $ServiceName -Force
        (Get-Service $ServiceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    Move-Item $tmpPath $BinPath -Force
    Write-Status "Installed $BinPath"
} catch {
    $failure = $_
    Remove-Item $tmpPath -Force -ErrorAction SilentlyContinue
    if ($wasRunning) { Start-Service $ServiceName -ErrorAction SilentlyContinue }
    Write-Err "Download or checksum validation failed: $failure"
}

# ── Write config (skip if exists) ────────────────────────────────────

if (-not (Test-Path $ConfigPath)) {
    $config = @"
network = "$NetworkName"
data_dir = $(ConvertTo-BurstTomlString ($DataDir -replace '\\', '/'))
port = $NetworkPort
max_peers = 50
enable_rpc = true
rpc_port = 7077
rpc_bind = "127.0.0.1"
enable_websocket = true
websocket_port = 7078
websocket_bind = "127.0.0.1"
bootstrap_peers = $BootstrapPeers
log_format = "human"
log_level = "info"
work_threads = 2
enable_metrics = true
enable_faucet = false
enable_upnp = true
enable_verification = false
"@
    [IO.File]::WriteAllText($ConfigPath, $config, [Text.UTF8Encoding]::new($false))
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

$binArgs = "--windows-service --config `"$ConfigPath`" node run"
sc.exe create $ServiceName binPath= "`"$BinPath`" $binArgs" start= auto DisplayName= "BURST Protocol Node" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Windows service registration failed' }
sc.exe description $ServiceName "BURST Protocol Node - decentralized economic infrastructure" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Windows service description failed' }
sc.exe failure $ServiceName reset= 60 actions= restart/5000/restart/10000/restart/30000 | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Windows service recovery configuration failed' }

Start-Service $ServiceName
Write-Status "Windows Service '$ServiceName' started."

# ── Scheduled Task for auto-updates ──────────────────────────────────

Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue

$updateScript = "`$ErrorActionPreference = 'Stop'`n" +
    "function Get-BurstReleaseChecksum { ${function:Get-BurstReleaseChecksum} }`n" +
    "function Update-BurstNode { ${function:Update-BurstNode} }`n" +
    "Update-BurstNode -Binary '$($BinPath.Replace("'", "''"))' -Release '$ReleaseUrl' -Service '$ServiceName'`n"

$scriptPath = "$InstallDir\update.ps1"
Set-Content -Path $scriptPath -Value $updateScript -Encoding UTF8

$action  = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$scriptPath`""
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date) `
    -RepetitionInterval (New-TimeSpan -Minutes 30)
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Description 'Check for BURST node updates every 30 minutes' | Out-Null

Write-Status 'Auto-update task registered (checks every 30 minutes).'

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
Write-Host "  P2P port:  $NetworkPort"
Write-Host "  RPC port:  7077"
Write-Host ''
Write-Host '  View logs:    Get-WinEvent -FilterHashtable @{LogName="System"; ProviderName="Service Control Manager"} | Where-Object { $_.Message -like "*BurstNode*" } | Select-Object -First 20' -ForegroundColor Cyan
Write-Host "  Node status:  Get-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Restart:      Restart-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Stop:         Stop-Service $ServiceName" -ForegroundColor Cyan
Write-Host "  Uninstall:    irm https://raw.githubusercontent.com/$Repo/main/install.ps1 | iex; Uninstall-BurstNode" -ForegroundColor Cyan
Write-Host ''
Write-Host '  Auto-updates are enabled (checks every 30 minutes).'
Write-Host "  Edit $ConfigPath to customize your node."
Write-Host ''
