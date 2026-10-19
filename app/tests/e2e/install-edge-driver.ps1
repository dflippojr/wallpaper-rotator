# Match the WebView2 runtime used by Tauri, rather than a different Edge channel.
param([string]$DriverDirectory = 'target/e2e-tools/edge')
$ErrorActionPreference = 'Stop'
$runtimeRoots = @(
    (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft/EdgeWebView/Application'),
    "$env:ProgramFiles/Microsoft/EdgeWebView/Application",
    "$env:LOCALAPPDATA/Microsoft/EdgeWebView/Application"
)
$versions = foreach ($runtimeRoot in $runtimeRoots) {
    if (Test-Path -LiteralPath $runtimeRoot) {
        Get-ChildItem -LiteralPath $runtimeRoot -Directory |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
            ForEach-Object { [version]$_.Name }
    }
}
$runtimeVersion = $versions | Sort-Object -Descending | Select-Object -First 1
if (-not $runtimeVersion) { throw 'WebView2 runtime not found; install the Tauri Windows prerequisites first.' }
New-Item -ItemType Directory -Force -Path $DriverDirectory | Out-Null
$driverRoot = (Resolve-Path -LiteralPath $DriverDirectory).Path
$archive = Join-Path $driverRoot 'edgedriver.zip'
Invoke-WebRequest -UseBasicParsing -Uri "https://msedgedriver.microsoft.com/$runtimeVersion/edgedriver_win64.zip" -OutFile $archive
Expand-Archive -LiteralPath $archive -DestinationPath $driverRoot -Force
$driver = Join-Path $driverRoot 'msedgedriver.exe'
& $driver --version
if ($LASTEXITCODE -ne 0) { throw 'Downloaded Edge WebDriver could not run.' }
if ($env:GITHUB_ENV) { "NATIVE_WEBDRIVER=$driver" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append }
Write-Host "WebView2 $runtimeVersion; driver: $driver"
