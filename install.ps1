# diskutility installer — usage:
#   irm https://raw.githubusercontent.com/viorizz/diskutility/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$repo = 'viorizz/diskutility'
$dir = Join-Path $env:LOCALAPPDATA 'diskutility'

Write-Host "Fetching latest diskutility release..." -ForegroundColor Cyan
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'diskutility-installer' }
$asset = $release.assets | Where-Object name -eq 'diskutility.exe' | Select-Object -First 1
if (-not $asset) { throw "no diskutility.exe asset found in the latest release" }

New-Item -ItemType Directory -Force $dir | Out-Null
$exe = Join-Path $dir 'diskutility.exe'
Invoke-WebRequest $asset.browser_download_url -OutFile $exe -UseBasicParsing

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "Added $dir to your user PATH (open a new terminal to pick it up)." -ForegroundColor Yellow
}

Write-Host "Installed diskutility $($release.tag_name) to $exe" -ForegroundColor Green
Write-Host "Run 'diskutility' from an elevated terminal for write operations."
Write-Host "Update later with: diskutility --update"
