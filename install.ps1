<#
Installs, updates or uninstalls Wallpaper Rotator on Windows 10 or 11.

  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.ps1)))

Options go after the closing parenthesis, for example `... ))) -DryRun`.
From a clone: powershell -ExecutionPolicy Bypass -File install.ps1 [options]

It checks the machine and what is already installed, then downloads the
latest release's setup .exe and installs it silently for the current user.
The setup installs the WebView2 runtime if it is missing.

  -DryRun   only report what it finds and what it would do
  -Verbose  print every check instead of a progress bar
  -Force    reinstall even if this version is already installed
  -Version  install a specific release tag instead of the latest
  -Uninstall  remove the app, its start-at-login entry and its cache;
              settings stay unless -Purge is given too, and the favorites
              folder is never touched

Run it again any time to update.
#>
param(
    [switch]$DryRun,
    [switch]$Force,
    [switch]$Verbose,
    [switch]$Uninstall,
    [switch]$Purge,
    [string]$Version = ''
)

$ErrorActionPreference = 'Stop'
$Repo = 'dflippojr/wallpaper-rotator'
$AppName = 'Wallpaper Rotator'
$WebView2Id = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
if ($Purge -and -not $Uninstall) { throw '-Purge goes with -Uninstall.' }
if ($Uninstall) { $Activity = "Uninstalling $AppName" } else { $Activity = "Installing $AppName" }
$Log = Join-Path ([IO.Path]::GetTempPath()) 'wallpaper-rotator-install.log'
# One progress bar, with the details in $Log; -Verbose and -DryRun print
# every line instead.
$Bar = -not ($DryRun -or $Verbose)
# Built at run time so this file stays ASCII (Windows PowerShell 5.1 reads
# scripts without a byte-order mark as ANSI).
$Tick = [string][char]0x2713
$Cross = [string][char]0x2717

# Windows PowerShell 5.1 may default to older TLS, which GitHub refuses.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
if ($Bar) { Set-Content -Path $Log -Value "$Activity, $(Get-Date)" -Encoding UTF8 }

function Write-Line([string]$Text, [string]$Color = '') {
    if ($Bar) { Add-Content -Path $Log -Value $Text -Encoding UTF8 }
    elseif ($Color) { Write-Host $Text -ForegroundColor $Color }
    else { Write-Host $Text }
}
function Say([string]$Message) { Write-Line "==> $Message" }
function Note([string]$Message) { Write-Line "    $Message" }
function Found([string]$Message) { Write-Line "    found    $Message" }
function Missing([string]$Message) { Write-Line "    missing  $Message" 'Yellow' }
# A line the user sees either way.
function Show([string]$Text, [string]$Color = 'Gray') {
    if ($Bar) { Add-Content -Path $Log -Value $Text -Encoding UTF8 }
    Write-Host $Text -ForegroundColor $Color
}
# Moves the bar (and logs the step).
function Step-Progress([int]$Percent, [string]$Status) {
    Say $Status
    if ($Bar) { Write-Progress -Activity $Activity -Status $Status -PercentComplete $Percent }
}
# Runs $Action, or in a dry run only says what it would do. Returns the
# action's output.
function Step([string]$Description, [scriptblock]$Action) {
    if ($DryRun) { Note "would run: $Description"; return $null }
    & $Action
}

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

# Streams $Url to $File, filling the bar from 20% to 80% as bytes arrive.
# (Invoke-WebRequest's own progress display makes downloads very slow.)
function Save-Download([string]$Url, [string]$File, [long]$Size) {
    Add-Type -AssemblyName System.Net.Http
    $client = New-Object System.Net.Http.HttpClient
    try {
        $client.DefaultRequestHeaders.UserAgent.ParseAdd('wallpaper-rotator-installer')
        $response = $client.GetAsync($Url, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        [void]$response.EnsureSuccessStatusCode()
        if ($response.Content.Headers.ContentLength) { $Size = $response.Content.Headers.ContentLength }
        $in = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $out = [IO.File]::Create($File)
        try {
            $buffer = New-Object byte[] 262144
            $done = 0L
            $clock = [Diagnostics.Stopwatch]::StartNew()
            while (($n = $in.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $out.Write($buffer, 0, $n)
                $done += $n
                if ($Bar -and $clock.ElapsedMilliseconds -ge 150) {
                    $clock.Restart()
                    $pct = [Math]::Min(80, 20 + [int](60 * $done / [Math]::Max($Size, 1)))
                    $status = 'Downloading {0:N1} of {1:N1} MB' -f ($done / 1MB), ($Size / 1MB)
                    Write-Progress -Activity $Activity -Status $status -PercentComplete $pct
                }
            }
        } finally {
            $out.Dispose()
            $in.Dispose()
        }
    } finally {
        $client.Dispose()
    }
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("wallpaper-rotator-" + [Guid]::NewGuid().ToString('N'))
try {
    if ($Bar) { Write-Host $Activity }

    # -------------------------------------------------------------- detect --

    Step-Progress 2 'Checking this machine'
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

    # ----------------------------------------------------------- uninstall --

    if ($Uninstall) {
        $removed = New-Object System.Collections.Generic.List[string]
        # Deletes the folders that exist, noting them. WebView2 can hold a lock
        # file for a moment after the app exits, so retry for a few seconds.
        function Remove-Folder([string[]]$Paths) {
            foreach ($path in $Paths) {
                if (Test-Path $path) {
                    Step "remove $path" {
                        for ($try = 1; ; $try++) {
                            try { Remove-Item -Recurse -Force $path; break }
                            catch { if ($try -ge 20) { throw }; Start-Sleep -Milliseconds 500 }
                        }
                    } | Out-Null
                    $removed.Add($path)
                }
            }
        }
        Step-Progress 10 "Stopping $AppName"
        Step 'stop the running app and its WebView2 helpers' {
            Get-Process -Name 'wallpaper-rotator' -ErrorAction SilentlyContinue | Stop-Process -Force
            # Only the helpers using this app's data folder, not other apps'.
            Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" -ErrorAction SilentlyContinue |
                Where-Object { $_.CommandLine -like '*dev.dflippojr.wallpaper-rotator*' } |
                ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        } | Out-Null

        Step-Progress 25 'Removing start at login'
        $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
        if ((Get-ItemProperty -Path $run -ErrorAction SilentlyContinue).$AppName) {
            Step 'remove the start-at-login entry' {
                Remove-ItemProperty -Path $run -Name $AppName
            } | Out-Null
            $removed.Add('the start-at-login entry')
        }

        Step-Progress 40 'Removing the app'
        $installed = Get-Installed
        if ($installed) {
            $uninstaller = $installed.UninstallString.Trim('"')
            Step "run $uninstaller /S" {
                $p = Start-Process -FilePath $uninstaller -ArgumentList '/S' -PassThru
                $null = $p.Handle
                $p.WaitForExit()
                # The uninstaller copies itself and returns before it's done.
                $deadline = (Get-Date).AddSeconds(60)
                while ((Get-Installed) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
            } | Out-Null
            $removed.Add("$AppName $($installed.DisplayVersion)")
        }

        Step-Progress 75 'Removing its cache'
        Remove-Folder @(
            (Join-Path $env:LOCALAPPDATA 'dflippojr\Wallpaper Rotator'),
            (Join-Path $env:LOCALAPPDATA 'dev.dflippojr.wallpaper-rotator'))
        if ($Purge) {
            Step-Progress 90 'Removing settings'
            Remove-Folder @(Join-Path $env:APPDATA 'dflippojr\Wallpaper Rotator')
        }
        # The vendor folders, once empty.
        foreach ($parent in @((Join-Path $env:APPDATA 'dflippojr'), (Join-Path $env:LOCALAPPDATA 'dflippojr'))) {
            if ((Test-Path $parent) -and -not (Get-ChildItem $parent)) {
                Step "remove empty $parent" { Remove-Item $parent } | Out-Null
            }
        }

        if ($DryRun) { Say 'Dry run: nothing was changed.'; return }
        if ($Bar) { Write-Progress -Activity $Activity -Completed }
        if ($removed.Count -eq 0) {
            Show "$Tick $AppName wasn't installed; nothing to remove." 'Green'
        } else {
            Say ("Removed: " + ($removed -join '; '))
            Show "$Tick $AppName is uninstalled." 'Green'
            if (-not $Purge) { Show '  Your settings stay, in case you reinstall. To remove them too, add -Purge.' }
        }
        Show '  Your favorites folder is untouched: Pictures\Wallpaper Favorites'
        return
    }

    # -------------------------------------------------------- dependencies --

    Step-Progress 6 "Checking what's already installed"
    $webview = $null
    foreach ($key in @(
            "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$WebView2Id",
            "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$WebView2Id",
            "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$WebView2Id")) {
        $pv = (Get-ItemProperty -Path $key -Name pv -ErrorAction SilentlyContinue).pv
        if ($pv -and $pv -ne '0.0.0.0') { $webview = $pv; break }
    }
    if ($webview) { Found "WebView2 runtime ($webview)" } else { Missing 'WebView2 runtime (the setup installs it)' }

    # ------------------------------------------------------------ versions --

    $installed = Get-Installed
    $current = ''
    if ($installed) { $current = $installed.DisplayVersion }

    Step-Progress 15 'Finding the latest release'
    if ($Version) { $api = "https://api.github.com/repos/$Repo/releases/tags/$Version" }
    else { $api = "https://api.github.com/repos/$Repo/releases/latest" }
    $headers = @{ 'User-Agent' = 'wallpaper-rotator-installer' }
    # A token (set in CI) lifts the 60-an-hour limit for anonymous calls.
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    # Invoke-RestMethod's own progress display would flash over the bar.
    $savedProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        $release = Invoke-RestMethod -Uri $api -Headers $headers
    } catch {
        throw "Couldn't read the release from GitHub ($api): $($_.Exception.Message)"
    } finally {
        $ProgressPreference = $savedProgress
    }
    $tag = $release.tag_name
    $asset = $release.assets | Where-Object { $_.name -like '*x64-setup.exe' } | Select-Object -First 1
    if (-not $asset) { throw "Release $tag has no Windows setup file." }
    $shownCurrent = $current
    if (-not $shownCurrent) { $shownCurrent = 'none' }
    Say "Installed: $shownCurrent.  Installing: $tag"
    if ($current -and ("v$current" -eq $tag) -and -not $Force) {
        if ($Bar) { Write-Progress -Activity $Activity -Completed }
        Show "$Tick $AppName $current is already up to date. Use -Force to reinstall." 'Green'
        return
    }

    # ------------------------------------------------------------- install --

    Step-Progress 20 "Downloading $tag"
    $setup = Step "download $($asset.browser_download_url)" {
        New-Item -ItemType Directory -Path $tmp | Out-Null
        $file = Join-Path $tmp $asset.name
        Save-Download $asset.browser_download_url $file $asset.size
        $file
    }

    Step-Progress 82 'Stopping the running app'
    Step 'stop the running app' {
        Get-Process -Name 'wallpaper-rotator' -ErrorAction SilentlyContinue | Stop-Process -Force
    } | Out-Null

    Step-Progress 85 'Installing'
    Step 'run the setup silently (/S) for the current user' {
        $p = Start-Process -FilePath $setup -ArgumentList '/S' -PassThru
        # Reading the handle now keeps ExitCode available after it exits.
        $null = $p.Handle
        $pct = 85
        $start = Get-Date
        while (-not $p.HasExited) {
            if ($Bar) {
                $secs = [int]((Get-Date) - $start).TotalSeconds
                Write-Progress -Activity $Activity -Status "Installing (${secs}s)" -PercentComplete $pct
            }
            Start-Sleep -Milliseconds 500
            if ($pct -lt 94) { $pct++ }
        }
        $p.WaitForExit()
        if ($p.ExitCode -ne 0) { throw "Setup failed with exit code $($p.ExitCode)." }
    } | Out-Null

    Step-Progress 97 "Starting $AppName"
    Step 'start the app' { Start-Process -FilePath (Get-AppExe) } | Out-Null

    if ($DryRun) {
        Say 'Dry run: nothing was changed.'
    } else {
        if ($Bar) { Write-Progress -Activity $Activity -Completed }
        $now = Get-Installed
        Show "$Tick $AppName $($now.DisplayVersion) is installed and running." 'Green'
        Show '  Click its icon in the taskbar tray (under ^ if hidden); on a first install, pick photo sources under the gear.'
    }
} catch {
    if (-not $Bar) { throw }
    # A short message instead of a red error block; the details are in the log.
    Write-Progress -Activity $Activity -Completed
    Add-Content -Path $Log -Value "error: $($_.Exception.Message)" -Encoding UTF8
    Write-Host "$Cross $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "  Full log: $Log"
} finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
}
