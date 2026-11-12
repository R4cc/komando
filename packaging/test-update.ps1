# Exercise the actual update helper with process calls replaced, so CI never installs or
# elevates anything. Windows PowerShell 5.1 is the runtime used by the shipped app.
$ErrorActionPreference = 'Stop'
$komandoTestDir = Join-Path ([IO.Path]::GetTempPath()) ('komando-handoff-test-' + [Guid]::NewGuid())
[IO.Directory]::CreateDirectory($komandoTestDir) | Out-Null
$env:KOMANDO_UPDATE_PARENT = '1234'
$env:KOMANDO_UPDATE_READY = Join-Path $komandoTestDir 'helper.ready'
$env:KOMANDO_UPDATE_ERROR = Join-Path $komandoTestDir 'update-error.txt'
# Shell syntax in a path must remain data, including a space and an apostrophe.
$env:KOMANDO_UPDATE_MSI = Join-Path $komandoTestDir 'O''Brien $(); & update.msi'
$env:KOMANDO_UPDATE_EXE = Join-Path $komandoTestDir 'Komando app.exe'

function Assert-Update($condition, $message) {
    if (-not $condition) { throw $message }
}
function Get-Process {
    [CmdletBinding()]
    param([int] $Id)
    $process = [PSCustomObject]@{}
    $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {
        param($timeout)
        Assert-Update ([IO.File]::ReadAllText($env:KOMANDO_UPDATE_READY) -eq 'ready') 'Missing helper readiness signal'
        $script:komandoEvents.Add('wait')
        return (-not $script:komandoTimeout)
    }
    return $process
}
function Start-Process {
    [CmdletBinding()]
    param([string] $FilePath, [string[]] $ArgumentList, [switch] $Wait, [switch] $PassThru, [string] $Verb)
    if ($FilePath.EndsWith('msiexec.exe')) {
        Assert-Update ($script:komandoEvents.Count -eq 1 -and $script:komandoEvents[0] -eq 'wait') 'Installation started before Komando exited'
        Assert-Update ($Wait -and $PassThru) 'The helper must wait for the installer result'
        Assert-Update ($ArgumentList -contains ('"{0}"' -f $env:KOMANDO_UPDATE_MSI)) 'Installer path was not quoted intact'
        Assert-Update ($ArgumentList -contains '/passive' -and $ArgumentList -contains '/norestart') 'Wrong installer display or restart options'
        Assert-Update ($ArgumentList -contains 'KOMANDO_UPDATING=1') 'Missing update scope override'
        if ($env:KOMANDO_UPDATE_ALL_USERS -eq '1') {
            Assert-Update ($Verb -eq 'RunAs' -and $ArgumentList -contains 'ALLUSERS=1') 'All-users update must elevate only the installer'
        } else {
            Assert-Update (-not $Verb -and $ArgumentList -contains 'ALLUSERS=2' -and $ArgumentList -contains 'MSIINSTALLPERUSER=1') 'Per-user scope changed'
        }
        $script:komandoEvents.Add('install')
        if ($script:komandoCancel) { throw 'The operation was canceled by the user.' }
        return [PSCustomObject]@{ ExitCode = $script:komandoExitCode }
    }
    Assert-Update ($FilePath -eq $env:KOMANDO_UPDATE_EXE -and -not $Verb) 'Komando must reopen unelevated from its existing location'
    $script:komandoEvents.Add('relaunch')
}

try {
    foreach ($scenario in @(
        @{ Scope = '0'; Code = 0; Failed = $false },
        @{ Scope = '1'; Code = 0; Failed = $false },
        @{ Scope = '1'; Code = 3010; Failed = $false },
        @{ Scope = '1'; Code = 1603; Failed = $true },
        @{ Scope = '1'; Code = 0; Cancel = $true; Failed = $true },
        @{ Scope = '0'; Code = 0; Timeout = $true; Failed = $true }
    )) {
        $script:komandoEvents = [Collections.Generic.List[string]]::new()
        $script:komandoExitCode = $scenario.Code
        $script:komandoCancel = $scenario.Cancel
        $script:komandoTimeout = $scenario.Timeout
        $env:KOMANDO_UPDATE_ALL_USERS = $scenario.Scope
        # A successful update must clear an earlier failure.
        [IO.File]::WriteAllText($env:KOMANDO_UPDATE_ERROR, 'previous error')
        . (Join-Path $PSScriptRoot '..\src\update-install.ps1')
        $expected = if ($scenario.Timeout) { 'wait' } else { 'wait,install,relaunch' }
        Assert-Update (($script:komandoEvents -join ',') -eq $expected) "Wrong process order: $script:komandoEvents"
        Assert-Update ((Test-Path -LiteralPath $env:KOMANDO_UPDATE_ERROR) -eq $scenario.Failed) 'Wrong persisted error state'
        if ($scenario.Failed) {
            Assert-Update ([IO.File]::ReadAllText($env:KOMANDO_UPDATE_ERROR).StartsWith('The update did not finish:')) 'Missing error explanation'
        }
        Assert-Update (-not (Test-Path -LiteralPath $env:KOMANDO_UPDATE_READY)) 'Readiness signal was not cleaned up'
    }
    Write-Output 'Update helper: all 6 handoff scenarios passed.'
} finally {
    Remove-Item -LiteralPath $komandoTestDir -Recurse -Force
}
