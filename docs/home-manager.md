# tapbridge with Home Manager

The Home Manager module installs tapbridge and runs it as a systemd user service. The PC/SC reader and the udev rules belong to the system: on NixOS, add [`services.tapbridge`](nixos.md) as well (it creates the socket directory and the pcscd reader this module's `ifdSocket` points at). Without it, the app still pairs with your phone, but there is no PC/SC reader until you set one up by hand (see "Without NixOS" in [nixos.md](nixos.md)).

## Setup

```nix
{
  inputs.tapbridge.url = "github:ethernet8023/tapbridge";

  # in your Home Manager configuration:
  imports = [ tapbridge.homeModules.default ];
  programs.tapbridge.enable = true;
}
```

When tapbridge is installed this way, its Settings window shows your settings grayed out and links back to these docs. To change anything, edit your configuration and switch.

## Options

Everything is under `programs.tapbridge`. The table below is generated from the module's option descriptions (`nix run .#docs`); edit those, not the table.

<!-- BEGIN generated options: nix run .#docs -->
| Option | Default | What it does |
|---|---|---|
| `enable` | `false` | Whether to enable tapbridge, your phone as the PC's NFC card reader and FIDO security key. |
| `startAtLogin` | `true` | Start at login. tapbridge starts with your graphical session, as a systemd user service. |
| `useUsb` | `true` | Connect to phone via USB. The phone can also reach the PC over a USB cable, which works without a network. |
| `ifdSocket` | `"/run/tapbridge/${config.home.username}/ifd.sock"` | The unix socket the PC/SC reader driver talks to. It must be the path in `DEVICENAME unix:<path>` of this user's reader in pcscd's reader.conf, which `services.tapbridge` on NixOS sets to the default. |
<!-- END generated options -->
