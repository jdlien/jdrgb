<#
.SYNOPSIS
  Install jdrgb to run at Windows startup (as SYSTEM, no login required).
.DESCRIPTION
  Copies jdrgb.exe to Program Files and registers a Scheduled Task that runs at
  boot (and, if possible, on resume-from-sleep), setting the LEDs to a static
  color. Writes a log to install.log next to this script.
.PARAMETER Color
  Optional RRGGBB hex color to set at boot. Defaults to jdrgb's warm-white preset.
.PARAMETER InstallDir
  Where to place the binary. Defaults to "C:\Program Files\jdrgb".
.PARAMETER NoWake
  Skip the resume-from-sleep trigger (register the startup trigger only).
#>
[CmdletBinding()]
param(
    [string]$Color = "",
    [string]$Config = "",
    [string]$InstallDir = "$env:ProgramFiles\jdrgb",
    [switch]$NoWake
)

$ErrorActionPreference = "Stop"
$TaskName = "jdrgb"

# --- Self-elevate if not running as Administrator ---------------------------
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "Elevating (accept the UAC prompt)..."
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"")
    if ($Color)  { $argList += @("-Color", $Color) }
    if ($Config) { $argList += @("-Config", "`"$Config`"") }
    if ($NoWake) { $argList += "-NoWake" }
    if ($PSBoundParameters.ContainsKey("InstallDir")) { $argList += @("-InstallDir", "`"$InstallDir`"") }
    Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $argList
    return
}

# --- Elevated from here. Log everything and keep the window open. ------------
$log = Join-Path $PSScriptRoot "install.log"
try { Start-Transcript -Path $log -Force | Out-Null } catch {}

try {
    # Locate and copy the binary.
    $source = Join-Path $PSScriptRoot "target\release\jdrgb.exe"
    if (-not (Test-Path $source)) {
        throw "jdrgb.exe not found at $source. Build it first: cargo build --release"
    }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $target = Join-Path $InstallDir "jdrgb.exe"
    Copy-Item -Path $source -Destination $target -Force
    Write-Host "Installed binary: $target"

    # Task components. A config file wins over a single color.
    $taskArgs = "--wait"
    if ($Config) {
        if (-not (Test-Path $Config)) { throw "config file not found: $Config" }
        $confTarget = Join-Path $InstallDir "leds.conf"
        Copy-Item -Path $Config -Destination $confTarget -Force
        Write-Host "Installed config: $confTarget"
        $taskArgs = "load `"$confTarget`" --wait"
    } elseif ($Color) {
        $taskArgs = "$Color --wait"
    }
    $action    = New-ScheduledTaskAction -Execute $target -Argument $taskArgs
    $principal = New-ScheduledTaskPrincipal -UserId "NT AUTHORITY\SYSTEM" -LogonType ServiceAccount -RunLevel Highest
    $settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                    -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 2)

    # Startup fires before login; logon is a belt-and-suspenders re-apply that
    # survives a late controller reset. Both run as SYSTEM.
    $base = @(
        (New-ScheduledTaskTrigger -AtStartup),
        (New-ScheduledTaskTrigger -AtLogOn)
    )

    # Try base + resume-from-sleep; if the wake trigger is rejected, fall back.
    $registered = $false
    if (-not $NoWake) {
        try {
            $evtClass = Get-CimClass -Namespace ROOT\Microsoft\Windows\TaskScheduler -ClassName MSFT_TaskEventTrigger
            $wake = New-CimInstance -CimClass $evtClass -ClientOnly
            $wake.Enabled = $true
            $wake.Subscription = '<QueryList><Query Id="0" Path="System"><Select Path="System">*[System[Provider[@Name=''Microsoft-Windows-Power-Troubleshooter''] and (EventID=1)]]</Select></Query></QueryList>'
            Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger ($base + $wake) `
                -Principal $principal -Settings $settings -Force `
                -Description "Set ASUS Aura LEDs to a static color at boot (jdrgb)." | Out-Null
            Write-Host "Registered task '$TaskName' with startup + logon + resume-from-sleep triggers."
            $registered = $true
        } catch {
            Write-Warning "Could not register the resume-from-sleep trigger: $($_.Exception.Message)"
            Write-Warning "Falling back to startup + logon."
        }
    }

    if (-not $registered) {
        Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $base `
            -Principal $principal -Settings $settings -Force `
            -Description "Set ASUS Aura LEDs to a static color at boot (jdrgb)." | Out-Null
        Write-Host "Registered task '$TaskName' with startup + logon triggers."
    }

    # Run it once now.
    Start-ScheduledTask -TaskName $TaskName
    Start-Sleep -Milliseconds 500
    $info = Get-ScheduledTaskInfo -TaskName $TaskName
    Write-Host ("Ran once: LastTaskResult=0x{0:X8}" -f $info.LastTaskResult)
    $what = if ($Config) { "config '$confTarget'" } elseif ($Color) { "color '$Color'" } else { "warm-white preset" }
    Write-Host "SUCCESS. The $what will be set on every boot."
}
catch {
    Write-Host ""
    Write-Host "INSTALL FAILED: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host $_.ScriptStackTrace
}
finally {
    try { Stop-Transcript | Out-Null } catch {}
    Write-Host ""
    Read-Host "Press Enter to close"
}
