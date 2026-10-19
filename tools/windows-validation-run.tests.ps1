$ErrorActionPreference = 'Stop'
$runner = Join-Path $PSScriptRoot 'windows-validation-run.ps1'
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
