<#
Installs or updates Wallpaper Rotator on Windows 10 or 11.

  install.ps1 [-DryRun] [-Force] [-Version vX.Y.Z]

It checks the machine and what is already installed, installs only the
missing pieces (the GitHub CLI, used to download from the private repo),
then downloads the latest release's setup .exe and installs it silently for
the current user. The setup installs the WebView2 runtime if it is missing.

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
$Owner = 'dflippojr'
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
# Runs gh, dropping its stderr. Windows PowerShell turns a native command's
# stderr into errors, which 'Stop' would make fatal; $LASTEXITCODE says
# whether it worked.
function Invoke-Gh {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & gh @args 2>$null } finally { $ErrorActionPreference = $saved }
}
function Refresh-Path {
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
        [Environment]::GetEnvironmentVariable('Path', 'User')
}

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

if (Get-Command gh -ErrorAction SilentlyContinue) {
    Found "GitHub CLI ($(((gh --version) | Select-Object -First 1).Split(' ')[2]))"
} else {
    Missing 'GitHub CLI'
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw 'Install the GitHub CLI from https://cli.github.com, then run this again.'
    }
    Step 'winget install --id GitHub.cli' {
        winget install --id GitHub.cli -e --accept-source-agreements --accept-package-agreements | Out-Host
        Refresh-Path
    } | Out-Null
}

$webview = $null
foreach ($key in @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$WebView2Id",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$WebView2Id",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$WebView2Id")) {
    $pv = (Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue).pv
    if ($pv -and $pv -ne '0.0.0.0') { $webview = $pv; break }
}
if ($webview) { Found "WebView2 runtime ($webview)" } else { Missing 'WebView2 runtime (the setup installs it)' }

# Prefer the repo owner's account even when another account is active.
function Get-Token {
    $t = Invoke-Gh auth token -u $Owner
    if ($LASTEXITCODE -ne 0 -or -not $t) { $t = Invoke-Gh auth token }
    if ($LASTEXITCODE -ne 0) { return '' }
    return [string]$t
}
$token = ''
if (Get-Command gh -ErrorAction SilentlyContinue) {
    $token = Get-Token
    if (-not $token) {
        Missing 'GitHub login'
        $token = Step 'gh auth login' {
            Say "Log in to GitHub as $Owner (choose GitHub.com, HTTPS, web browser)"
            gh auth login | Out-Host
            Get-Token
        }
    }
}
if ($token) {
    $env:GH_TOKEN = $token
    Invoke-Gh api "repos/$Repo" --silent | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "The GitHub account logged in can't see $Repo. Run: gh auth login (as $Owner)"
    }
    Found "GitHub access to $Repo"
}

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

$tag = $Version
if (-not $tag -and $token) {
    $tag = Invoke-Gh release view --repo $Repo --json tagName -q .tagName
    if ($LASTEXITCODE -ne 0 -or -not $tag) { throw "No releases found in $Repo." }
}
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
    $setup = Step "gh release download $shownTag --repo $Repo --pattern '*x64-setup.exe'" {
        New-Item -ItemType Directory -Path $tmp | Out-Null
        Invoke-Gh release download $tag --repo $Repo --pattern '*x64-setup.exe' --dir $tmp | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Download failed.' }
        Get-ChildItem -Path $tmp -Filter '*x64-setup.exe' | Select-Object -First 1
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
