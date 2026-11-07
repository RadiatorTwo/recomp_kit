# Makes a new game project from template/:
#   tools\new-game.ps1 -Name rust_metal -Title "Gun Metal" -Target C:\!Code2\rust_metal
# -Name: the crate and program name (lower case, letters, digits, _);
# -Title: the game's name as shown; -Target: the new folder (must not exist).
# The framework (crates/recomp, crates/win32, crates/tools) is copied into the new project:
# the game owns its copy and changes it freely; recomp_kit's commit is noted in its PLAN.md.
param(
    [Parameter(Mandatory)] [string] $Name,
    [Parameter(Mandatory)] [string] $Title,
    [Parameter(Mandatory)] [string] $Target
)
$ErrorActionPreference = 'Stop'
if ($Name -notmatch '^[a-z][a-z0-9_]*$') { throw "Name: lower case letters, digits and _ only" }
if (Test-Path $Target) { throw "$Target exists already" }
$kit = Split-Path -Parent $PSScriptRoot
Copy-Item -Recurse (Join-Path $kit 'template') $Target
Copy-Item (Join-Path $kit 'LICENSE') $Target
# the dependency versions recomp_kit was tested with
Copy-Item (Join-Path $kit 'Cargo.lock') $Target
foreach ($crate in 'recomp', 'win32', 'tools') {
    $to = Join-Path $Target "crates\$crate"
    Copy-Item -Recurse (Join-Path $kit "crates\$crate") $to
    if (Test-Path "$to\target") { Remove-Item -Recurse -Force "$to\target" }
}
# recomp_kit's shared versions of the dependencies, for the copied crates
$ws = [IO.File]::ReadAllText((Join-Path $kit 'Cargo.toml')) -replace "`r`n", "`n"
$from = $ws.IndexOf('[workspace.package]')
$to = $ws.IndexOf('[profile')
if ($from -lt 0 -or $to -lt $from) { throw "recomp_kit's Cargo.toml: [workspace.package] not found" }
$block = $ws.Substring($from, $to - $from).TrimEnd() + "`n"
$commit = (git -C $kit log -1 --format='%h %cs') 2>$null
if (-not $commit) { $commit = 'unknown' }
$prefix = $Name.ToUpper() -replace '_', ''
$utf8 = New-Object System.Text.UTF8Encoding $false
Get-ChildItem -Recurse -File $Target | Where-Object { $_.FullName -notlike "$Target\crates\*" } | ForEach-Object {
    $t = [IO.File]::ReadAllText($_.FullName)
    $t = $t.Replace('GAMETITLE', $Title).Replace('MYGAME', $prefix).Replace('mygame', $Name)
    $t = $t.Replace('KITCOMMIT', $commit)
    $t = $t -replace "# KIT_WORKSPACE[^\n]*\n#[^\n]*\n", $block
    [IO.File]::WriteAllText($_.FullName, $t, $utf8)
}
Push-Location $Target
git init -q
Pop-Location
Write-Host "made $Target (framework of recomp_kit $commit); next: docs/GUIDE.md of recomp_kit, step 1"
