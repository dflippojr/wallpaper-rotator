<#
Installs or updates Wallpaper Rotator on Windows 10 or 11.

  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.ps1)))

Options go after the closing parenthesis, for example `... ))) -DryRun`.
From a clone: powershell -ExecutionPolicy Bypass -File install.ps1 [options]

It checks the machine and what is already installed, then downloads the
latest release's setup .exe and installs it silently for the current user.
The setup installs the WebView2 runtime if it is missing.

  -DryRun   only report what it finds and what it would do
  -Force    reinstall even if this version is already installed
  -Version  install a specific release tag instead of the latest

Run it again any time to update.
#>
param(
    [switch]$DryRun,
    [switch]$Force,
    [string]$Version = ''
)

$ErrorActionPreference = 'Stop'
$Repo = 'dflippojr/wallpaper-rotator'
$AppName = 'Wallpaper Rotator'
$WebView2Id = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'

function Say([string]$Message) { Write-Host "==> $Message" }
function Note([string]$Message) { Write-Host "    $Message" }
function Found([string]$Message) { Write-Host '    found    ' -ForegroundColor Green -NoNewline; Write-Host $Message }
function Missing([string]$Message) { Write-Host '    missing  ' -ForegroundColor Yellow -NoNewline; Write-Host $Message }
# Runs $Action, or in a dry run only says what it would do. Returns the
# action's output.
function Step([string]$Description, [scriptblock]$Action) {
    if ($DryRun) { Note "would run: $Description"; return $null }
    & $Action
}
# Windows PowerShell 5.1 may default to older TLS, which GitHub refuses, and
# its progress bar slows downloads a lot.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

# ------------------------------------------------------------------ detect --

Say 'Checking this machine'
$os = Get-CimInstance Win32_OperatingSystem
Note "$($os.Caption) (build $($os.BuildNumber)), $env:PROCESSOR_ARCHITECTURE"
if ([int]$os.BuildNumber -lt 17763) {
    throw 'Wallpaper Rotator needs Windows 10 version 1809 or later.'
}
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    Note 'ARM64: the x64 build runs under Windows emulation.'
} elseif ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "Only 64-bit Windows is supported (this is $env:PROCESSOR_ARCHITECTURE)."
}

# ------------------------------------------------------------- dependencies --

Say "Checking what's already installed"

$webview = $null
foreach ($key in @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$WebView2Id",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$WebView2Id",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$WebView2Id")) {
    $pv = (Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue).pv
    if ($pv -and $pv -ne '0.0.0.0') { $webview = $pv; break }
}
if ($webview) { Found "WebView2 runtime ($webview)" } else { Missing 'WebView2 runtime (the setup installs it)' }

# ---------------------------------------------------------------- versions --

function Get-Installed {
    $keys = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*')
    Get-ItemProperty -Path $keys -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq $AppName } | Select-Object -First 1
}

function Get-AppExe {
    $installed = Get-Installed
    if ($installed -and $installed.InstallLocation) {
        $exe = Join-Path ($installed.InstallLocation.Trim('"')) 'wallpaper-rotator.exe'
        if (Test-Path $exe) { return $exe }
    }
    return (Join-Path $env:LOCALAPPDATA "$AppName\wallpaper-rotator.exe")
}

$installed = Get-Installed
$current = ''
if ($installed) { $current = $installed.DisplayVersion }

if ($Version) { $api = "https://api.github.com/repos/$Repo/releases/tags/$Version" }
else { $api = "https://api.github.com/repos/$Repo/releases/latest" }
try {
    $headers = @{ 'User-Agent' = 'wallpaper-rotator-installer' }
    # A token (set in CI) lifts the 60-an-hour limit for anonymous calls.
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $release = Invoke-RestMethod -Uri $api -Headers $headers
} catch {
    throw "Couldn't read the release from GitHub ($api): $($_.Exception.Message)"
}
$tag = $release.tag_name
$asset = $release.assets | Where-Object { $_.name -like '*x64-setup.exe' } | Select-Object -First 1
if (-not $asset) { throw "Release $tag has no Windows setup file." }
$shownCurrent = $current
if (-not $shownCurrent) { $shownCurrent = 'none' }
$shownTag = $tag
if (-not $shownTag) { $shownTag = 'latest release' }
Say "Installed: $shownCurrent.  Installing: $shownTag"
if ($current -and $tag -and ("v$current" -eq $tag) -and -not $Force) {
    Say 'Already up to date. Use -Force to reinstall.'
    return
}

# ----------------------------------------------------------------- install --

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("wallpaper-rotator-" + [Guid]::NewGuid().ToString('N'))
try {
    Say "Downloading $shownTag"
    $setup = Step "download $($asset.browser_download_url)" {
        New-Item -ItemType Directory -Path $tmp | Out-Null
        $file = Join-Path $tmp $asset.name
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $file -UseBasicParsing
        Get-Item $file
    }

    Say 'Installing'
    Step 'stop the running app' {
        Get-Process -Name 'wallpaper-rotator' -ErrorAction SilentlyContinue | Stop-Process -Force
    } | Out-Null
    Step 'run the setup silently (/S) for the current user' {
        $p = Start-Process -FilePath $setup.FullName -ArgumentList '/S' -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "Setup failed with exit code $($p.ExitCode)." }
    } | Out-Null

    Say "Starting $AppName"
    Step 'start the app' { Start-Process -FilePath (Get-AppExe) } | Out-Null
} finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
}

if ($DryRun) {
    Say 'Dry run: nothing was changed.'
} else {
    $now = Get-Installed
    Say "Done. $AppName $($now.DisplayVersion) is installed."
    Note 'Its icon is in the taskbar tray (click ^ if hidden): click it, then the gear to add your Unsplash key.'
}
