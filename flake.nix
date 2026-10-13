{
  description = "tapbridge dev shell: Rust (host + Android target), JDK 17, Android SDK/NDK, adb";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Builds the Linux app as dependencies-only + workspace derivations, so
    # editing our code never recompiles the (huge) dependency tree.
    dream2nix = {
      url = "github:nix-community/dream2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, dream2nix, ... }:
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
      linuxSystems = [ "x86_64-linux" "aarch64-linux" ];
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      tapbridgeFor = pkgs: pkgs.callPackage ./nix/package.nix {
        src = pkgs.lib.cleanSource ./.;
        inherit version;
      };
      # Only what the build reads: editing the modules, docs or tests must not
      # change the source (and so not recompile the workspace).
      linuxSrc = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./crates
          ./third_party
          ./packaging/linux
        ];
      };
      # dream2nix's rust-crane module: crane's buildDepsOnly (every dependency,
      # keyed only by Cargo.lock) feeding buildPackage (our crates).
      tapbridgeLinuxWith = tests: pkgs: dream2nix.lib.evalModules {
        packageSets.nixpkgs = pkgs;
        modules = [
          (import ./nix/package-linux.nix { src = linuxSrc; inherit version tests; })
          {
            paths.projectRoot = ./.;
            paths.projectRootFile = "flake.nix";
            paths.package = ./.;
          }
        ];
      };
      tapbridgeLinuxFor = tapbridgeLinuxWith false;
    in
    {
      packages =
        nixpkgs.lib.genAttrs darwinSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          rec {
            tapbridge = tapbridgeFor pkgs;
            default = tapbridge;
            # The programs.tapbridge options table, from the module itself.
            options-doc = (import ./nix/options-doc.nix { inherit (pkgs) lib; inherit pkgs; }).darwin;
          })
        // nixpkgs.lib.genAttrs linuxSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          rec {
            tapbridge = tapbridgeLinuxFor pkgs;
            default = tapbridge;
            options-doc-nixos = (import ./nix/options-doc.nix { inherit (pkgs) lib; inherit pkgs; }).nixos;
            options-doc-home = (import ./nix/options-doc.nix { inherit (pkgs) lib; inherit pkgs; }).home;
          });

      # `nix run .#docs`: rewrites the options tables in docs/ (nix-darwin.md
      # on macOS; nixos.md and home-manager.md on Linux).
      apps =
        let
          docsApp = pkgs: files: {
            type = "app";
            program = toString (pkgs.writeShellScript "tapbridge-docs" ''
              set -euo pipefail
              root="$(git rev-parse --show-toplevel)"
              ${nixpkgs.lib.concatMapStringsSep "\n" (f: ''
                ${pkgs.python3}/bin/python3 ${./nix/splice-doc.py} "$root/docs/${f.file}" ${f.table}
              '') files}
            '');
          };
        in
        nixpkgs.lib.genAttrs darwinSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          { docs = docsApp pkgs [{ file = "nix-darwin.md"; table = self.packages.${system}.options-doc; }]; })
        // nixpkgs.lib.genAttrs linuxSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          {
            docs = docsApp pkgs [
              { file = "nixos.md"; table = self.packages.${system}.options-doc-nixos; }
              { file = "home-manager.md"; table = self.packages.${system}.options-doc-home; }
            ];
          });

      # Fails when a docs table is out of date. On Linux the check also builds
      # the NixOS VM test (checks.<system>.vm).
      checks =
        let
          docsCheck = pkgs: files: pkgs.runCommand "tapbridge-docs-check" { } ''
            ${nixpkgs.lib.concatMapStringsSep "\n" (f: ''
              cp ${./docs + "/${f.file}"} doc.md
              chmod +w doc.md
              ${pkgs.python3}/bin/python3 ${./nix/splice-doc.py} doc.md ${f.table}
              if ! diff -u ${./docs + "/${f.file}"} doc.md; then
                echo "docs/${f.file} is out of date: run nix run .#docs" >&2
                exit 1
              fi
            '') files}
            touch $out
          '';
        in
        nixpkgs.lib.genAttrs darwinSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          { docs = docsCheck pkgs [{ file = "nix-darwin.md"; table = self.packages.${system}.options-doc; }]; })
        // nixpkgs.lib.genAttrs linuxSystems (system:
          let pkgs = nixpkgs.legacyPackages.${system}; in
          {
            docs = docsCheck pkgs [
              { file = "nixos.md"; table = self.packages.${system}.options-doc-nixos; }
              { file = "home-manager.md"; table = self.packages.${system}.options-doc-home; }
            ];
          }
          # The VM test needs KVM, so it only runs where CI has it.
          // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
            vm = import ./nix/vm-test.nix {
              inherit pkgs;
              module = self.nixosModules.default;
              package = self.packages.${system}.tapbridge;
              tests = tapbridgeLinuxWith true pkgs;
            };
          });

      # nix-darwin: `programs.tapbridge`. See docs/nix-darwin.md.
      darwinModules = rec {
        tapbridge = { pkgs, ... }: {
          imports = [ ./nix/darwin-module.nix ];
          programs.tapbridge.package = nixpkgs.lib.mkDefault (tapbridgeFor pkgs);
        };
        default = tapbridge;
      };

      # NixOS: `services.tapbridge`; Home Manager: `programs.tapbridge`. See docs/nixos.md.
      nixosModules = rec {
        tapbridge = { pkgs, ... }: {
          imports = [ ./nix/nixos-module.nix ];
          services.tapbridge.package = nixpkgs.lib.mkDefault (tapbridgeLinuxFor pkgs);
        };
        default = tapbridge;
      };
      homeModules = rec {
        tapbridge = { pkgs, ... }: {
          imports = [ ./nix/home-module.nix ];
          programs.tapbridge.package = nixpkgs.lib.mkDefault (tapbridgeLinuxFor pkgs);
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
          # Linux desktop build inputs (gpui + tray-icon); the dlopen'd ones also
          # go on LD_LIBRARY_PATH so `cargo run` finds them.
          linuxLibs = with pkgs; [
            gtk3 libayatana-appindicator glib cairo pango gdk-pixbuf atk xdotool
            wayland libxkbcommon vulkan-loader fontconfig freetype openssl
            libxcb libx11 libxcursor libxi libxrandr udev pcsclite dbus
          ];
          isLinux = pkgs.stdenv.hostPlatform.isLinux;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.cargo-ndk
              jdk
              android.androidsdk
              android.platform-tools
            ] ++ commands ++ pkgs.lib.optionals isLinux ([ pkgs.pkg-config ] ++ linuxLibs);
            LD_LIBRARY_PATH = pkgs.lib.optionalString isLinux (pkgs.lib.makeLibraryPath linuxLibs);
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
