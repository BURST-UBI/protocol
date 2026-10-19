# Run only on a disposable Windows validation machine as administrator.
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$directory = 'C:\burst-validation'
New-Item -ItemType Directory -Force -Path $directory | Out-Null
Start-Transcript -Path (Join-Path $directory 'setup.log') -Append
try {
    $installer = Join-Path $directory 'vs_buildtools.exe'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://aka.ms/vs/17/release/vs_buildtools.exe' -OutFile $installer
    $signature = Get-AuthenticodeSignature $installer
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw 'Build Tools installer signature is invalid'
    }
    $process = Start-Process -FilePath $installer -ArgumentList '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended' -Wait -PassThru
    if ($process.ExitCode -notin @(0, 3010)) { throw "Build Tools failed: $($process.ExitCode)" }

    $rustup = Join-Path $directory 'rustup-init.exe'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile $rustup
    Invoke-WebRequest -UseBasicParsing -Uri 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe.sha256' -OutFile "$rustup.sha256"
    $expected = ((Get-Content "$rustup.sha256" -Raw).Trim() -split '\s+')[0]
    if ((Get-FileHash $rustup -Algorithm SHA256).Hash -ine $expected) { throw 'Rustup checksum mismatch' }
    $env:RUSTUP_HOME = Join-Path $directory 'rustup'
    $env:CARGO_HOME = Join-Path $directory 'cargo'
    & $rustup -y --profile minimal --default-toolchain nightly-2026-10-11 --no-modify-path
    if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain installation failed' }

    $archive = Join-Path $directory 'powershell.zip'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/PowerShell/PowerShell/releases/download/v7.6.6/PowerShell-7.6.6-win-x64.zip' -OutFile $archive
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ine '02fe458be20493fbdf43f61ea20610b811ee6c738ab1676c61b9cfcd1a33c860') {
        throw 'PowerShell checksum mismatch'
    }
    Expand-Archive -Path $archive -DestinationPath (Join-Path $directory 'pwsh') -Force
    Write-Output 'Windows build and validation tools installed.'
} finally {
    Stop-Transcript
}
