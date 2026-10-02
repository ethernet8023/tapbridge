param([switch]$Release, [switch]$Test)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $llvm = Join-Path $env:ProgramFiles 'LLVM/bin/clang.exe'
    if (!(Test-Path $llvm)) { throw 'Install LLVM clang; ring requires clang on Windows ARM64.' }
    $hostTriple = (& rustc -vV | Select-String '^host: ').ToString().Substring(6)
    if ($LASTEXITCODE -ne 0) { throw 'rustc failed' }
    if ($hostTriple -eq 'aarch64-pc-windows-msvc') {
        $env:CC_aarch64_pc_windows_msvc = $llvm
    }
    if ($Test) {
        & cargo test --workspace --locked
        if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
        & cargo clippy --workspace --all-targets --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
        & cargo fmt --all -- --check
        if ($LASTEXITCODE -ne 0) { throw 'Rust formatting failed' }
    } else {
        $cargoArgs = @('build', '-p', 'tapbridge-host', '--locked')
        if ($Release) { $cargoArgs += '--release' }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw 'Host build failed' }
    }
} finally { Pop-Location }
