# Pieces the NixOS and Home Manager modules share: the options both have
# and the settings the app reads. Docs for both are generated from these.
{ lib, pkgs }:

let
  inherit (lib) mkOption types;
  bool = default: description: mkOption { type = types.bool; inherit default description; };
in
{
  # Options under `services.tapbridge` (NixOS) and `programs.tapbridge` (Home Manager).
  sharedOptions = {
    package = mkOption {
      type = types.package;
      description = "The tapbridge package (set by the flake's module).";
    };

    startAtLogin = bool true ''
      Start at login. tapbridge starts with your graphical session, as a
      systemd user service.
    '';

    useUsb = bool true ''
      Connect to phone via USB. The phone can also reach the PC over a USB
      cable, which works without a network.
    '';
  };

  # The app's settings, as the environment of its systemd unit. TAPBRIDGE_MANAGED
  # grays out the Settings window and stops the app writing its own autostart entry.
  environment = { cfg, socket }: {
    TAPBRIDGE_MANAGED = "1";
    TAPBRIDGE_START_AT_LOGIN = lib.boolToString cfg.startAtLogin;
    TAPBRIDGE_IFD_SOCKET = socket;
    # The Settings window's "Register" for sudo and su runs this.
    TAPBRIDGE_PAMU2FCFG = "${pkgs.pam_u2f}/bin/pamu2fcfg";
  };

  # Run before the app: the USB setting is the absence of `usb-off` in its state dir.
  usbSetup = cfg: pkgs.writeShellScript "tapbridge-usb-setting" ''
    dir="''${XDG_STATE_HOME:-$HOME/.local/state}/tapbridge"
    mkdir -p "$dir"
    ${if cfg.useUsb then ''rm -f "$dir/usb-off"'' else ''touch "$dir/usb-off"''}
  '';

  # Where each user's reader socket is: the NixOS module creates the
  # directory and writes the path into reader.conf; Home Manager defaults
  # to the same path.
  socketFor = user: "/run/tapbridge/${user}/ifd.sock";
}
