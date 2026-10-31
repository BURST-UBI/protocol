param(
    [string]$SourceUrl,
    [string]$SourceSha256,
    [string]$ValidationDirectory = 'C:\burst-validation'
)
$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($SourceUrl)) {
    throw 'SourceUrl is required for Windows validation.'
}
if ($SourceSha256 -notmatch '\A[0-9a-fA-F]{64}\z') {
    throw 'SourceSha256 must be a 64-character hexadecimal checksum.'
}
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$directory = [IO.Path]::GetFullPath($ValidationDirectory)
$archive = Join-Path $directory 'source.tar.gz'
$source = Join-Path $directory 'source'
if (Test-Path -LiteralPath $source) {
    throw 'Source directory already exists; refusing to overlay a validation archive. Reuse the existing workspace with its recorded workflow.'
}
New-Item -ItemType Directory -Force -Path $directory | Out-Null
Start-Transcript -Path (Join-Path $directory 'validation.log') -Append
try {
    try { Invoke-WebRequest -UseBasicParsing -Uri $SourceUrl -OutFile $archive }
    catch { throw 'Could not download private validation source archive' }
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ine $SourceSha256) {
        throw 'Validation source checksum mismatch'
    }
    New-Item -ItemType Directory -Force -Path $source | Out-Null
    & tar.exe -xzf $archive -C $source
    if ($LASTEXITCODE -ne 0) { throw 'Source extraction failed' }
    Set-Location $source
    $env:RUSTUP_HOME = Join-Path $directory 'rustup'
    $env:CARGO_HOME = Join-Path $directory 'cargo'
    $env:RUSTUP_TOOLCHAIN = 'nightly-2026-10-11'
    $env:CARGO_BUILD_JOBS = '1'
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    $env:CARGO_PROFILE_TEST_DEBUG = '0'
    $env:PATH = "$directory\cargo\bin;$env:PATH"
    $cargo = Join-Path $directory 'cargo\bin\cargo.exe'
    $pwsh = Join-Path $directory 'pwsh\pwsh.exe'
    $windowsPowerShell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
    foreach ($scriptHost in @($windowsPowerShell, $pwsh)) {
        & $scriptHost -NoProfile -NonInteractive -File tools/windows-validation-run.tests.ps1
        if ($LASTEXITCODE -ne 0) { throw 'Validation runner input checks failed' }
        & $scriptHost -NoProfile -NonInteractive -File tools/install-checksum.tests.ps1
        if ($LASTEXITCODE -ne 0) { throw 'Installer contract checks failed' }
    }
    & $cargo test -p burst-daemon --locked
    if ($LASTEXITCODE -ne 0) { throw 'Native daemon tests failed' }
    & $cargo build -p burst-daemon --locked
    if ($LASTEXITCODE -ne 0) { throw 'Native daemon build failed' }
    & $pwsh -NoProfile -File tools/windows-service-smoke.ps1 -Daemon target/debug/burst-daemon.exe
    if ($LASTEXITCODE -ne 0) { throw 'Native service lifecycle checks failed' }
    Write-Output 'PASS: Windows installer contracts, daemon tests and native service lifecycle.'
} finally {
    Stop-Transcript
}
