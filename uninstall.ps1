<#
.SYNOPSIS
  Remove the jdrgb scheduled tasks and installed binaries — and, if the Home
  Assistant bridge was installed, the light it created in Home Assistant.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = "$env:ProgramFiles\jdrgb"
)

$ErrorActionPreference = "Stop"
$TaskName = "jdrgb"
$TrayTaskName = "jdrgb-tray"
$HaTaskName = "jdrgb-ha"

# --- Self-elevate if not running as Administrator ---------------------------
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"")
    if ($PSBoundParameters.ContainsKey("InstallDir")) { $argList += @("-InstallDir", "`"$InstallDir`"") }
    Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $argList
    return
}

# --- The Home Assistant bridge goes first, and in order ----------------------
# 1. Stop it, and wait until neither it nor any jdrgb.exe it started is running:
#    a cleanup racing a resident would have the resident republish the entity.
# 2. Delete the light from HA with `jdrgb-ha.exe --unpublish`, using the config
#    of the user the bridge ran as.
# 3. Only then remove its task and binary, below.
$haTask = Get-ScheduledTask -TaskName $HaTaskName -ErrorAction SilentlyContinue
$haConf = $null
if ($haTask) {
    # Whose ha.conf: the task's own principal, which may be stored as a SID or
    # as an account name.
    $principal = $haTask.Principal.UserId
    $sid = $principal
    if ($principal -notmatch '^S-1-') {
        try { $sid = (New-Object Security.Principal.NTAccount($principal)).Translate([Security.Principal.SecurityIdentifier]).Value } catch {}
    }
    $profileDir = (Get-CimInstance Win32_UserProfile -Filter "SID='$sid'" -ErrorAction SilentlyContinue).LocalPath
    if ($profileDir) { $haConf = Join-Path $profileDir "AppData\Local\jdrgb\ha.conf" }

    Disable-ScheduledTask -TaskName $HaTaskName -ErrorAction SilentlyContinue | Out-Null
    Stop-ScheduledTask -TaskName $HaTaskName -ErrorAction SilentlyContinue
}

$haExe = $null
try { $haExe = Join-Path (Resolve-Path -LiteralPath $InstallDir -ErrorAction Stop).ProviderPath "jdrgb-ha.exe" } catch {}
if ($haExe) {
    $cliExe = Join-Path (Split-Path $haExe) "jdrgb.exe"
    # Ending the bridge ends its children too — it runs them in a kill-on-close
    # job — but wait for both to be gone rather than assume it.
    Get-Process -Name "jdrgb-ha" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $haExe } |
        ForEach-Object { $_.Kill(); Write-Host "Stopped the running bridge." }
    # Gone means gone: no bridge or jdrgb.exe from here still running, and the
    # bridge's singleton no longer held — it outlives the process only while
    # some other bridge, perhaps under another account, still has it. Carrying
    # on otherwise would race a cleanup against a live bridge that republishes
    # what it deletes, and delete binaries out from under running processes.
    $deadline = (Get-Date).AddSeconds(30)
    $held = $null
    while ($true) {
        $running = @(Get-Process -Name "jdrgb-ha", "jdrgb" -ErrorAction SilentlyContinue |
                         Where-Object { $_.Path -eq $haExe -or $_.Path -eq $cliExe })
        $singleton = [System.Threading.Mutex]::TryOpenExisting("Global\jdrgb-ha", [ref]$held)
        if ($held) { $held.Dispose(); $held = $null }
        if ($running.Count -eq 0 -and -not $singleton) { break }
        if ((Get-Date) -ge $deadline) {
            $what = if ($running.Count) { "$($running.Count) jdrgb process(es) still running" } else { "another jdrgb-ha still holding Global\jdrgb-ha" }
            Write-Host "UNINSTALL STOPPED: $what after 30s. Nothing was removed; stop it and run this again." -ForegroundColor Red
            exit 1
        }
        Start-Sleep -Milliseconds 250
    }

    if ((Test-Path -LiteralPath $haExe) -and $haConf -and (Test-Path -LiteralPath $haConf)) {
        Write-Host "Removing the light from Home Assistant..."
        $errFile = [IO.Path]::GetTempFileName()
        # A windows-subsystem program: Start-Process -Wait is what makes PowerShell
        # wait for it and hand back its exit code.
        $p = Start-Process -FilePath $haExe -ArgumentList @("--unpublish", "--config", "`"$haConf`"") `
                 -Wait -PassThru -NoNewWindow -RedirectStandardError $errFile
        $err = (Get-Content -LiteralPath $errFile -Raw -ErrorAction SilentlyContinue)
        Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
        if ($p.ExitCode -eq 0) {
            Write-Host "Removed the light from Home Assistant."
        } else {
            Write-Warning "Couldn't remove the light from Home Assistant (exit $($p.ExitCode)): $err"
            Write-Warning "Delete the device by hand under Settings > Devices > MQTT in Home Assistant."
        }
    } elseif ($haTask) {
        Write-Warning "No bridge config found, so the light was not removed from Home Assistant."
        Write-Warning "Delete the device by hand under Settings > Devices > MQTT in Home Assistant."
    }
}

foreach ($name in @($TaskName, $TrayTaskName, $HaTaskName)) {
    if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $name -Confirm:$false
        Write-Host "Removed scheduled task '$name'."
    } else {
        Write-Host "No scheduled task '$name' found."
    }
}

# --- Remove only what install.ps1 put there ---------------------------------
# Deliberately not `Remove-Item -Recurse` on $InstallDir. That ran elevated,
# accepted any directory, and treated the value as a wildcard pattern, so a typo
# like "-InstallDir 'C:\Program Files'" would have deleted far more than jdrgb.
# Instead: resolve the path, require our own binary in it as proof it's really an
# install, delete the known artifacts by literal path, and only drop the
# directory itself once it's empty.
$Artifacts = @("jdrgb.exe", "jdrgb-tray.exe", "jdrgb-ha.exe", "leds.conf")

$resolved = $null
try { $resolved = (Resolve-Path -LiteralPath $InstallDir -ErrorAction Stop).ProviderPath } catch {}

if (-not $resolved) {
    Write-Host "No install directory at $InstallDir."
}
elseif (-not (Test-Path -LiteralPath (Join-Path $resolved "jdrgb.exe"))) {
    Write-Warning "$resolved does not contain jdrgb.exe — refusing to delete anything there."
    Write-Warning "If this really is the install directory, remove it by hand."
}
else {
    # A running tray holds its own image open, so it has to go before the file
    # can. Matched by full path, not by name: `Stop-Process -Name jdrgb-tray`
    # would also kill a copy someone is running from a build directory.
    $trayExe = Join-Path $resolved "jdrgb-tray.exe"
    Get-Process -Name "jdrgb-tray" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $trayExe } |
        ForEach-Object {
            # taskkill without /F posts WM_CLOSE, which the tray answers by
            # removing its icon and then exiting. Kill() outright would leave a
            # ghost icon in the notification area until something forced a
            # refresh. CloseMainWindow() is no use here — the tray's window is
            # never shown, so it has no MainWindowHandle to close.
            & taskkill.exe /PID $_.Id 2>&1 | Out-Null
            if (-not $_.WaitForExit(3000)) { $_.Kill() }
            Write-Host "Stopped the running tray."
        }

    foreach ($name in $Artifacts) {
        $file = Join-Path $resolved $name
        if (Test-Path -LiteralPath $file) {
            Remove-Item -LiteralPath $file -Force
            Write-Host "Removed $file."
        }
    }
    $leftover = @(Get-ChildItem -LiteralPath $resolved -Force -ErrorAction SilentlyContinue)
    if ($leftover.Count -eq 0) {
        Remove-Item -LiteralPath $resolved -Force
        Write-Host "Removed $resolved."
    } else {
        Write-Host "Left $resolved in place — it still contains $($leftover.Count) file(s) jdrgb did not install."
    }
}

if ($haConf -and (Test-Path -LiteralPath $haConf)) {
    # Left on purpose: on an install that uses one it holds a password, which is
    # for its owner to keep or delete deliberately.
    Write-Host "Left the bridge's config in place: $haConf"
}

Write-Host "Uninstalled. (LEDs keep their current color until the next cold boot.)"
