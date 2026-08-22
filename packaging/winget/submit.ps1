# Submit (or update) the diskutility package on winget via wingetcreate.
#
#   ./packaging/winget/submit.ps1 -Version 0.4.6 -Token <github PAT with public_repo scope>
#   ./packaging/winget/submit.ps1 -Version 0.4.6 -Token $env:WINGET_TOKEN -First   # very first submission
#
# `wingetcreate update` needs the package to already exist in microsoft/winget-pkgs;
# pass -First for the initial submission, which generates manifests from the
# release asset and opens the first PR (Microsoft reviews new packages manually).
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Token,
    [switch] $First,
    [switch] $DryRun
)
$ErrorActionPreference = 'Stop'
$Version = $Version.TrimStart('v')
$id = 'viorizz.diskutility'
$url = "https://github.com/viorizz/diskutility/releases/download/v$Version/diskutility.exe"

if (-not (Get-Command wingetcreate -ErrorAction SilentlyContinue)) {
    Write-Host "Installing wingetcreate…"
    winget install --id Microsoft.WingetCreate --exact --accept-source-agreements --accept-package-agreements --silent | Out-Null
}

if ($First) {
    # Generate manifests locally (deterministic, no prompts), validate, then submit them.
    & (Join-Path $PSScriptRoot 'make-manifests.ps1') -Version $Version
    $dir = Join-Path $PSScriptRoot 'manifests'
    if ($DryRun) { winget validate --manifest $dir; return }
    wingetcreate submit --token $Token --prtitle "New package: $id version $Version" $dir
} else {
    $args = @('update', $id, '--version', $Version, '--urls', $url, '--token', $Token)
    if (-not $DryRun) { $args += '--submit' }
    wingetcreate @args
}
