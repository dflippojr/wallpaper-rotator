# The full local gate on Windows: runs what the CI `test` job runs on
# windows-latest (format check, clippy with -D warnings, cargo test --workspace).
# Coverage is measured on Linux only; run scripts/check.sh there.
# Works in Windows PowerShell 5.1 and PowerShell 7.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\check.ps1
$ErrorActionPreference = 'Stop'

Set-Location (Split-Path -Parent $PSScriptRoot)

# Test scratch (tempfile directories) stays in the worktree, not the system temp dir.
$scratch = Join-Path (Get-Location) ('.verify\tmp.' + [IO.Path]::GetRandomFileName().Replace('.', ''))
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$env:TMP = $scratch
$env:TEMP = $scratch

function Invoke-Step {
    param([string]$Label, [string[]]$CargoArgs)
    Write-Host ''
    Write-Host "==> cargo $Label"
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: cargo $Label (exit $LASTEXITCODE)"
        exit $LASTEXITCODE
    }
}

try {
    Invoke-Step 'fmt --all --check' @('fmt', '--all', '--check')
    Invoke-Step 'clippy --workspace --all-targets -- -D warnings' @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
    Invoke-Step 'test --workspace' @('test', '--workspace')

    Write-Host ''
    Write-Host '==> coverage skipped: CI measures it on Linux only'
    Write-Host ''
    Write-Host '==> all checks passed'
} finally {
    Remove-Item -Recurse -Force -LiteralPath $scratch -ErrorAction SilentlyContinue
}
