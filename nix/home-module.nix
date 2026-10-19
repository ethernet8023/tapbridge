# Home Manager module: `programs.tapbridge`. See docs/nixos.md.
#
# Installs the app and runs it as a systemd user service. The PC/SC reader
# and the udev rules are system settings: with NixOS, `services.tapbridge`
# sets them up. Without it the app still pairs with the phone, but PIV
# needs a reader.conf entry (see docs/nixos.md, "Without NixOS").
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.tapbridge;
  common = import ./linux-common.nix { inherit lib pkgs; };
  inherit (lib) mkOption mkEnableOption mkIf types;
in
{
  options.programs.tapbridge = common.sharedOptions // {
    enable = mkEnableOption "tapbridge, your phone as the PC's NFC card reader and FIDO security key";

    ifdSocket = mkOption {
      type = types.str;
      default = common.socketFor config.home.username;
      defaultText = lib.literalExpression ''"/run/tapbridge/''${config.home.username}/ifd.sock"'';
      description = ''
        The unix socket the PC/SC reader driver talks to. It must be the
        path in `DEVICENAME unix:<path>` of this user's reader in pcscd's reader.conf, which
        `services.tapbridge` on NixOS sets to the default.
      '';
    };
  };

  config = mkIf cfg.enable {
    # Deliberately NOT home.packages: a bare `tapbridge` runs without the
    # unit's environment (no TAPBRIDGE_MANAGED, reader socket or pamu2fcfg).
    # To bring the app back after Quit: systemctl --user start tapbridge.
    systemd.user.services.tapbridge = {
      Unit = {
        Description = "tapbridge: your phone as the NFC reader for your security key";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        Environment = lib.mapAttrsToList (name: value: "${name}=${value}") (common.environment {
          inherit cfg;
          socket = cfg.ifdSocket;
        });
        ExecStartPre = "${common.usbSetup cfg}";
        ExecStart = "${cfg.package}/bin/tapbridge";
        Restart = "on-failure";
      };
      Install.WantedBy = lib.optional cfg.startAtLogin "graphical-session.target";
    };
  };
}
