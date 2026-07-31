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
.PARAMETER Tray
  Also install jdrgb-tray.exe and start it at logon: a notification-area menu
  for picking a preset colour. This is a second, separate task — the boot task
  runs as SYSTEM in session 0 and cannot show a tray icon.
.PARAMETER TrayOnly
  Install the tray and skip the boot task entirely. Implies -Tray.

  jdrgb commits to the controller's flash on every solid set, so the color
  already survives a cold boot without anything running. That makes the boot
  task insurance rather than a requirement — and it isn't free, since it fires
  at startup, at logon and on resume, writing flash each time to reassert a
  color that was already there. If you don't need the insurance, don't pay for
  it. See "GPU persistence" in the README.
.PARAMETER TrayUser
  Internal. The SID of the user the tray task belongs to, captured before
  elevation. Not meant to be passed by hand.
#>
[CmdletBinding()]
param(
    [string]$Color = "",
    [string]$Config = "",
    [string]$InstallDir = "$env:ProgramFiles\jdrgb",
    [switch]$NoWake,
    [switch]$Gpu,
    [switch]$All,
    [switch]$Tray,
    [switch]$TrayOnly,
    [string]$TrayUser = ""
)

$ErrorActionPreference = "Stop"
$TaskName = "jdrgb"
$TrayTaskName = "jdrgb-tray"

if ($TrayOnly) { $Tray = $true }

# --- Reject combinations that would install a permanently failing task -------
if ($Gpu -and $All) {
    throw "-Gpu and -All are mutually exclusive."
}
# -Color and -Config only ever configured the boot task. Silently ignoring them
# would look like the color had been set up when nothing was listening.
if ($TrayOnly -and ($Color -or $Config)) {
    throw ("-TrayOnly registers no boot task, so -Color/-Config would have nothing to configure. " +
           "Set the color once by hand — it persists — e.g. `"jdrgb $Color --all`".")
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
    if ($Tray)   {
        # Carry the *invoking* user's SID across the elevation boundary. If UAC
        # is answered with a different administrator's credentials, the elevated
        # process is that administrator — so reading the identity over there
        # would register the tray for the wrong account, and it would never
        # appear for the person who ran this.
        $sid = ([Security.Principal.WindowsIdentity]::GetCurrent()).User.Value
        $argList += @("-Tray", "-TrayUser", $sid)
    }
    if ($TrayOnly) { $argList += "-TrayOnly" }
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

    $trayTarget = Join-Path $InstallDir "jdrgb-tray.exe"
    if ($Tray) {
        $traySource = Join-Path $PSScriptRoot "target\release\jdrgb-tray.exe"
        if (-not (Test-Path $traySource)) {
            throw "jdrgb-tray.exe not found at $traySource. Build it first: cargo build --release"
        }
        # A running tray holds a lock on its own image, so an upgrade has to
        # stop it first. Matched on the full path so a development copy running
        # from target\release is left alone. taskkill without /F posts WM_CLOSE,
        # which the tray answers by removing its icon before exiting.
        Get-Process -Name "jdrgb-tray" -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $trayTarget } |
            ForEach-Object {
                & taskkill.exe /PID $_.Id 2>&1 | Out-Null
                if (-not $_.WaitForExit(3000)) { $_.Kill() }
            }
        Copy-Item -Path $traySource -Destination $trayTarget -Force
        Write-Host "Installed binary: $trayTarget"
    }

    # --- The boot task, which -TrayOnly skips entirely -----------------------
    # Optional, and genuinely so: jdrgb commits to the controller's flash on
    # every solid set, so the color already survives a cold boot with nothing
    # running. This is insurance for the cases that clear the controllers — a
    # BIOS update, a CMOS reset, reinstalling vendor software — and it is paid
    # for in flash writes, once per trigger, reasserting a color that was
    # already there.
    if (-not $TrayOnly) {
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
    }
    elseif (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        # Upgrading an install that had one. Leaving it would keep writing flash
        # on every boot behind your back, which is the thing -TrayOnly avoids.
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Host "Removed the existing boot task '$TaskName' (-TrayOnly)."
    }

    # --- The tray, which needs a task of its own ----------------------------
    # The boot task above runs as SYSTEM so it can fire before anyone logs in.
    # That is exactly why it cannot host the tray: a SYSTEM task runs in session
    # 0, which has no interactive desktop and therefore no notification area.
    # So the tray gets a second task, running as the user, unelevated.
    if ($Tray) {
        $sid = if ($TrayUser) { $TrayUser } else { ([Security.Principal.WindowsIdentity]::GetCurrent()).User.Value }
        try {
            $who = (New-Object Security.Principal.SecurityIdentifier($sid)).Translate([Security.Principal.NTAccount]).Value
        } catch { $who = $sid }

        $trayAction = New-ScheduledTaskAction -Execute $trayTarget
        # Nothing the tray does needs elevation — it spawns jdrgb.exe, which
        # talks to the controller over userspace HID and NVAPI.
        $trayPrincipal = New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
        # No execution time limit: unlike the boot task this is meant to stay
        # resident, and the default would terminate it after three days.
        $traySettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                            -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
        $trayTrigger = New-ScheduledTaskTrigger -AtLogOn -User $sid

        Register-ScheduledTask -TaskName $TrayTaskName -Action $trayAction -Trigger $trayTrigger `
            -Principal $trayPrincipal -Settings $traySettings -Force `
            -Description "jdrgb tray menu for picking a preset color." | Out-Null
        Write-Host "Registered task '$TrayTaskName' to start at logon for $who."

        # Start it now rather than making them log out to see it.
        Start-ScheduledTask -TaskName $TrayTaskName
        Start-Sleep -Milliseconds 800
        if (Get-Process -Name "jdrgb-tray" -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $trayTarget }) {
            Write-Host "Tray is running — look for the coloured dot in the notification area."
        } else {
            Write-Warning "The tray task was registered but the process isn't running yet; it will start at your next logon."
        }
    }

    if ($TrayOnly) {
        Write-Host ""
        Write-Host "SUCCESS. The tray will start at logon."
        Write-Host "No boot task: the color is already held in the controllers' flash, so it"
        Write-Host "survives a cold boot on its own. Set it once with jdrgb and leave it."
        return
    }

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
