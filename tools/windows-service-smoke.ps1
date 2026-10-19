param([Parameter(Mandatory=$true)][string]$Daemon)
$ErrorActionPreference = 'Stop'
$serviceName = 'BurstNode'
if (Get-Service $serviceName -ErrorAction SilentlyContinue) {
    throw 'Refusing to replace an existing BurstNode service during validation'
}
$binary = (Resolve-Path $Daemon).Path
$directory = Join-Path ([IO.Path]::GetTempPath()) ('burst-service-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
$config = Join-Path $directory 'node.toml'
$data = (Join-Path $directory 'ledger') -replace '\\', '/'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
$rpcPort = $listener.LocalEndpoint.Port
$listener.Stop()
@"
network = "Dev"
data_dir = "$data"
port = 0
bootstrap_peers = []
enable_representative = false
enable_rpc = true
rpc_bind = "127.0.0.1"
rpc_port = $rpcPort
enable_websocket = false
enable_upnp = false
enable_faucet = false
"@ | Set-Content -Path $config -Encoding utf8NoBOM

function Wait-NodeRpc {
    $deadline = (Get-Date).AddSeconds(60)
    do {
        try {
            $response = Invoke-RestMethod -Uri "http://127.0.0.1:$rpcPort" -Method Post `
                -ContentType 'application/json' -Body '{"action":"telemetry"}' -TimeoutSec 2
            if ([uint64]$response.result.block_count -ge 1 -and
                $response.result.network -eq 'dev' -and -not $response.result.faucet_enabled) { return }
        } catch { }
        if ((Get-Service $serviceName).Status -eq 'Stopped') { throw 'Service stopped before RPC was ready' }
        Start-Sleep -Milliseconds 200
    } while ((Get-Date) -lt $deadline)
    throw 'Service did not expose its local RPC'
}

try {
    $command = "`"$binary`" --windows-service --config `"$config`" node run"
    sc.exe create $serviceName binPath= $command start= demand | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not create isolated test service' }
    foreach ($attempt in 1..2) {
        Start-Service $serviceName
        Wait-NodeRpc
        Stop-Service $serviceName -NoWait
        (Get-Service $serviceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        $service = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
        if ($service.ExitCode -ne 0) { throw "Unclean service stop: $($service.ExitCode)" }
    }
    # An explicit missing config must fail startup, not start an accidental Dev node.
    $missing = Join-Path $directory 'missing.toml'
    sc.exe config $serviceName binPath= "`"$binary`" --windows-service --config `"$missing`" node run" | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not configure negative startup check' }
    try { Start-Service $serviceName } catch { }
    (Get-Service $serviceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    $service = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
    if ($service.ExitCode -eq 0) { throw 'Invalid config did not produce a service failure' }
    Write-Output 'Windows service start, RPC, graceful stop, restart and invalid-config checks passed.'
} catch {
    $failure = $_
    # Diagnose the isolated test service before cleanup hides the SCM result.
    sc.exe qc $serviceName | Out-Host
    sc.exe queryex $serviceName | Out-Host
    Get-CimInstance Win32_Service -Filter "Name='$serviceName'" |
        Select-Object Name, State, PathName, ExitCode, ServiceSpecificExitCode | Format-List | Out-Host
    Get-WinEvent -FilterHashtable @{LogName='System'; ProviderName='Service Control Manager'; StartTime=(Get-Date).AddMinutes(-5)} -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match 'BurstNode' } |
        Select-Object TimeCreated, Id, Message | Format-List | Out-Host
    # Distinguish an invalid daemon/config from an SCM-only startup failure.
    # This probe uses only this test's disposable ledger, never an installed node.
    if ((Get-Service $serviceName -ErrorAction SilentlyContinue).Status -eq 'Stopped') {
        $stdout = Join-Path $directory 'foreground.stdout'
        $stderr = Join-Path $directory 'foreground.stderr'
        $probe = Start-Process -FilePath $binary -ArgumentList @('--config', "`"$config`"", 'node', 'run') `
            -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        if (-not $probe.WaitForExit(3000)) {
            Write-Output 'Foreground diagnostic reached a running process; terminating only this disposable probe.'
            $probe.Kill()
            $probe.WaitForExit()
        }
        Write-Output "Foreground diagnostic exit: $($probe.ExitCode)"
        Get-Content $stdout, $stderr -ErrorAction SilentlyContinue | Out-Host
    }
    throw $failure
} finally {
    $service = Get-Service $serviceName -ErrorAction SilentlyContinue
    if ($service) {
        if ($service.Status -ne 'Stopped') {
            Stop-Service $serviceName -NoWait -ErrorAction SilentlyContinue
            $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        }
        sc.exe delete $serviceName | Out-Host
    }
    Remove-Item -LiteralPath $directory -Recurse -Force
}
