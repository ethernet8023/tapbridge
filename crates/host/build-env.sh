# Build environment helper for crates/host (Windows ARM64).
# Source before cargo:  source crates/host/build-env.sh
# ring/blake3 build scripts require clang even on the msvc toolchain.
export PATH="/c/Program Files/LLVM/bin:$PATH"
export CC_aarch64_pc_windows_msvc="C:\Program Files\LLVM\bin\clang.exe"
export AR_aarch64_pc_windows_msvc="C:\Program Files\LLVM\bin\llvm-ar.exe"
MSVC="C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207"
SDK="C:\Program Files (x86)\Windows Kits\10"
export INCLUDE="$MSVC\\include;$SDK\\Include\\10.0.26100.0\\ucrt;$SDK\\Include\\10.0.26100.0\\shared;$SDK\\Include\\10.0.26100.0\\um;$SDK\\Include\\10.0.26100.0\\winrt"
export LIB="$MSVC\\lib\\arm64;$SDK\\Lib\\10.0.26100.0\\ucrt\\arm64;$SDK\\Lib\\10.0.26100.0\\um\\arm64"
