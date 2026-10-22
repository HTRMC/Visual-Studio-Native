# Builds target\installer\VSNUserSetup-x64-<version>.exe: a release build,
# then Inno Setup 6's compiler (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw "Inno Setup 6 not found: winget install JRSoftware.InnoSetup" }
& $iscc /Q "/DAppVersion=$version" installer\vsn.iss
exit $LASTEXITCODE
