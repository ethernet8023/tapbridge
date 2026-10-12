# tapbridge with nix-darwin

If you manage your Mac with [nix-darwin](https://github.com/nix-darwin/nix-darwin), you can install tapbridge and set all of its options in your config. You don't need the installer package.

When tapbridge is installed this way, its Preferences window shows your settings grayed out and links back here. To change anything, edit your config and run `darwin-rebuild switch` again.

## Setup

Add tapbridge to your flake's inputs and import its module:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    nix-darwin.url = "github:nix-darwin/nix-darwin";
    nix-darwin.inputs.nixpkgs.follows = "nixpkgs";
    tapbridge.url = "github:ethernet8023/tapbridge";
  };

  outputs = { nix-darwin, tapbridge, ... }: {
    darwinConfigurations.my-mac = nix-darwin.lib.darwinSystem {
      system = "aarch64-darwin";
      modules = [
        tapbridge.darwinModules.default
        {
          programs.tapbridge.enable = true;
        }
      ];
    };
  };
}
```

Then:

```sh
darwin-rebuild switch --flake .#my-mac
```

That installs the app at `/run/current-system/Applications/tapbridge.app` and the card reader driver. tapbridge starts the next time you log in. To start it now, run `open /run/current-system/Applications/tapbridge.app`.

If you installed the `.pkg` before, remove that copy first so there aren't two: `sudo rm -rf /Applications/tapbridge.app`.

## Options

Everything is under `programs.tapbridge`. The defaults match a fresh macOS install, except that tapbridge starts at login. The table below is generated from the module's option descriptions (`nix run .#docs`); edit those, not the table.

<!-- BEGIN generated options: nix run .#docs -->
| Option | Default | What it does |
|---|---|---|
| `enable` | `false` | Whether to enable tapbridge, your phone as the Mac's NFC card reader. |
| `startAtLogin` | `true` | Start at login. Open tapbridge in the menu bar when you log in. |
| `useUsb` | `true` | Connect to phone via USB. The phone can also reach the Mac over a USB cable, which works without a network. Required by `smartCard.enforceSmartCard`. |
| `alwaysPresent` | `false` | Card always ready. Your login card shows as inserted even before you tap it, so the lock screen and sudo ask for it straight away. If you don't tap within 25 seconds, you can use your password. |
| `smartCard.allowSmartCard` | `true` | Log in with smart cards. Cards can log in, unlock the screen and approve admin prompts. Needs a reboot. (`allowSmartCard`) |
| `smartCard.userPairing` | `true` | Offer to pair new cards. macOS asks to link a card to your account when it's tapped or inserted. (`UserPairing`) |
| `smartCard.oneCardPerUser` | `false` | One card per user. Each user account can be paired with only one card. (`oneCardPerUser`) |
| `smartCard.lockOnRemoval` | `false` | Lock when the card is removed. The screen locks when your card goes away. (`tokenRemovalAction`) |
| `smartCard.sudo` | `true` | Card for sudo. Approve sudo prompts with your card. macOS ships with this on; `false` removes it. (`pam_smartcard.so` in /etc/pam.d/sudo) |
| `smartCard.su` | `false` | Card for su. Approve su prompts with your card. (`pam_smartcard.so` in /etc/pam.d/su) |
| `smartCard.login` | `false` | Card for login. Approve the login command with your card. (`pam_smartcard.so` in /etc/pam.d/login) |
| `smartCard.ssh.client` | `false` | SSH with your card. ssh offers your card's key to servers. (`PKCS11Provider /usr/lib/ssh-keychain.dylib`) |
| `smartCard.ssh.serverCardsOnly` | `false` | Don't accept passwords in SSHD. This Mac's SSH server only accepts keys and cards. |
| `smartCard.enforceSmartCard` | `false` | Smart card only. Passwords stop working: no card, no login. tapbridge must be running and your phone reachable. Make sure card login works first. This option requires tapbridge's USB support to be enabled (`useUsb`). (`enforceSmartCard`) |
| `smartCard.checkCertificateTrust` | `0` | Certificate check: only accept cards whose certificate a trusted authority issued. 0 off, 1 check, 2 check + soft revocation, 3 check + hard revocation. tapbridge makes self-signed certificates, so leave this at 0. (`checkCertificateTrust`) |
<!-- END generated options -->

These follow Apple's guide, [Configure macOS for smart card-only authentication](https://support.apple.com/en-us/101438).

### Example

```nix
programs.tapbridge = {
  enable = true;
  alwaysPresent = true;
  smartCard = {
    sudo = true;
    ssh.client = true;
  };
};
```

## Before you turn on "smart card only"

With `enforceSmartCard = true`, a card is the only way to log in, unlock or use `sudo`. With tapbridge, the card is only reachable while:

- tapbridge is running, which it isn't at the login screen after a restart,
- your phone is on and can be reached over the network, and
- your phone and Mac are still paired.

tapbridge won't let you turn this on without `useUsb` (in the app, or in nix-darwin, where it fails the build): with a USB cable, the phone can still reach the Mac when the network is down.

If any of these fail, you're locked out. So:

1. Pair your card with your account and check that you can log in with it.
2. Keep a second admin account that isn't paired with a card, or know the Recovery steps below.
3. Only then turn on `enforceSmartCard`.

**To undo it from macOS Recovery:** start up in Recovery, mount your disk in Disk Utility, open Terminal and run:

```sh
rm "/Volumes/Macintosh HD/Library/Preferences/com.apple.security.smartcard.plist"
```

Then restart, set `enforceSmartCard = false` and run `darwin-rebuild switch`.

## How it works

The module:

- writes your options to `/etc/tapbridge/config`. tapbridge reads that file, and its presence is how the app knows nix-darwin manages it.
- sets the `com.apple.security.smartcard` preferences.
- adds or removes `pam_smartcard.so` in `/etc/pam.d/sudo`, `su` and `login`. macOS updates can reset these files; the next `darwin-rebuild switch` fixes them.
- writes `/etc/ssh/ssh_config.d/050-tapbridge.conf` and `/etc/ssh/sshd_config.d/050-tapbridge.conf` for the SSH options.
- copies the reader driver to `/usr/local/libexec/SmartCardServices/drivers/` and registers it in `/etc/reader.conf`, as the installer package does.
- starts tapbridge at login with a launchd agent.

### Limits

- The Nix-built app has no app icon, because compiling it needs Xcode.
- The virtual security key (passkeys in the browser) needs an Apple entitlement, so it only works with the signed `.pkg` build, or with AMFI turned off.
- Each rebuild changes the app's signature, and macOS may ask you to allow keychain access again. If tapbridge forgets your phone, pair it again.
