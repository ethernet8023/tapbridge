# NixOS module: `services.tapbridge`. See docs/nixos.md.
#
# Installs the app and its udev rules, adds one virtual PC/SC reader per
# listed user to pcscd (no USB device: reader.conf names the driver and the
# user's socket), and runs the app as a systemd user service.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.tapbridge;
  common = import ./linux-common.nix { inherit lib pkgs; };
  inherit (lib) mkOption mkEnableOption mkIf types;

  # One reader per user; the name shows up in `pcsc_scan`, OpenSC and browsers.
  readerName = user: if lib.length cfg.users == 1 then "tapbridge" else "tapbridge (${user})";
  readerConfig = lib.concatStringsSep "\n" (lib.imap1 (index: user: ''
    FRIENDLYNAME "${readerName user}"
    DEVICENAME unix:${common.socketFor user}
    LIBPATH ${cfg.package}/lib/libtapbridge_ifd.so
    CHANNELID ${toString index}
  '') cfg.users);

  # The phone's USB access always; the /dev/uhid and hidraw access only with the virtual key.
  udevRules = pkgs.runCommand "tapbridge-udev-rules" { } ''
    install -Dm644 ${cfg.package}/lib/udev/rules.d/70-tapbridge-usb.rules -t $out/lib/udev/rules.d
    ${lib.optionalString cfg.virtualSecurityKey ''
      install -Dm644 ${cfg.package}/lib/udev/rules.d/71-tapbridge-key.rules -t $out/lib/udev/rules.d
    ''}
  '';
in
{
  options.services.tapbridge = common.sharedOptions // {
    enable = mkEnableOption "tapbridge, your phone as the PC's NFC card reader and FIDO security key";

    users = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Users who get a tapbridge card reader. Each gets a virtual PC/SC
        reader whose socket only that user can open, so PIV and OpenSC work
        for them.
      '';
    };

    virtualSecurityKey = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Show browsers a virtual FIDO security key (for WebAuthn and passkeys),
        backed by the card on your phone. This lets the logged-in user open
        `/dev/uhid`, which any program of theirs can also use to create
        virtual input devices.
      '';
    };

    pam = {
      sudo = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Let a tap on your phone authenticate `sudo` (through pam_u2f and the
          virtual security key). Needs `virtualSecurityKey`. Each user first
          registers a card once: Settings, the card's gear, "Register" (or
          `pamu2fcfg > ~/.config/Yubico/u2f_keys`). The password still works
          when the key is absent.
        '';
      };

      su = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Let a tap on your phone authenticate `su`, the same way as `pam.sudo`.
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.users != [ ];
        message = "services.tapbridge.users is empty: list the users who get a tapbridge reader.";
      }
      {
        assertion = !(cfg.pam.sudo || cfg.pam.su) || cfg.virtualSecurityKey;
        message = "services.tapbridge.pam.sudo/su sign in with the virtual security key: set services.tapbridge.virtualSecurityKey = true.";
      }
    ];

    # pam_u2f talks to the virtual key like any other FIDO key. It stays
    # `sufficient` (the nixpkgs default), so the password still works whenever
    # the key is absent, such as when tapbridge isn't running.
    # Not `security.pam.u2f.enable`: that makes every PAM service (login, sshd,
    # the display manager) default to u2fAuth. Only the listed services get it.
    security.pam.u2f = mkIf (cfg.pam.sudo || cfg.pam.su) {
      # "Please touch the device": a tap on the phone is easy to miss without it.
      settings.cue = lib.mkDefault true;
    };
    security.pam.services.sudo.u2fAuth = mkIf cfg.pam.sudo true;
    security.pam.services.su.u2fAuth = mkIf cfg.pam.su true;

    # Deliberately NOT environment.systemPackages: a bare `tapbridge` runs
    # without the unit's environment (no TAPBRIDGE_MANAGED, reader socket or
    # pamu2fcfg) and half works. pcscd and udev get the package by store path;
    # to bring the app back after Quit: systemctl --user start tapbridge.
    services.udev.packages = [ udevRules ];
    # /dev/uhid only exists once the module is loaded (nothing else asks for it).
    boot.kernelModules = lib.optional cfg.virtualSecurityKey "uhid";

    services.pcscd = {
      enable = true;
      readerConfigs = [ readerConfig ];
    };

    # Each user's socket directory. pcscd runs as its own user, so the
    # directory is group `pcscd` (setgid: the app's socket inherits it);
    # everyone else has no access.
    systemd.tmpfiles.rules = map (user: "d /run/tapbridge/${user} 2750 ${user} pcscd -") cfg.users;

    # pcscd asks polkit before talking to a client; allow the listed users
    # outside an active local session too (ssh, the user service).
    security.polkit.extraConfig = mkIf config.security.polkit.enable ''
      polkit.addRule(function(action, subject) {
        if ((action.id == "org.debian.pcsc-lite.access_pcsc" ||
             action.id == "org.debian.pcsc-lite.access_card") &&
            ${lib.concatMapStringsSep " || " (user: ''subject.user == "${user}"'') cfg.users}) {
          return polkit.Result.YES;
        }
      });
    '';

    systemd.user.services.tapbridge = {
      description = "tapbridge: your phone as the NFC reader for your security key";
      wantedBy = lib.optional cfg.startAtLogin "graphical-session.target";
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      # The same unit runs for every user; each one's socket is theirs.
      environment = common.environment {
        inherit cfg;
        socket = "/run/tapbridge/%u/ifd.sock";
      };
      serviceConfig = {
        ExecStartPre = "${common.usbSetup cfg}";
        ExecStart = "${cfg.package}/bin/tapbridge";
        Restart = "on-failure";
      };
    };
  };
}
