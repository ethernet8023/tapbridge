# tapbridge for Linux: the desktop app, the pcscd reader driver
# (libtapbridge_ifd.so) and the udev rules. Used by the NixOS and Home
# Manager modules (nix/nixos-module.nix, nix/home-module.nix).
#
# A dream2nix module (rust-crane): the dependencies build once in their own
# derivation, so a change to our code recompiles only our crates.
{ src, version, tests ? false }:
{ config, lib, dream2nix, ... }:

let
  inherit (config.deps) stdenv;
  # Opened with dlopen at run time (gpui, tray-icon): not in DT_NEEDED, so
  # they go on the binary's RPATH.
  runtimeLibs = with config.deps; [
    wayland
    libxkbcommon
    vulkan-loader
    libayatana-appindicator
    libx11
    libxcb
    libxcursor
    libxi
    libxrandr
    gtk3
  ];
  # Both the dependencies derivation and ours compile against these.
  toolchain = {
    nativeBuildInputs = with config.deps; [ pkg-config rustPlatform.bindgenHook ];
    buildInputs = with config.deps; [ gtk3 libayatana-appindicator dbus udev openssl fontconfig freetype ] ++ runtimeLibs;
  };
in
{
  imports = [
    dream2nix.modules.dream2nix.rust-cargo-lock
    dream2nix.modules.dream2nix.rust-cargo-vendor
    dream2nix.modules.dream2nix.rust-crane
  ];

  # The cargo package: crane builds `--package <name>`.
  name = "tapbridge-desktop";
  inherit version;

  deps = { nixpkgs, ... }: lib.mapAttrs (_: lib.mkDefault) {
    inherit (nixpkgs)
      stdenv pkg-config rustPlatform patchelf jq
      gtk3 libayatana-appindicator dbus udev openssl fontconfig freetype
      wayland libxkbcommon vulkan-loader libxcb libx11 libxcursor libxi libxrandr;
  };

  # The workspace tests need a network and pcscd: CI and the NixOS VM test run them.
  rust-crane.runTests = false;
  rust-crane.depsDrv.mkDerivation = toolchain;

  mkDerivation = toolchain // {
    inherit src;
    nativeBuildInputs = toolchain.nativeBuildInputs ++ [ config.deps.patchelf ];

    postBuild = ''
      bash crates/desktop/ifd_shim/build.sh
    '';

    postInstall = ''
      install -Dm755 crates/desktop/ifd_shim/dist/libtapbridge_ifd.so $out/lib/libtapbridge_ifd.so
      install -Dm644 packaging/linux/70-tapbridge-usb.rules packaging/linux/71-tapbridge-key.rules -t $out/lib/udev/rules.d
      install -Dm644 packaging/linux/tapbridge.desktop $out/share/applications/tapbridge.desktop
      for size in 16 24 32 48 64 128 256; do
        install -Dm644 crates/desktop/assets/icons/app-$size.png \
          $out/share/icons/hicolor/''${size}x''${size}/apps/tapbridge.png
      done
    '';

    # After the fixup phase's RPATH shrink, which drops every entry the binary
    # does not link against at build time (all of these are dlopen'd).
    postFixup = ''
      patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/tapbridge
    '';

    meta = {
      description = "Use your phone as the NFC reader and FIDO security key for your Linux PC";
      homepage = "https://github.com/ethernet8023/tapbridge";
      license = lib.licenses.mit;
      platforms = lib.platforms.linux;
      mainProgram = "tapbridge";
    };
  }
  # The NixOS VM test's variant: the same dependencies derivation (so the
  # dependency tree is not rebuilt), but the output is the test executable.
  // lib.optionalAttrs tests {
    nativeBuildInputs = toolchain.nativeBuildInputs ++ [ config.deps.jq ];
    postBuild = ''
      cargo test -p tapbridge-desktop --release --offline \
        --no-run --message-format=json-render-diagnostics > messages.json
    '';
    installPhase = ''
      exe=$(jq -r 'select(.profile.test == true and .executable != null) | .executable' messages.json | head -1)
      install -Dm755 "$exe" $out/bin/tapbridge-tests
    '';
    postInstall = "";
  };
}
