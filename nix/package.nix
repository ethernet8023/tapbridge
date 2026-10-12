# tapbridge for nix-darwin: the app (built from this repo) and the
# `programs.tapbridge` module that installs it system-wide, the same way the
# macOS installer pkg does, plus every Mac smart card setting.
{ lib, rustPlatform, stdenv, apple-sdk, darwin, src, version }:

rustPlatform.buildRustPackage {
  pname = "tapbridge";
  inherit version src;

  cargoLock = {
    lockFile = "${src}/Cargo.lock";
    allowBuiltinFetchGit = true;
  };

  cargoBuildFlags = [ "-p" "tapbridge-desktop" ];
  # The workspace tests need a keychain, a network and PC/SC: CI runs them.
  doCheck = false;

  nativeBuildInputs = [ rustPlatform.bindgenHook darwin.sigtool ];
  buildInputs = [ apple-sdk ];

  postBuild = ''
    crates/desktop/ifd_shim/build.sh
  '';

  # Same layout as the pkg (scripts/build-pkg.sh), under $out.
  installPhase = ''
    runHook preInstall
    app=$out/Applications/tapbridge.app
    mkdir -p $app/Contents/MacOS $app/Contents/Resources
    cp target/${stdenv.hostPlatform.rust.rustcTarget}/release/tapbridge $app/Contents/MacOS/tapbridge
    cat > $app/Contents/Info.plist <<PLIST
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
        <key>CFBundleName</key><string>tapbridge</string>
        <key>CFBundleDisplayName</key><string>tapbridge</string>
        <key>CFBundleIdentifier</key><string>computer.ethernet.tapbridge</string>
        <key>CFBundleVersion</key><string>${version}</string>
        <key>CFBundleShortVersionString</key><string>${version}</string>
        <key>CFBundlePackageType</key><string>APPL</string>
        <key>CFBundleExecutable</key><string>tapbridge</string>
        <key>LSMinimumSystemVersion</key><string>13.0</string>
        <key>LSUIElement</key><true/>
        <key>NSHighResolutionCapable</key><true/>
    </dict>
    </plist>
    PLIST
    # Ad hoc signature on the binary: the keychain ties saved pairings to
    # it. Nix's codesign (sigtool) only signs Mach-O files, not bundles.
    # (The app icon needs Xcode's actool, which a Nix build doesn't have.)
    codesign --force --sign - $app/Contents/MacOS/tapbridge

    mkdir -p $out/libexec/SmartCardServices/drivers
    cp -R crates/desktop/ifd_shim/dist/tapbridge.bundle $out/libexec/SmartCardServices/drivers/
    runHook postInstall
  '';

  meta = {
    description = "Use your phone as the NFC card reader for your Mac";
    homepage = "https://github.com/ethernet8023/tapbridge";
    license = lib.licenses.mit;
    platforms = lib.platforms.darwin;
    mainProgram = "tapbridge";
  };
}
