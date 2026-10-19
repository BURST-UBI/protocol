$ErrorActionPreference = 'Stop'
$installer = Join-Path $PSScriptRoot '../install.ps1'
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path $installer), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) { throw ($parseErrors | Out-String) }
foreach ($name in @('Get-BurstReleaseChecksum', 'Update-BurstNode', 'ConvertTo-BurstTomlString')) {
    $definition = $ast.Find({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq $name
    }, $true)
    if (-not $definition) { throw "Installer helper not found: $name" }
    Invoke-Expression $definition.Extent.Text
}

$asset = 'burst-daemon-windows-amd64.exe'
$hash = 'a' * 64
if ((Get-BurstReleaseChecksum "$hash  $asset`r`n" $asset) -cne $hash) {
    throw 'Valid checksum was not retained'
}
if ((Get-BurstReleaseChecksum "$($hash.ToUpperInvariant())  $asset`n" $asset) -cne $hash) {
    throw 'Hex case normalization failed'
}
foreach ($manifest in @('', "$hash  $asset.extra", "wrong  $asset",
        "$hash  $asset`n$hash  $asset", "$hash  $asset  extra")) {
    $rejected = $false
    try { Get-BurstReleaseChecksum $manifest $asset | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Accepted invalid checksum manifest: $manifest" }
}
Write-Output 'Installer syntax and checksum parser checks passed.'
# The scheduled task receives serialized helper bodies, including their parameters.
$generated = "function Get-BurstReleaseChecksum { ${function:Get-BurstReleaseChecksum} }`n" +
    "function Update-BurstNode { ${function:Update-BurstNode} }"
$generatedErrors = $null
[System.Management.Automation.Language.Parser]::ParseInput($generated, [ref]$tokens, [ref]$generatedErrors) | Out-Null
if ($generatedErrors.Count -gt 0) { throw ($generatedErrors | Out-String) }
Invoke-Expression $generated
if ((Get-BurstReleaseChecksum "$hash  $asset" $asset) -cne $hash) { throw 'Serialized checksum helper lost its parameters' }
if ((ConvertTo-BurstTomlString 'a\b"c') -cne '"a\\b\"c"') { throw 'TOML quoting failed' }
foreach ($value in @("new`nline", "tab`tvalue", "nul$([char]0)value")) {
    $rejected = $false
    try { ConvertTo-BurstTomlString $value | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'TOML control character was accepted' }
}
foreach ($code in (0..31) + (127..159)) {
    $rejected = $false
    try { ConvertTo-BurstTomlString "before$([char]$code)after" | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw "TOML control character $code was accepted" }
}

# Updates use temporary files and mocked HTTP/service commands, never real services.
function New-Object([string]$TypeName) {
    if ($TypeName -ne 'System.Net.WebClient') { throw "Unexpected object: $TypeName" }
    $client = [pscustomobject]@{}
    $client | Add-Member ScriptMethod DownloadString { param($url) return $script:manifest }
    $client | Add-Member ScriptMethod DownloadFile { param($url, $path)
        if ($script:downloadFails) { throw 'Simulated HTTP failure' }
        [IO.File]::WriteAllText($path, $script:body)
    }
    $client | Add-Member ScriptMethod Dispose { }
    return $client
}
function Get-Service([string]$Name) {
    if ($Name -ne 'MockBurst') { throw 'Unexpected service name' }
    $service = [pscustomobject]@{ Status = $script:serviceState }
    $service | Add-Member ScriptMethod WaitForStatus { param($status, $timeout)
        if ($script:serviceState -ne $status) { throw 'Unexpected service state' }
    }
    return $service
}
function Stop-Service([string]$Name) { $script:stops++; $script:serviceState = 'Stopped' }
function Start-Service([string]$Name) {
    $script:starts++
    if ($script:startFails) { throw 'Simulated startup failure' }
    $script:serviceState = 'Running'
}

foreach ($case in @('success', 'unchanged', 'mismatch', 'duplicate', 'download', 'stopped', 'start-failure')) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ('burst-update-contract-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $directory | Out-Null
    try {
        $binary = Join-Path $directory 'installed daemon.exe'
        [IO.File]::WriteAllText($binary, 'old binary')
        $script:body = if ($case -eq 'unchanged') { 'old binary' } else { 'new binary' }
        $hasher = [Security.Cryptography.SHA256]::Create()
        try { $digest = [BitConverter]::ToString($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($script:body))).Replace('-', '').ToLowerInvariant() }
        finally { $hasher.Dispose() }
        $script:manifest = "$digest  $asset"
        if ($case -eq 'duplicate') { $script:manifest += "`n$script:manifest" }
        if ($case -eq 'mismatch') { $script:body = 'tampered' }
        $script:downloadFails = $case -eq 'download'
        $script:startFails = $case -eq 'start-failure'
        $script:serviceState = if ($case -eq 'stopped') { 'Stopped' } else { 'Running' }
        $script:starts = 0
        $script:stops = 0
        $failed = $false
        try { Update-BurstNode $binary 'https://example.invalid/release' 'MockBurst' }
        catch { $failed = $true }
        $expectedFailure = $case -in @('mismatch', 'duplicate', 'download', 'start-failure')
        if ($failed -ne $expectedFailure) { throw "Wrong update result for $case" }
        $replaced = $case -in @('success', 'stopped', 'start-failure')
        $expectedBody = if ($replaced) { 'new binary' } else { 'old binary' }
        if ([IO.File]::ReadAllText($binary) -cne $expectedBody) { throw "Wrong installed binary for $case" }
        $expectedStops = if ($case -in @('success', 'start-failure')) { 1 } else { 0 }
        if ($script:stops -ne $expectedStops -or $script:starts -ne $expectedStops) { throw "Wrong service operations for $case" }
        $files = @(Get-ChildItem -LiteralPath $directory -Force)
        if ($files.Count -ne (1 + [int]$replaced)) { throw "Staging files leaked for $case" }
        if ($replaced) {
            $backup = $files | Where-Object Name -Like '*.previous-*'
            if ([IO.File]::ReadAllText($backup.FullName) -cne 'old binary') { throw 'Previous executable was not retained' }
        }
    } finally {
        Remove-Item -LiteralPath $directory -Recurse -Force
    }
}
Write-Output 'Updater checksum, service state, failure and backup checks passed.'
