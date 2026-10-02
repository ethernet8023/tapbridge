param(
    [ValidateSet('arm64', 'x64', 'both')][string]$Arch = 'arm64'
)
# Builds tapbridge.exe per architecture and compiles the Inno Setup installers
# into dist\tapbridge-<ver>-<arch>-setup.exe. Requires: LLVM clang (ring), the
# MSVC x64 cross-link libs for -Arch x64/both, Inno Setup 6 (iscc on PATH).
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$llvm = Join-Path $env:ProgramFiles 'LLVM/bin/clang.exe'
    if (!(Test-Path $llvm)) { throw 'Install LLVM clang; ring requires clang on Windows.' }
    $version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
        Where-Object { $_.name -eq 'tapbridge-desktop' } | Select-Object -ExpandProperty version
    $targets = if ($Arch -eq 'both') { @('arm64', 'x64') } else { @($Arch) }
    foreach ($target in $targets) {
        $env:PATH = "C:\Program Files\LLVM\bin;$env:PATH"
        $env:CC = $llvm
        $msvc = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207'
        $sdk = 'C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0'
        if ($target -eq 'arm64') {
            $env:CARGO_BUILD_TARGET = 'aarch64-pc-windows-msvc'
            $env:LIB = "$msvc\lib\arm64;$sdk\ucrt\arm64;$sdk\um\arm64"
        } else {
            $env:CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc'
            $env:LIB = "$msvc\lib\x64;$sdk\ucrt\x64;$sdk\um\x64"
        }
        Write-Host "== cargo build --release --target $env:CARGO_BUILD_TARGET =="
        cargo build --release -p tapbridge-desktop --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $target" }
        $outDir = Join-Path $root "dist\tapbridge-$version-$target"
        New-Item -ItemType Directory -Force -Path $outDir | Out-Null
        $exe = Join-Path $root "target\$env:CARGO_BUILD_TARGET\release\tapbridge.exe"
        Copy-Item $exe $outDir -Force

        # Downloads once, verifies the pinned SHA-256 every time, and returns
        # the exact installer file name.
        $usbipFile = & $PSScriptRoot\fetch-usbip.ps1 -Arch $target
        $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
        if (!$iscc) {
            $iscc = "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe"
        }
        if (!(Test-Path $iscc)) { throw 'Inno Setup 6 (iscc.exe) not found; run: winget install --id JRSoftware.InnoSetup -e --scope user' }
        & $iscc "/DAppArch=$target" "/DAppVersion=$version" "/DUsbipFile=$usbipFile" (Join-Path $root 'installer\tapbridge.iss')
        if ($LASTEXITCODE -ne 0) { throw "iscc failed for $target" }
        Write-Host "== dist\tapbridge-$version-$target-setup.exe ready =="
    }
} finally {
    Remove-Item Env:CARGO_BUILD_TARGET -ErrorAction SilentlyContinue
    Pop-Location
}
