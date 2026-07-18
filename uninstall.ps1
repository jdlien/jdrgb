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

# --- Self-elevate if not running as Administrator ---------------------------
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    $argList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"")
    if ($PSBoundParameters.ContainsKey("InstallDir")) { $argList += @("-InstallDir", "`"$InstallDir`"") }
    Start-Process -FilePath (Get-Process -Id $PID).Path -Verb RunAs -ArgumentList $argList
    return
}

if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "Removed scheduled task '$TaskName'."
} else {
    Write-Host "No scheduled task '$TaskName' found."
}

if (Test-Path $InstallDir) {
    Remove-Item -Path $InstallDir -Recurse -Force
    Write-Host "Removed $InstallDir."
}

Write-Host "Uninstalled. (LEDs keep their current color until the next cold boot.)"
