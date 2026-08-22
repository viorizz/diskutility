<#
.SYNOPSIS
  Push the patch notes from CHANGELOG.md onto existing GitHub releases.
  Requires `gh auth login`. By default updates every vX.Y.Z section found in
  CHANGELOG.md that has a matching release; pass -Version to do just one.
.EXAMPLE
  ./packaging/sync-release-notes.ps1            # all releases
  ./packaging/sync-release-notes.ps1 -Version 0.4.2
#>
param(
    [string]$Version,
    [string]$Repo = 'viorizz/diskutility'
)
$changelog = Join-Path $PSScriptRoot '..' 'CHANGELOG.md'
# Prefer the real GitHub CLI: an npm "gh" package (node-gh) can shadow it on PATH.
$gh = if (Test-Path "$env:ProgramFiles\GitHub CLI\gh.exe") { "$env:ProgramFiles\GitHub CLI\gh.exe" } else { 'gh' }
$versions = if ($Version) { @($Version.TrimStart('v')) } else {
    (Get-Content $changelog) | Where-Object { $_ -match '^## v(\d+\.\d+\.\d+)' } | ForEach-Object { $Matches[1] }
}
foreach ($v in $versions) {
    $notes = & (Join-Path $PSScriptRoot 'changelog-section.ps1') -Version $v
    if (-not $notes) { Write-Warning "no CHANGELOG section for v$v"; continue }
    $tag = "v$v"
    & $gh release view $tag --repo $Repo --json tagName 1>$null 2>$null
    if ($LASTEXITCODE -ne 0) { Write-Warning "no GitHub release for $tag, skipping"; continue }
    # Keep GitHub's auto-generated "Full Changelog" comparison link under our notes.
    $prev = git tag --sort=v:refname | Where-Object { [version]($_.TrimStart('v')) -lt [version]$v } | Select-Object -Last 1
    $body = $notes
    if ($prev) { $body += "`n`n**Full Changelog**: https://github.com/$Repo/compare/$prev...$tag" }
    $tmp = New-TemporaryFile
    $body | Out-File -FilePath $tmp -Encoding utf8
    & $gh release edit $tag --repo $Repo --notes-file $tmp
    Remove-Item $tmp
    Write-Host "updated $tag"
}
