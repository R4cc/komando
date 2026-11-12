# Builds the Komando MSI from packaging/komando.wxs with WiX v5. Run from the repository root,
# with the WiX tool installed (dotnet tool install --global wix --version 5.0.2):
#   ./packaging/build-msi.ps1 -Version 1.2.3 -BinDir target\release -Out Komando.msi
# Used by the CI workflow (to catch installer mistakes on every push) and the Release workflow.
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $BinDir,
    [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'
$wixVersion = '5.0.2'

# The setup pages come from the UI extension, opening Komando at the end from the Util one.
foreach ($ext in 'WixToolset.UI.wixext', 'WixToolset.Util.wixext') {
    wix extension add -g "$ext/$wixVersion"
    if ($LASTEXITCODE -ne 0) { throw "Couldn't add the WiX extension $ext" }
}

wix build packaging/komando.wxs -arch x64 `
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
    -culture en-US -loc packaging/komando.en-us.wxl `
    -d "Version=$Version" -d "BinDir=$BinDir" -o $Out
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
