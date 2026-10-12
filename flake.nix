{
  description = "tapbridge dev shell: Rust (host + Android target), JDK 17, Android SDK/NDK, adb";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        config = {
          allowUnfree = true; # Android SDK
          android_sdk.accept_license = true;
        };
      }));
      darwinSystems = [ "aarch64-darwin" "x86_64-darwin" ];
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      tapbridgeFor = pkgs: pkgs.callPackage ./nix/package.nix {
        src = pkgs.lib.cleanSource ./.;
        inherit version;
      };
    in
    {
      packages = nixpkgs.lib.genAttrs darwinSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        rec {
          tapbridge = tapbridgeFor pkgs;
          default = tapbridge;
        });

      # nix-darwin: `programs.tapbridge`. See docs/nix-darwin.md.
      darwinModules = rec {
        tapbridge = { pkgs, ... }: {
          imports = [ ./nix/darwin-module.nix ];
          programs.tapbridge.package = nixpkgs.lib.mkDefault (tapbridgeFor pkgs);
        };
        default = tapbridge;
      };

      devShells = forAll (pkgs:
        let
          # Versions pinned to android/app/build.gradle.kts (compileSdk 36,
          # ndkVersion 27.2.12479018; AGP 8.9 defaults to build-tools 35).
          android = pkgs.androidenv.composeAndroidPackages {
            platformVersions = [ "36" ];
            buildToolsVersions = [ "35.0.0" "36.0.0" ];
            includeNDK = true;
            ndkVersions = [ "27.2.12479018" ];
            includeEmulator = false;
            includeSystemImages = false;
          };
          sdk = "${android.androidsdk}/libexec/android-sdk";
          rust = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "clippy" "rustfmt" ];
            targets = [ "aarch64-linux-android" ];
          };
          jdk = pkgs.jdk17;
          # Dev shell commands; each runs the repo script of the same job.
          repoScript = name: script: pkgs.writeShellScriptBin name ''
            root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
            exec "$root/scripts/${script}" "$@"
          '';
          commands = [
            (repoScript "build-android" "build-android.sh")
          ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            (repoScript "build-and-run-macos-pkg" "run-macos.sh")
          ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.cargo-ndk
              jdk
              android.androidsdk
              android.platform-tools
            ] ++ commands;
            JAVA_HOME = jdk.home;
            ANDROID_HOME = sdk;
            ANDROID_SDK_ROOT = sdk;
            ANDROID_NDK_HOME = "${sdk}/ndk/27.2.12479018";
            # The Nix SDK is read-only; point AGP's aapt2 at the pinned build-tools
            # instead of letting it try to install anything.
            GRADLE_OPTS = "-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdk}/build-tools/36.0.0/aapt2";
          };
        });
    };
}
