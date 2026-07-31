<#
.SYNOPSIS
  Install jdrgb to run at Windows startup (as SYSTEM, no login required).
.DESCRIPTION
  Copies jdrgb.exe to Program Files and registers a Scheduled Task that runs at
  boot (and, if possible, on resume-from-sleep), setting the LEDs to a static
  color. Writes a log to install.log next to this script.
.PARAMETER Color
  Optional color to set at boot: a preset name (e.g. warmwhite) or RRGGBB hex.
  Defaults to jdrgb's built-in default color (coolwhite).
.PARAMETER InstallDir
  Where to place the binary. Defaults to "C:\Program Files\jdrgb".
.PARAMETER NoWake
  Skip the resume-from-sleep trigger (register the startup trigger only).
.PARAMETER Gpu
  Set the GPU LEDs instead of the motherboard strip.
.PARAMETER All
  Set both the motherboard strip and the GPU LEDs.
#>
[CmdletBinding()]
param(
    [string]$Color = "",
    [string]$Config = "",
    [string]$InstallDir = "$env:ProgramFiles\jdrgb",
    [switch]$NoWake,
    [switch]$Gpu,
    [switch]$All
)

$ErrorActionPreference = "Stop"
$TaskName = "jdrgb"

# --- Reject combinations that would install a permanently failing task -------
if ($Gpu -and $All) {
    throw "-Gpu and -All are mutually exclusive."
}
# -Config combined with -Gpu/-All is fine: a config can carry a `gpu:` line, so
# one file describes the whole machine. If it has no `gpu:` line, jdrgb says so
# rather than failing silently.

# --- Self-elevate if not running as Administrator ---------------------------
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "Elevating (accept the UAC prompt)..."
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"")
    if ($Color)  { $argList += @("-Color", $Color) }
    if ($Config) { $argList += @("-Config", "`"$Config`"") }
    if ($NoWake) { $argList += "-NoWake" }
    if ($Gpu)    { $argList += "-Gpu" }
    if ($All)    { $argList += "-All" }
    if ($PSBoundParameters.ContainsKey("InstallDir")) { $argList += @("-InstallDir", "`"$InstallDir`"") }
    Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $argList
    return
}

# --- Elevated from here. Log everything and keep the window open. ------------
$log = Join-Path $PSScriptRoot "install.log"
try { Start-Transcript -Path $log -Force | Out-Null } catch {}

# Tracked so the script can exit non-zero. It used to swallow every failure and
# return 0, which made the install look successful to anything checking.
$failed = $false

try {
    # Locate and copy the binary.
    $source = Join-Path $PSScriptRoot "target\release\jdrgb.exe"
    if (-not (Test-Path $source)) {
        throw "jdrgb.exe not found at $source. Build it first: cargo build --release"
    }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

    # The task runs this binary as SYSTEM, so wherever it lands must be writable
    # only by administrators — otherwise anything running as an ordinary user
    # could swap jdrgb.exe and get SYSTEM execution at the next boot. The
    # Program Files default is already safe; a custom -InstallDir may not be.
    #
    # Allow-list rather than deny-list: naming the unsafe principals (Users,
    # Everyone) misses the common case of a directory writable by one specific
    # user, such as anything under a profile or %TEMP%.
    $adminOnly = @("BUILTIN\Administrators", "NT AUTHORITY\SYSTEM", "NT SERVICE\TrustedInstaller", "CREATOR OWNER")
    $risky = (Get-Acl -LiteralPath $InstallDir).Access | Where-Object {
        $_.AccessControlType -eq "Allow" -and
        ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -and
        $adminOnly -notcontains $_.IdentityReference.Value
    }
    if ($risky) {
        $who = ($risky.IdentityReference.Value | Sort-Object -Unique) -join ", "
        throw ("$InstallDir is writable by: $who — but the scheduled task would run it as SYSTEM. " +
               "Anyone able to replace jdrgb.exe there would gain SYSTEM execution at the next boot. " +
               "Install somewhere only administrators can write; the default is `"$env:ProgramFiles\jdrgb`".")
    }

    $target = Join-Path $InstallDir "jdrgb.exe"
    Copy-Item -Path $source -Destination $target -Force
    Write-Host "Installed binary: $target"

    # Task components. A config file wins over a single color.
    $targetFlag = if ($Gpu) { " --gpu" } elseif ($All) { " --all" } else { "" }
    $taskArgs = "--wait$targetFlag"
    if ($Config) {
        if (-not (Test-Path $Config)) { throw "config file not found: $Config" }
        $confTarget = Join-Path $InstallDir "leds.conf"
        Copy-Item -Path $Config -Destination $confTarget -Force
        Write-Host "Installed config: $confTarget"
        $taskArgs = "load `"$confTarget`" --wait$targetFlag"
    } elseif ($Color) {
        $taskArgs = "$Color --wait$targetFlag"
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

    # Run it once now. The task uses --wait, so it can legitimately take up to a
    # minute if the controller isn't enumerated yet; poll instead of guessing at
    # a fixed 500ms, which reported a result from before the task had finished.
    Start-ScheduledTask -TaskName $TaskName
    $deadline = (Get-Date).AddSeconds(90)
    do {
        Start-Sleep -Milliseconds 500
        $info = Get-ScheduledTaskInfo -TaskName $TaskName
        $running = (Get-ScheduledTask -TaskName $TaskName).State -eq "Running"
    } while ($running -and (Get-Date) -lt $deadline)

    $what = if ($Config) { "config '$confTarget'" } elseif ($Color) { "color '$Color'" } else { "default preset" }
    $where = if ($Gpu) { "the GPU" } elseif ($All) { "the motherboard strip and the GPU" } else { "the motherboard strip" }

    if ($running) {
        Write-Warning "The task is still running after 90s. It is installed; check Task Scheduler for its result."
    }
    elseif ($info.LastTaskResult -ne 0) {
        # Installed but the first run failed — say so plainly rather than
        # printing SUCCESS regardless, which is what this used to do.
        Write-Host ""
        Write-Host ("INSTALLED, BUT THE FIRST RUN FAILED: LastTaskResult=0x{0:X8}" -f $info.LastTaskResult) -ForegroundColor Red
        Write-Host "The task is registered and will retry on the next boot."
        Write-Host "To see why, run the same command by hand: `"$target`" $taskArgs"
        $failed = $true
    }
    else {
        Write-Host ("Ran once: LastTaskResult=0x{0:X8}" -f $info.LastTaskResult)
        Write-Host "SUCCESS. The $what will be set on $where on every boot."
    }
}
catch {
    Write-Host ""
    Write-Host "INSTALL FAILED: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host $_.ScriptStackTrace
    $failed = $true
}
finally {
    try { Stop-Transcript | Out-Null } catch {}
    Write-Host ""
    Read-Host "Press Enter to close"
}

if ($failed) { exit 1 }
