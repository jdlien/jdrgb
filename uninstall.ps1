<#
.SYNOPSIS
  Remove the jdrgb scheduled task and installed binary.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = "$env:ProgramFiles\jdrgb"
)

$ErrorActionPreference = "Stop"
$TaskName = "jdrgb"
$TrayTaskName = "jdrgb-tray"

# --- Self-elevate if not running as Administrator ---------------------------
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"")
    if ($PSBoundParameters.ContainsKey("InstallDir")) { $argList += @("-InstallDir", "`"$InstallDir`"") }
    Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $argList
    return
}

foreach ($name in @($TaskName, $TrayTaskName)) {
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
$Artifacts = @("jdrgb.exe", "jdrgb-tray.exe", "leds.conf")

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

Write-Host "Uninstalled. (LEDs keep their current color until the next cold boot.)"
