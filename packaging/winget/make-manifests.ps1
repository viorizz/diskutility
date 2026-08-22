# Generate winget manifests for a published release.
#
#   ./packaging/winget/make-manifests.ps1 -Version 0.4.6
#   ./packaging/winget/make-manifests.ps1 -Version 0.4.6 -Sha256 <hash>   # skip the download
#
# Writes manifests/viorizz.diskutility.*.yaml with the version, download URL
# and SHA256 (fetched from the release's checksums.txt unless given). The
# result is what `wingetcreate`/`winget validate` expect for a portable package.
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Version,
    [string] $Sha256,
    [string] $OutDir = (Join-Path $PSScriptRoot 'manifests')
)
$ErrorActionPreference = 'Stop'
$Version = $Version.TrimStart('v')
$id = 'viorizz.diskutility'
$url = "https://github.com/viorizz/diskutility/releases/download/v$Version/diskutility.exe"

if (-not $Sha256) {
    $sumsUrl = "https://github.com/viorizz/diskutility/releases/download/v$Version/checksums.txt"
    Write-Host "Fetching $sumsUrl"
    $text = (Invoke-WebRequest $sumsUrl -UseBasicParsing).Content
    if ($text -is [byte[]]) { $text = [Text.Encoding]::ASCII.GetString($text) }  # pwsh 7 returns bytes for text/plain
    $line = ($text -split "`n" | Where-Object { $_ -match 'diskutility\.exe' } | Select-Object -First 1)
    if (-not $line) { throw "checksums.txt has no diskutility.exe entry" }
    $Sha256 = ($line -split '\s+')[0]
}
$Sha256 = $Sha256.ToUpper()
if ($Sha256.Length -ne 64) { throw "SHA256 must be 64 hex chars, got '$Sha256'" }

New-Item -ItemType Directory -Force $OutDir | Out-Null

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $OutDir "$id.yaml")

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
InstallerType: portable
Commands:
  - diskutility
ReleaseDate: $(Get-Date -Format 'yyyy-MM-dd')
Installers:
  - Architecture: x64
    InstallerUrl: $url
    InstallerSha256: $Sha256
ManifestType: installer
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $OutDir "$id.installer.yaml")

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
PackageLocale: en-US
Publisher: viorizz
PublisherUrl: https://github.com/viorizz
PublisherSupportUrl: https://github.com/viorizz/diskutility/issues
PackageName: diskutility
PackageUrl: https://github.com/viorizz/diskutility
License: MIT
LicenseUrl: https://github.com/viorizz/diskutility/blob/main/LICENSE
ShortDescription: A fast terminal disk utility for Windows - format, erase, benchmark, back up, clone and write bootable images with a beautiful TUI.
Moniker: diskutility
Tags:
  - backup
  - clone
  - disk
  - format
  - iso
  - rust
  - tui
ReleaseNotesUrl: https://github.com/viorizz/diskutility/releases/tag/v$Version
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@ | Set-Content -Encoding utf8 (Join-Path $OutDir "$id.locale.en-US.yaml")

Write-Host "Wrote manifests for $id $Version (sha256 $Sha256) to $OutDir"
Write-Host "Validate with:  winget validate --manifest $OutDir"
