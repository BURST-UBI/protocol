$ErrorActionPreference = 'Stop'
$runner = Join-Path $PSScriptRoot 'validation/windows-validation-run.ps1'
$cases = @(
    @{ Arguments = @{}; Expected = 'SourceUrl is required' },
    @{ Arguments = @{ SourceUrl = '  '; SourceSha256 = ('a' * 64) }; Expected = 'SourceUrl is required' },
    @{ Arguments = @{ SourceUrl = 'https://example.invalid/source.tar.gz' }; Expected = 'SourceSha256 must be' },
    @{ Arguments = @{ SourceUrl = 'https://example.invalid/source.tar.gz'; SourceSha256 = ('a' * 63) }; Expected = 'SourceSha256 must be' },
    @{ Arguments = @{ SourceUrl = 'https://example.invalid/source.tar.gz'; SourceSha256 = ('g' * 64) }; Expected = 'SourceSha256 must be' },
    @{ Arguments = @{ SourceUrl = 'https://example.invalid/source.tar.gz'; SourceSha256 = (('a' * 64) + "`n") }; Expected = 'SourceSha256 must be' }
)
foreach ($case in $cases) {
    $arguments = $case.Arguments
    $failure = $null
    try { & $runner @arguments }
    catch { $failure = $_.Exception.Message }
    if (-not $failure -or $failure -notmatch $case.Expected) {
        throw "Validation runner did not reject invalid input before doing work: $failure"
    }
}
Write-Output 'PASS: missing or invalid validation inputs fail without prompting or downloading.'

$directory = Join-Path ([IO.Path]::GetTempPath()) ('burst-source-preflight-' + [guid]::NewGuid().ToString('N'))
$source = Join-Path $directory 'source'
New-Item -ItemType Directory -Path $source -Force | Out-Null
$sentinel = Join-Path $source 'old-source.rs'
Set-Content -LiteralPath $sentinel -Value 'preserve existing work'
try {
    $failure = $null
    try {
        & $runner -SourceUrl 'https://example.invalid/source.tar.gz' -SourceSha256 ('a' * 64) -ValidationDirectory $directory
    } catch { $failure = $_.Exception.Message }
    if (-not $failure -or $failure -notmatch 'refusing to overlay') {
        throw "Existing source was not rejected before downloading: $failure"
    }
    if ((Get-Content -LiteralPath $sentinel -Raw).Trim() -ne 'preserve existing work') {
        throw 'Existing source changed'
    }
    foreach ($name in @('source.tar.gz', 'validation.log')) {
        if (Test-Path -LiteralPath (Join-Path $directory $name)) { throw "Unexpected work before preflight: $name" }
    }
} finally {
    Remove-Item -LiteralPath $directory -Recurse -Force
}
Write-Output 'PASS: existing validation source is preserved and rejected before download.'
