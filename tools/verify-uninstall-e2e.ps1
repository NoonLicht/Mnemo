# Mnemo install/uninstall E2E verification (run as the current user).
# Usage: powershell -ExecutionPolicy Bypass -File tools\verify-uninstall-e2e.ps1 <installer-path>
param(
  [Parameter(Mandatory = $true)][string]$Installer
)

$ErrorActionPreference = 'Stop'
function Test-DataPaths {
  $paths = @(
    "$env:APPDATA\mnemo",
    "$env:APPDATA\com.user.mnemo",
    "$env:LOCALAPPDATA\com.user.mnemo"
  )
  $paths | ForEach-Object { "{0} -> {1}" -f $_, (Test-Path $_) }
  $run = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -ErrorAction SilentlyContinue
  "Run entry 'Mnemo' -> $($null -ne ($run.PSObject.Properties | Where-Object Name -eq 'Mnemo'))"
}

Write-Host '=== BEFORE INSTALL (current machine state) ==='
Test-DataPaths

Write-Host '=== INSTALLING silently ==='
$p = Start-Process -FilePath $Installer -ArgumentList '/S' -PassThru -Wait
"installer exit: $($p.ExitCode)"

Write-Host '=== POST-INSTALL state ==='
Test-DataPaths
$inst = @(
  "$env:LOCALAPPDATA\mnemo",
  "$env:LOCALAPPDATA\Programs\mnemo",
  "$env:ProgramFiles\mnemo"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($inst) { "install dir: $inst" } else { 'install dir: NOT FOUND (check productName/installMode)' }
$uninst = Get-ChildItem "$env:APPDATA\Microsoft\Windows\Start Menu\Programs" -Recurse -Filter '*ensaku*' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName
$uninst | ForEach-Object { "shortcut: $_" }

Write-Host '=== UNINSTALLING silently ==='
# NSIS uninstaller relaunches itself elevated-free; /S silent.
$unExe = Get-ChildItem "$inst\Uninstall*.exe" -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
if (-not $unExe) { throw 'Uninstaller not found' }
$u = Start-Process -FilePath $unExe -ArgumentList '/S' -PassThru -Wait
"uninstaller exit: $($u.ExitCode)"

Start-Sleep -Seconds 3
Write-Host '=== POST-UNINSTALL state (expect all False) ==='
Test-DataPaths
"install dir still exists -> $(if ($inst) { (Test-Path $inst) } else { 'n/a (never found)' })"
