# tapbridge on Linux with Nix

tapbridge on Linux gives you two things from the phone's NFC reader:

- a **PC/SC smart card reader** (named `tapbridge`) for PIV, OpenSC, `ssh-keygen -K` and anything else that speaks PC/SC, and
- a **virtual FIDO security key**, so WebAuthn and passkeys in Firefox and Chromium ask for a tap on your phone.

When something needs the card, tapbridge wakes your phone (a notification), you tap your key on the phone, and the request goes through.

## NixOS

Add tapbridge to your flake's inputs and import its module:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    tapbridge.url = "github:ethernet8023/tapbridge";
  };

  outputs = { nixpkgs, tapbridge, ... }: {
    nixosConfigurations.my-pc = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        tapbridge.nixosModules.default
        {
          services.tapbridge = {
            enable = true;
            users = [ "alice" ];
          };
        }
      ];
    };
  };
}
```

Then `nixos-rebuild switch --flake .#my-pc`, log out and in (so the udev access applies to your session), and open tapbridge from your app menu or let it start with your session. Pair your phone from its tray icon.

The module:

- installs tapbridge and its udev rules (access to your phone's USB port, and to `/dev/uhid` for the virtual key),
- turns on `services.pcscd` and adds one reader per user in `users` (no USB device is involved: pcscd's `reader.conf` names the driver and a socket that only that user and pcscd can open),
- lets those users use pcscd without an active local session (polkit), and
- runs tapbridge as a systemd user service.

When tapbridge is installed this way, its Settings window shows your settings grayed out and links here. To change anything, edit your configuration and rebuild.

### Options

Everything is under `services.tapbridge`. The table below is generated from the module's option descriptions (`nix run .#docs`); edit those, not the table.

<!-- BEGIN generated options: nix run .#docs -->
| Option | Default | What it does |
|---|---|---|
| `enable` | `false` | Whether to enable tapbridge, your phone as the PC's NFC card reader and FIDO security key. |
| `users` | `[ ]` | Users who get a tapbridge card reader. Each gets a virtual PC/SC reader whose socket only that user can open, so PIV and OpenSC work for them. |
| `startAtLogin` | `true` | Start at login. tapbridge starts with your graphical session, as a systemd user service. |
| `useUsb` | `true` | Connect to phone via USB. The phone can also reach the PC over a USB cable, which works without a network. |
| `virtualSecurityKey` | `true` | Show browsers a virtual FIDO security key (for WebAuthn and passkeys), backed by the card on your phone. This lets the logged-in user open `/dev/uhid`, which any program of theirs can also use to create virtual input devices. |
| `pam.su` | `false` | Let a tap on your phone authenticate `su`, the same way as `pam.sudo`. |
| `pam.sudo` | `false` | Let a tap on your phone authenticate `sudo` (through pam_u2f and the virtual security key). Needs `virtualSecurityKey`. Each user first registers the key once with `pamu2fcfg > ~/.config/Yubico/u2f_keys`; the password still works when the key is absent. |
<!-- END generated options -->

## Home Manager

If you only manage your user's config, use the Home Manager module for the app and its service. The reader and the udev rules are system settings, so on NixOS keep `services.tapbridge` too (it sets the socket path Home Manager defaults to). See [home-manager.md](home-manager.md).

## Without NixOS

Build or install the package (`nix profile install github:ethernet8023/tapbridge`), then do by hand what the module does:

1. Copy `lib/udev/rules.d/*.rules` from the package to `/etc/udev/rules.d/` and run `udevadm control --reload && udevadm trigger`.
2. Run pcscd and add a reader to its configuration (a file in `/etc/reader.conf.d/`, or `/etc/reader.conf` on older versions). The `DEVICENAME` must be `unix:` plus the socket tapbridge listens on, `$XDG_RUNTIME_DIR/tapbridge/ifd.sock` by default (or set `TAPBRIDGE_IFD_SOCKET`). Keep the `unix:` prefix: without a colon pcscd checks the path exists when it starts, and refuses to start while tapbridge is not running:

   ```
   FRIENDLYNAME "tapbridge"
   DEVICENAME unix:/run/user/1000/tapbridge/ifd.sock
   LIBPATH /path/to/lib/libtapbridge_ifd.so
   CHANNELID 1
   ```
   pcscd must be able to open that socket: root can, but a pcscd that runs as its own user (as on NixOS) needs a directory it can enter, which `$XDG_RUNTIME_DIR` is not.
3. Start `tapbridge`. Without a reader.conf entry the virtual security key still works; only PC/SC needs it.

## Notes

- `/dev/uhid` access lets any program of the logged-in user create virtual input devices (keyboards included), not just the security key. Turn it off with `virtualSecurityKey = false` if you only want the PC/SC reader.
- The pairing and your host key are stored in `$XDG_STATE_HOME/tapbridge`. When a Secret Service (GNOME Keyring, KWallet, KeePassXC) is running, the record is encrypted with a key kept there. Without one it is a plain file only you can read, like an SSH key.
