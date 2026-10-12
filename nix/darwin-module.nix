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

    startAtLogin = bool true "Start tapbridge in the menu bar when you log in.";

    alwaysPresent = bool false ''
      "Card always ready": your login card shows as inserted before you tap
      it, so the lock screen and sudo ask for it straight away. If you don't
      tap within 25 seconds, the card is reported gone and the password works.
    '';

    smartCard = {
      allowSmartCard = bool true ''
        `allowSmartCard`: cards can log in, unlock the screen and approve
        admin prompts. Takes effect after a restart.
      '';
      userPairing = bool true ''
        `UserPairing`: macOS offers to pair a card with your account when it's inserted.
      '';
      checkCertificateTrust = mkOption {
        type = types.ints.between 0 3;
        default = 0;
        description = ''
          `checkCertificateTrust`: 0 off, 1 check, 2 check with soft
          revocation, 3 check with hard revocation. tapbridge makes
          self-signed certificates, so keep 0 unless you use your own CA.
        '';
      };
      oneCardPerUser = bool false "`oneCardPerUser`: each account can be paired with only one card.";
      lockOnRemoval = bool false "`tokenRemovalAction`: lock the screen when the card goes away.";
      enforceSmartCard = bool false ''
        `enforceSmartCard`: passwords stop working; only a card can log in or
        authenticate. tapbridge must be running and your phone reachable.
        Make sure card login works first, and know how to undo this from
        macOS Recovery.
      '';
      sudo = bool true ''
        Allow your card for `sudo` (`pam_smartcard.so` in /etc/pam.d/sudo).
        macOS ships with this on; `false` removes it.
      '';
      su = bool false "Allow your card for `su`.";
      login = bool false "Allow your card for the `login` command.";
      ssh = {
        client = bool false "ssh offers the card's key (`PKCS11Provider /usr/lib/ssh-keychain.dylib`).";
        serverCardsOnly = bool false "This Mac's SSH server refuses passwords (keys and cards only).";
      };
    };
  };

  config = mkIf cfg.enable {
    assertions = [{
      assertion = !(sc.enforceSmartCard && !sc.allowSmartCard);
      message = "programs.tapbridge.smartCard.enforceSmartCard needs allowSmartCard.";
    }];

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
