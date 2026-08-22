# diskutility installer — usage:
#   irm https://raw.githubusercontent.com/viorizz/diskutility/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$repo = 'viorizz/diskutility'
$dir = Join-Path $env:LOCALAPPDATA 'diskutility'

Write-Host "Fetching latest diskutility release..." -ForegroundColor Cyan
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'diskutility-installer' }
$asset = $release.assets | Where-Object name -eq 'diskutility.exe' | Select-Object -First 1
if (-not $asset) { throw "no diskutility.exe asset found in the latest release" }

$sums = $release.assets | Where-Object name -eq 'checksums.txt' | Select-Object -First 1
if (-not $sums) { throw "no checksums.txt asset found in the latest release - refusing to install an unverified binary" }
$expected = "https://github.com/$repo/releases/download/"
foreach ($u in @($asset.browser_download_url, $sums.browser_download_url)) {
    if (-not $u.StartsWith($expected)) { throw "unexpected download location: $u" }
}

New-Item -ItemType Directory -Force $dir | Out-Null
$exe = Join-Path $dir 'diskutility.exe'
$staged = "$exe.new"
Invoke-WebRequest $asset.browser_download_url -OutFile $staged -UseBasicParsing
$sumText = (Invoke-WebRequest $sums.browser_download_url -UseBasicParsing).Content
# GitHub serves assets as application/octet-stream, so .Content is a byte[] not a string.
if ($sumText -is [byte[]]) { $sumText = [Text.Encoding]::UTF8.GetString($sumText) }
$want = ($sumText -split "`n" | Where-Object { $_ -match 'diskutility\.exe' } | Select-Object -First 1) -split '\s+' | Select-Object -First 1
$have = (Get-FileHash -Algorithm SHA256 -LiteralPath $staged).Hash
if (-not $want -or $want.ToLower() -ne $have.ToLower()) {
    Remove-Item -Force $staged -ErrorAction SilentlyContinue
    throw "SHA256 mismatch: expected '$want', got '$have' - download corrupted or tampered, aborting"
}
Write-Host "SHA256 verified: $have" -ForegroundColor DarkGray
Move-Item -Force $staged $exe

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "Added $dir to your user PATH (open a new terminal to pick it up)." -ForegroundColor Yellow
}

Write-Host "Installed diskutility $($release.tag_name) to $exe" -ForegroundColor Green
Write-Host "Run 'diskutility' from an elevated terminal for write operations."
Write-Host "Update later with: diskutility --update"
