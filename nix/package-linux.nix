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
    libx11
    libxcb
    libxcursor
    libxi
    libxrandr
  ];
  # crane builds the dependencies from a dummy copy of the workspace (every
  # crate of ours an empty stub), but gpui-tray, a registry crate, needs the
  # real patched gpui-ce. Its content-addressed copy only changes when the
  # vendored gpui does, so the dependency derivation stays cached.
  gpuiVendor = builtins.path {
    path = src + "/third_party/gpui-glass/vendor/gpui-ce";
    name = "gpui-ce-vendor";
  };
  # Both the dependencies derivation and ours compile against these.
  toolchain = {
    nativeBuildInputs = with config.deps; [ pkg-config rustPlatform.bindgenHook ];
    buildInputs = with config.deps; [ dbus udev openssl fontconfig freetype ] ++ runtimeLibs;
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
      stdenv pkg-config rustPlatform patchelf jq perl
      dbus udev openssl fontconfig freetype
      wayland libxkbcommon vulkan-loader libxcb libx11 libxcursor libxi libxrandr;
  };

  # The workspace tests need a network and pcscd: CI and the NixOS VM test run them.
  rust-crane.runTests = false;
  # crane (v0.19, pinned by dream2nix) strips references to the vendored
  # crate sources with ONE GNU sed whose regex alternates a few hundred
  # store hashes; over the release binary that gets OOM-killed on CI
  # ("xargs: sed: terminated by signal 9"). postInstall does the same
  # rewrite with perl, which handles the alternation in bounded memory.
  package-func.args.doNotRemoveReferencesToVendorDir = true;
  rust-crane.depsDrv.mkDerivation = toolchain;
  rust-crane.depsDrv.package-func.args.extraDummyScript = ''
    rm -rf $out/third_party/gpui-glass/vendor/gpui-ce
    cp -r ${gpuiVendor} $out/third_party/gpui-glass/vendor/gpui-ce
    chmod -R u+w $out/third_party/gpui-glass/vendor/gpui-ce
  '';

  mkDerivation = toolchain // {
    inherit src;
    nativeBuildInputs = toolchain.nativeBuildInputs ++ [ config.deps.patchelf config.deps.perl ];

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

      # crane's removeReferencesToVendoredSources (see doNotRemove... above),
      # same targets and replacement: the vendor dir and every crate source
      # it links to, rewritten to an all-e hash so the closure keeps none.
      hashes=$(
        { echo "$cargoVendorDir"
          find -L "$cargoVendorDir" -mindepth 1 -maxdepth 1 -type d -print0 |
            xargs -0 -I DIR find -H DIR -type l -exec readlink '{}' \;
        } | grep -o "$NIX_STORE/[a-z0-9]\{32\}" | sed "s|^$NIX_STORE/||" | sort -u | paste -sd'|' -
      )
      if [ -n "$hashes" ]; then
        echo "stripping references to $(echo "$hashes" | tr '|' '\n' | wc -l) vendored sources"
        find "$out" -type f -print0 |
          xargs -0 perl -0777 -pi -e "s!\Q$NIX_STORE\E/(?:$hashes)!$NIX_STORE/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee!g"
      fi
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
    postFixup = "";
  };
}
