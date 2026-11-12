# Runs in a separate, unelevated Windows PowerShell process after an in-app update.
# Paths arrive through the environment; never interpolate them into executable script text.
$ErrorActionPreference = 'Stop'
$canRestart = $false
try {
    $parent = Get-Process -Id ([int]$env:KOMANDO_UPDATE_PARENT) -ErrorAction SilentlyContinue
    # Tell Komando the helper is running before it closes its sessions and exits.
    [IO.File]::WriteAllText($env:KOMANDO_UPDATE_READY, 'ready')
    if ($null -ne $parent -and -not $parent.WaitForExit(120000)) {
        throw 'Komando did not close in time. Please try the update again.'
    }
    $canRestart = $true
    $arguments = @('/i', ('"{0}"' -f $env:KOMANDO_UPDATE_MSI), '/passive', '/norestart', 'KOMANDO_UPDATING=1')
    $options = @{
        FilePath = (Join-Path $env:SystemRoot 'System32\msiexec.exe')
        Wait = $true
        PassThru = $true
    }
    if ($env:KOMANDO_UPDATE_ALL_USERS -eq '1') {
        $arguments += @('ALLUSERS=1', 'KOMANDO_SCOPE=perMachine')
        # Elevate only Windows Installer. Komando reopens as the original Windows user.
        $options.Verb = 'RunAs'
    } else {
        $arguments += @('ALLUSERS=2', 'MSIINSTALLPERUSER=1', 'KOMANDO_SCOPE=perUser')
    }
    $options.ArgumentList = $arguments
    $installer = Start-Process @options
    # 3010 means installation succeeded but Windows needs a reboot later. Never reboot here.
    if ($installer.ExitCode -notin @(0, 3010)) {
        throw "Windows Installer could not finish the update (code $($installer.ExitCode)). Please try again or run the installer manually."
    }
    Remove-Item -LiteralPath $env:KOMANDO_UPDATE_ERROR -Force -ErrorAction SilentlyContinue
} catch {
    $message = "The update did not finish: $($_.Exception.Message)"
    # Even a full or unwritable disk must not prevent reopening the existing app.
    try { [IO.File]::WriteAllText($env:KOMANDO_UPDATE_ERROR, $message) } catch { }
} finally {
    Remove-Item -LiteralPath $env:KOMANDO_UPDATE_READY -Force -ErrorAction SilentlyContinue
}
if ($canRestart) {
    try {
        Start-Process -FilePath $env:KOMANDO_UPDATE_EXE
    } catch {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.MessageBox]::Show(
            'Please open Komando from the Start menu to finish the update.', 'Komando update') | Out-Null
    }
}
