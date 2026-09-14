param(
    [string]$OutputDirectory = "target/release-package",
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $root
if (-not $SkipBuild) {
    cargo build --release --locked -p lvos-agent -p lvos
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build failed.' }
}

$isccCommand = Get-Command iscc.exe -ErrorAction SilentlyContinue
$isccPath = if ($null -eq $isccCommand) { $null } else { $isccCommand.Source }
if ($null -eq $isccPath) {
    foreach ($candidate in @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $isccPath = $candidate
            break
        }
    }
}
if ($null -eq $isccPath) {
    throw 'Inno Setup 6 compiler (ISCC.exe) is required.'
}

$version = (python scripts/workspace_version.py).Trim()
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$output = (Resolve-Path -LiteralPath $OutputDirectory).Path
$script = (Resolve-Path -LiteralPath 'packaging/windows/LVOS.iss').Path
& $isccPath "/DSourceRoot=$root" "/DMyAppVersion=$version" "/O$output" $script
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed.' }
$installer = Join-Path $output "LVOS-$version-windows-x86_64-setup.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "Expected installer was not created: $installer"
}
Write-Output $installer
