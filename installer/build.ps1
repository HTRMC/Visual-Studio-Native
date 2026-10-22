# Builds target\installer\VSNUserSetup-x64-<version>.exe: a release build,
# then Inno Setup 6's compiler (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
# The extension host's runtime, found next to vsn.exe before PATH.
# ponytail: ships this machine's node (or $env:VSN_NODE), download a pinned
# release from nodejs.org instead if builds need to be reproducible.
$node = $env:VSN_NODE
if (-not $node) { $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source }
if ($node) {
    New-Item -ItemType Directory -Force target\release\runtime | Out-Null
    Copy-Item $node target\release\runtime\node.exe
} else {
    Write-Warning "node.exe not found: the installer won't bundle a JS runtime"
}
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw "Inno Setup 6 not found: winget install JRSoftware.InnoSetup" }
& $iscc /Q "/DAppVersion=$version" installer\vsn.iss
exit $LASTEXITCODE
