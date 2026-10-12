# nix-darwin module: `programs.tapbridge`. See docs/nix-darwin.md.
#
# Installs what the macOS pkg installs (the app, the PC/SC reader driver
# and /etc/reader.conf), starts the app at login, and sets every Mac smart
# card option from Apple's "Configure macOS for smart card-only
# authentication" (support.apple.com/101438). The app reads
# /etc/tapbridge/config, shows these settings grayed out, and links here.
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.tapbridge;
  sc = cfg.smartCard;
  inherit (lib) mkOption mkEnableOption mkIf types optionalString concatStringsSep;

  bool = default: description: mkOption { type = types.bool; inherit default description; };

  app = "${cfg.package}/Applications/tapbridge.app";
  driver = "/usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle";

  # Read by the app (crates/desktop/src/mac_security.rs: parse_config).
  configText = concatStringsSep "\n" (lib.mapAttrsToList (k: v: "${k} = ${toString v}") {
    start_at_login = lib.boolToString cfg.startAtLogin;
    always_present = lib.boolToString cfg.alwaysPresent;
    use_usb = lib.boolToString cfg.useUsb;
    allow_smart_card = lib.boolToString sc.allowSmartCard;
    user_pairing = lib.boolToString sc.userPairing;
    check_certificate_trust = sc.checkCertificateTrust;
    one_card_per_user = lib.boolToString sc.oneCardPerUser;
    lock_on_removal = lib.boolToString sc.lockOnRemoval;
    enforce_smart_card = lib.boolToString sc.enforceSmartCard;
    sudo = lib.boolToString sc.sudo;
    su = lib.boolToString sc.su;
    login = lib.boolToString sc.login;
    ssh_client = lib.boolToString sc.ssh.client;
    sshd_cards_only = lib.boolToString sc.ssh.serverCardsOnly;
  }) + "\n";

  pamLine = "auth       sufficient     pam_smartcard.so # tapbridge";
  # Adds or removes our line in a PAM file macOS owns (it may be reset by
  # macOS updates; the next activation puts it back).
  pamEdit = file: on: if on then ''
    if ! grep -q 'pam_smartcard.so' ${file}; then
      awk -v l='${pamLine}' '!d && /^auth/ && !/sudo_local/ {print l; d=1} {print}' ${file} > ${file}.tapbridge-new
      cat ${file}.tapbridge-new > ${file}; rm -f ${file}.tapbridge-new
    fi
  '' else ''
    if grep -q 'pam_smartcard.so' ${file}; then
      grep -v 'pam_smartcard.so' ${file} > ${file}.tapbridge-new
      cat ${file}.tapbridge-new > ${file}; rm -f ${file}.tapbridge-new
    fi
  '';
in
{
  options.programs.tapbridge = {
    enable = mkEnableOption "tapbridge, your phone as the Mac's NFC card reader";

    package = mkOption {
      type = types.package;
      description = "The tapbridge package (set by the flake's module).";
    };

    startAtLogin = bool true "Start at login. Open tapbridge in the menu bar when you log in.";

    alwaysPresent = bool false ''
      Card always ready. Your login card shows as inserted even before you
      tap it, so the lock screen and sudo ask for it straight away. If you
      don't tap within 25 seconds, you can use your password.
    '';

    useUsb = bool true ''
      Connect to phone via USB. The phone can also reach the Mac over a USB
      cable, which works without a network. Required by
      `smartCard.enforceSmartCard`.
    '';

    smartCard = {
      allowSmartCard = bool true ''
        Log in with smart cards. Cards can log in, unlock the screen and
        approve admin prompts. Needs a reboot. (`allowSmartCard`)
      '';
      userPairing = bool true ''
        Offer to pair new cards. macOS asks to link a card to your account
        when it's tapped or inserted. (`UserPairing`)
      '';
      checkCertificateTrust = mkOption {
        type = types.ints.between 0 3;
        default = 0;
        description = ''
          Certificate check: only accept cards whose certificate a trusted
          authority issued. 0 off, 1 check, 2 check + soft revocation, 3
          check + hard revocation. tapbridge makes self-signed certificates,
          so leave this at 0. (`checkCertificateTrust`)
        '';
      };
      oneCardPerUser = bool false ''
        One card per user. Each user account can be paired with only one
        card. (`oneCardPerUser`)
      '';
      lockOnRemoval = bool false ''
        Lock when the card is removed. The screen locks when your card goes
        away. (`tokenRemovalAction`)
      '';
      sudo = bool true ''
        Card for sudo. Approve sudo prompts with your card. macOS ships with
        this on; `false` removes it. (`pam_smartcard.so` in /etc/pam.d/sudo)
      '';
      su = bool false ''
        Card for su. Approve su prompts with your card. (`pam_smartcard.so`
        in /etc/pam.d/su)
      '';
      login = bool false ''
        Card for login. Approve the login command with your card.
        (`pam_smartcard.so` in /etc/pam.d/login)
      '';
      ssh = {
        client = bool false ''
          SSH with your card. ssh offers your card's key to servers.
          (`PKCS11Provider /usr/lib/ssh-keychain.dylib`)
        '';
        serverCardsOnly = bool false ''
          Don't accept passwords in SSHD. This Mac's SSH server only accepts
          keys and cards.
        '';
      };
      enforceSmartCard = bool false ''
        Smart card only. Passwords stop working: no card, no login.
        tapbridge must be running and your phone reachable. Make sure card
        login works first. This option requires tapbridge's USB support to
        be enabled (`useUsb`). (`enforceSmartCard`)
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = !(sc.enforceSmartCard && !sc.allowSmartCard);
        message = "programs.tapbridge.smartCard.enforceSmartCard needs allowSmartCard.";
      }
      {
        assertion = !(sc.enforceSmartCard && !cfg.useUsb);
        message = ''
          programs.tapbridge.smartCard.enforceSmartCard needs useUsb: without
          the USB cable, a network problem would lock you out of the Mac.
        '';
      }
    ];

    environment.systemPackages = [ cfg.package ];
    environment.etc."tapbridge/config".text = configText;

    # The smart card keys, as a managed preference domain.
    system.defaults.CustomSystemPreferences."com.apple.security.smartcard" = {
      allowSmartCard = sc.allowSmartCard;
      UserPairing = sc.userPairing;
      checkCertificateTrust = sc.checkCertificateTrust;
      oneCardPerUser = sc.oneCardPerUser;
      tokenRemovalAction = if sc.lockOnRemoval then 1 else 0;
      enforceSmartCard = sc.enforceSmartCard;
    };

    environment.etc."ssh/ssh_config.d/050-tapbridge.conf" = mkIf sc.ssh.client {
      text = "Host *\n    PKCS11Provider /usr/lib/ssh-keychain.dylib\n";
    };
    environment.etc."ssh/sshd_config.d/050-tapbridge.conf" = mkIf sc.ssh.serverCardsOnly {
      text = "PasswordAuthentication no\nKbdInteractiveAuthentication no\n";
    };

    launchd.user.agents.tapbridge = mkIf cfg.startAtLogin {
      serviceConfig = {
        ProgramArguments = [ "${app}/Contents/MacOS/tapbridge" ];
        RunAtLoad = true;
        KeepAlive = false;
        ProcessType = "Interactive";
      };
    };

    system.activationScripts.postActivation.text = ''
      echo >&2 "setting up tapbridge..."
      # The PC/SC reader driver must be a real directory: ifdreader won't
      # follow links into /nix/store.
      mkdir -p /usr/local/libexec/SmartCardServices/drivers
      rm -rf ${driver}
      cp -R ${cfg.package}/libexec/SmartCardServices/drivers/tapbridge.bundle ${driver}
      chown -R root:wheel ${driver}
      chmod -R a+rX ${driver}
      if [ ! -e /etc/reader.conf ] || grep -q tapbridge /etc/reader.conf; then
        cat > /etc/reader.conf <<'CONF'
      # tapbridge virtual PIV reader (written by nix-darwin programs.tapbridge).
      FRIENDLYNAME "tapbridge PIV reader"
      DEVICENAME   /dev/null
      LIBPATH      ${driver}
      CHANNELID    0
      CONF
        chmod 644 /etc/reader.conf
      fi
      killall -SIGKILL com.apple.ifdreader 2>/dev/null || true

      ${pamEdit "/etc/pam.d/sudo" sc.sudo}
      ${pamEdit "/etc/pam.d/su" sc.su}
      ${pamEdit "/etc/pam.d/login" sc.login}
      ${optionalString (sc.ssh.serverCardsOnly) "launchctl kickstart -k system/com.openssh.sshd 2>/dev/null || true"}
    '';
  };
}
