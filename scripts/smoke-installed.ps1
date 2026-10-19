<#
Destructive: fresh disposable machines only, never a user's desktop.
Usage: .\scripts\smoke-installed.ps1 -Assets .\dist
Disposable VMs outside GitHub Actions must set SMOKE_DISPOSABLE=1.
Compatible with Windows PowerShell 5.1.
#>
param([Parameter(Mandatory = $true)][string]$Assets)
$ErrorActionPreference = 'Stop'
if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted' -and $env:SMOKE_DISPOSABLE -ne '1') {
    throw 'Refusing to install/purge outside a disposable machine (SMOKE_DISPOSABLE=1).'
}
$root = Split-Path -Parent $PSScriptRoot
$evidence = Join-Path $root 'smoke-out'
New-Item -ItemType Directory -Force -Path $evidence | Out-Null
foreach ($name in @('WALLPAPER_ROTATOR_HOME', 'UNSPLASH_ACCESS_KEY', 'PEXELS_API_KEY')) {
    Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
}
function Get-Installed {
    Get-ItemProperty -Path @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    ) -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'Wallpaper Rotator' }
}
function Assert-Absent([string]$Path) {
    if (Test-Path -LiteralPath $Path) { throw "Unexpected leftover/pre-existing path: $Path" }
}
$app = $null
$passed = $false
try {
    if (Get-Installed) { throw 'Wallpaper Rotator is already installed; use a fresh disposable machine.' }
    Assert-Absent (Join-Path $env:LOCALAPPDATA 'Wallpaper Rotator')
    $setups = @(Get-ChildItem -LiteralPath $Assets -Filter '*x64-setup.exe')
    if ($setups.Count -ne 1) { throw 'Expected exactly one x64 NSIS setup.' }
    $setup = Start-Process -FilePath $setups[0].FullName -ArgumentList '/S' -WindowStyle Hidden -PassThru
    $null = $setup.Handle
    if (-not $setup.WaitForExit(180000)) { $setup.Kill(); throw 'Setup exceeded three minutes.' }
    if ($setup.ExitCode -ne 0) { throw "NSIS install failed: $($setup.ExitCode)" }
    $installed = @(Get-Installed)
    if ($installed.Count -ne 1 -or -not $installed[0].InstallLocation) { throw 'NSIS did not register its install location.' }
    $installDir = $installed[0].InstallLocation.Trim('"')
    $exe = Join-Path $installDir 'wallpaper-rotator.exe'
    $cli = Join-Path $installDir 'wallpaper-rotator-cli.exe'
    $pathLines = & $cli paths
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI paths failed.' }
    $pathLines | Tee-Object -FilePath (Join-Path $evidence 'nsis-paths.txt')
    $paths = @{}
    foreach ($line in $pathLines) {
        if ($line -match '^(config|state|images|log|favorites):\s+(.+)$') { $paths[$Matches[1]] = $Matches[2] }
    }
    if ($paths.Count -ne 5) { throw 'Installed CLI did not print all five paths.' }
    foreach ($path in $paths.Values) {
        if (-not [IO.Path]::GetFullPath($path).StartsWith($env:USERPROFILE + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw "Invalid CLI path: $path"
        }
        Assert-Absent $path
    }
    $stdout = Join-Path $evidence 'nsis-stdout.log'
    $stderr = Join-Path $evidence 'nsis-stderr.log'
    $app = Start-Process -FilePath $exe -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    Start-Sleep -Seconds 20
    $app.Refresh()
    if ($app.HasExited) { throw "Installed app exited within 20 seconds: $($app.ExitCode)" }
    "NSIS: installed app alive after 20 seconds (pid $($app.Id))" | Tee-Object -FilePath (Join-Path $evidence 'nsis-alive.txt')
    if (-not (Test-Path -LiteralPath $paths.config)) { throw 'App never initialized its configuration.' }
    & $cli status | Tee-Object -FilePath (Join-Path $evidence 'nsis-status.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI status failed.' }
    $logs = @($stdout, $stderr)
    if (Test-Path -LiteralPath $paths.log) {
        $savedLog = Join-Path $evidence 'nsis-rotator.log'
        Copy-Item -LiteralPath $paths.log -Destination $savedLog
        $logs += $savedLog
    } else {
        'No application log was created on no-key startup.' | Set-Content (Join-Path $evidence 'nsis-log-check.txt')
    }
    $errors = Select-String -LiteralPath $logs -Pattern '(?i)(^|\s)(error|fatal|panic|panicked|failed|could not)(:|\s)'
    if ($errors) { $errors | Out-String | Write-Host; throw 'Application logged an error.' }
    # The real installer owns shutdown, WebView2 cleanup and the NSIS uninstall.
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'install.ps1') -Uninstall -Purge -Verbose |
        Tee-Object -FilePath (Join-Path $evidence 'nsis-uninstall.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Installer purge failed.' }
    # NSIS starts a copied uninstaller; allow that asynchronous process to finish.
    $deadline = (Get-Date).AddSeconds(60)
    while ((Test-Path -LiteralPath $exe) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
    foreach ($path in $paths.Values) { Assert-Absent $path }
    Assert-Absent $exe
    Assert-Absent $cli
    if (Get-Installed) { throw 'NSIS uninstall registry entry remains.' }
    if (Get-Process -Name wallpaper-rotator -ErrorAction SilentlyContinue) { throw 'Installed app still running after uninstall.' }
    'NSIS: all five CLI paths, installed executables and uninstall registration removed' |
        Tee-Object -FilePath (Join-Path $evidence 'nsis-purge.txt')
    # Temporary CI proof: this assertion must fail after the real install/purge.
    Assert-Absent $evidence
    $passed = $true
} finally {
    if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue }
    if ($env:GITHUB_STEP_SUMMARY) {
        "### Installer smoke (Windows)\n\nPassed: $passed. See smoke evidence artifact.".Replace('\n', [Environment]::NewLine) |
            Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Append -Encoding utf8
    }
}
