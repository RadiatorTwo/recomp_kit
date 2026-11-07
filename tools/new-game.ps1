# Makes a new game project from template/:
#   tools\new-game.ps1 -Name rust_metal -Title "Gun Metal" -Target C:\!Code2\rust_metal [-Local]
# -Name: the crate and program name (lower case, letters, digits, _);
# -Title: the game's name as shown; -Target: the new folder (must not exist);
# -Local: depend on this checkout by path instead of the git repository.
param(
    [Parameter(Mandatory)] [string] $Name,
    [Parameter(Mandatory)] [string] $Title,
    [Parameter(Mandatory)] [string] $Target,
    [switch] $Local
)
$ErrorActionPreference = 'Stop'
if ($Name -notmatch '^[a-z][a-z0-9_]*$') { throw "Name: lower case letters, digits and _ only" }
if (Test-Path $Target) { throw "$Target exists already" }
$kit = Split-Path -Parent $PSScriptRoot
Copy-Item -Recurse (Join-Path $kit 'template') $Target
Copy-Item (Join-Path $kit 'LICENSE') $Target
$prefix = $Name.ToUpper() -replace '_', ''
$utf8 = New-Object System.Text.UTF8Encoding $false
Get-ChildItem -Recurse -File $Target | ForEach-Object {
    $t = [IO.File]::ReadAllText($_.FullName)
    $t = $t.Replace('GAMETITLE', $Title).Replace('MYGAME', $prefix).Replace('mygame', $Name)
    if ($Local) {
        $t = $t.Replace('recomp = { git = "https://github.com/RadiatorTwo/recomp_kit", branch = "main" }', "recomp = { path = `"$($kit -replace '\\', '/')/crates/recomp`" }")
        $t = $t.Replace('win32 = { git = "https://github.com/RadiatorTwo/recomp_kit", branch = "main" }', "win32 = { path = `"$($kit -replace '\\', '/')/crates/win32`" }")
    }
    [IO.File]::WriteAllText($_.FullName, $t, $utf8)
}
Push-Location $Target
git init -q
Pop-Location
Write-Host "made $Target; next: docs/GUIDE.md of recomp_kit, step 1"
