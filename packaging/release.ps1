<#
.SYNOPSIS
  Cut a release: verify CHANGELOG.md and Cargo.toml agree, the tree is clean
  and on main, then tag vX.Y.Z and push — the Release workflow does the rest.

.EXAMPLE
  ./packaging/release.ps1            # version from Cargo.toml
  ./packaging/release.ps1 -DryRun    # only run the checks
  ./packaging/release.ps1 -Version 0.2.0 -Force   # skip the branch check
#>
[CmdletBinding()]
param(
    # Version to release; defaults to `version = "…"` in Cargo.toml.
    [string]$Version,
    # Run every check but do not tag or push.
    [switch]$DryRun,
    # Allow releasing from a branch other than main.
    [switch]$Force,
    [string]$Remote = 'origin'
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Push-Location $root
try {
    $cargo = Get-Content -Raw -Encoding utf8 Cargo.toml
    if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Cargo.toml has no version = "…" line' }
    $cargoVersion = $Matches[1]
    if (-not $Version) { $Version = $cargoVersion }
    $Version = $Version.TrimStart('v')
    if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { throw "'$Version' is not a semver version" }
    if ($Version -ne $cargoVersion) { throw "Cargo.toml says $cargoVersion but you asked for $Version — bump Cargo.toml first" }
    $tag = "v$Version"
    $problems = New-Object System.Collections.Generic.List[string]

    # Patch notes present?
    $notes = & (Join-Path $PSScriptRoot 'changelog-section.ps1') -Version $Version
    if (-not $notes) { $problems.Add("CHANGELOG.md has no '## $tag' section with content") }
    else { Write-Host "Patch notes for ${tag}:`n$notes`n" }

    # Cargo.lock in sync (fails if it would change).
    cargo metadata --locked --format-version 1 > $null 2>&1
    if ($LASTEXITCODE -ne 0) { $problems.Add('Cargo.lock is out of date — run cargo build and commit it') }

    # Git state.
    if (git status --porcelain) { $problems.Add('working tree is not clean — commit or stash first') }
    $branch = git rev-parse --abbrev-ref HEAD
    if ($branch -ne 'main' -and -not $Force) { $problems.Add("on branch '$branch', not main (use -Force to override)") }
    if (git tag -l $tag) { $problems.Add("tag $tag already exists locally") }
    if (git ls-remote --tags $Remote "refs/tags/$tag") { $problems.Add("tag $tag already exists on $Remote") }
    git fetch -q $Remote
    $behind = git rev-list --count "HEAD..$Remote/$branch" 2>$null
    if ($behind -and [int]$behind -gt 0) { $problems.Add("HEAD is $behind commit(s) behind $Remote/$branch — pull first") }

    if ($problems.Count) {
        $problems | ForEach-Object { Write-Host "  ✗ $_" -ForegroundColor Red }
        throw "not releasing $tag"
    }
    Write-Host "  ✓ Cargo.toml $cargoVersion, CHANGELOG $tag, clean tree on $branch" -ForegroundColor Green
    if ($DryRun) { Write-Host "dry run — would tag $tag and push to $Remote"; return }

    git tag -a $tag -m "$tag"
    git push $Remote $branch $tag
    Write-Host "pushed $tag — the Release workflow builds and publishes it." -ForegroundColor Green
}
finally { Pop-Location }
