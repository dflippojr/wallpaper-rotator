<#
Measures the tray app's idle memory on Windows: the app plus the WebView2
(msedgewebview2.exe) processes it started, with the popover hidden.

  powershell -ExecutionPolicy Bypass -File scripts\idle-memory.ps1 [-Wait 60] [-Binary PATH]
  powershell -ExecutionPolicy Bypass -File scripts\idle-memory.ps1 -Id PID

With -Binary (default target\release\wallpaper-rotator.exe), it starts the app
with a fresh WALLPAPER_ROTATOR_HOME under .verify\, waits (default 60 s),
measures, then stops the app and deletes the home. The home holds the three
bundled synthetic favorites, rotation is paused and a current photo is set
with the CLI's `rotate --dry-run` (wallpaper-rotator-cli.exe next to the app),
so the app neither opens the popover for setup nor changes the desktop.
With -Id, it measures an app that is already running and leaves it alone.

It sums each process's private working set ("Working Set - Private"), or its
working set when that counter is unavailable, over the app and every process
it started, directly or through WebView2's browser process.

Only one copy of the app runs per identifier: quit the installed app first, or
build the binary under another identifier (see scripts/idle-memory.sh).
#>
param(
    [int]$Wait = 60,
    [string]$Binary = "target\release\wallpaper-rotator.exe",
    [int]$Id = 0
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

$started = $null
$home_ = $null
try {
    if ($Id -eq 0) {
        $cli = Join-Path (Split-Path -Parent $Binary) "wallpaper-rotator-cli.exe"
        if (-not (Test-Path $Binary)) { throw "no app binary at $Binary" }
        if (-not (Test-Path $cli)) { throw "no CLI next to the app at $cli" }
        New-Item -ItemType Directory -Force .verify | Out-Null
        $home_ = Join-Path (Resolve-Path .verify) ("idle-memory." + [guid]::NewGuid().ToString("N").Substring(0, 8))
        New-Item -ItemType Directory -Force (Join-Path $home_ "favorites") | Out-Null
        Copy-Item app\tests\e2e\fixtures\*.png (Join-Path $home_ "favorites")
        @(
            'sources = ["favorites"]',
            'interval_minutes = 0',
            'paused = true',
            'start_at_login = false',
            'per_display = false',
            'cache_cap_mb = 20'
        ) | Set-Content -Encoding ascii (Join-Path $home_ "config.toml")
        $env:WALLPAPER_ROTATOR_HOME = $home_
        $env:ROTATOR_SIZE = "320x200"
        $env:UNSPLASH_ACCESS_KEY = ""
        $env:PEXELS_API_KEY = ""
        # A current photo on disk, so the paused app has nothing to rotate. A
        # dry run never calls the OS wallpaper setter.
        & $cli rotate --dry-run *> $null
        if ($LASTEXITCODE -ne 0) { throw "the CLI's dry run failed" }
        Remove-Item Env:ROTATOR_SIZE
        $started = Start-Process -FilePath (Resolve-Path $Binary) -PassThru
        $Id = $started.Id
        [Console]::Error.WriteLine("started $Binary (pid $Id); measuring in ${Wait}s")
        Start-Sleep -Seconds $Wait
        if ($started.HasExited) { throw "the app exited before the measurement; is another instance running?" }
    }
    if (-not (Get-Process -Id $Id -ErrorAction SilentlyContinue)) { throw "no process $Id" }

    # The app and every descendant (WebView2's browser, renderer, GPU and
    # utility processes).
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
    $tree = @($Id)
    do {
        $grew = $false
        foreach ($p in $all) {
            if ($tree -contains $p.ParentProcessId -and -not ($tree -contains $p.ProcessId)) {
                $tree += $p.ProcessId
                $grew = $true
            }
        }
    } while ($grew)

    $total = 0.0
    "| Process | PID | Private working set (MB) |"
    "| --- | --- | --- |"
    foreach ($procId in $tree) {
        $proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
        if (-not $proc) { continue }
        $perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess = $procId" -ErrorAction SilentlyContinue
        $bytes = if ($perf -and $perf.WorkingSetPrivate) { [double]$perf.WorkingSetPrivate } else { [double]$proc.WorkingSet64 }
        $mb = $bytes / 1MB
        $total += $mb
        "| ``{0}`` | {1} | {2:N1} |" -f $proc.ProcessName, $procId, $mb
    }
    "| **Total** | | **{0:N1}** |" -f $total
}
finally {
    if ($started -and -not $started.HasExited) {
        # Stop only the app this script started, and its WebView2 children.
        & taskkill /PID $started.Id /T /F *> $null
    }
    if ($home_) {
        Start-Sleep -Seconds 1
        Remove-Item -Recurse -Force $home_ -ErrorAction SilentlyContinue
    }
}
