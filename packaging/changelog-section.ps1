<#
.SYNOPSIS
  Print the patch notes for one version from CHANGELOG.md (the text between
  "## vX.Y.Z" and the next "## " heading). Used by the release workflow and
  by packaging/sync-release-notes.ps1.
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Path = (Join-Path $PSScriptRoot '..' 'CHANGELOG.md')
)
$Version = $Version.TrimStart('v')
$lines = Get-Content -Path $Path -Encoding utf8
$out = New-Object System.Collections.Generic.List[string]
$inside = $false
foreach ($line in $lines) {
    if ($line -match '^## ') {
        if ($inside) { break }
        if ($line -match "^## v$([regex]::Escape($Version))(\s|$)") { $inside = $true }
        continue
    }
    if ($inside) { $out.Add($line) }
}
if ($out.Count -eq 0) { return $null }
($out -join "`n").Trim()
