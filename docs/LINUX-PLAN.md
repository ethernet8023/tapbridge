# tapbridge on Linux: plan

Goal: the phone is a PC/SC reader (PIV, OpenSC, smart-card login) AND a virtual FIDO security key (WebAuthn in Firefox/Chrome) on Linux, shipped as a package + a NixOS module + a Home Manager module.

Linux is "macOS track A + the mac virtual security key", not Windows: no usbip, no WebAuthN event log. Shared service/iroh/pairing/relay/phone code is untouched; the wire contract does not change.

Verified so far: `nix develop -c cargo check -p tapbridge-host` is clean on x86_64-linux. NOT verified: `tapbridge-desktop` on Linux (gpui + tray-icon), pcscd loading the shim.

## Design decisions (made, not open)

1. **Reader = pcscd IFD shim + unix socket**, same as macOS (`ifd_shim.c` <-> `host/src/ifd.rs`). The shim is header-free C, so the same source builds as a Linux `.so`.
2. **pcscd hotplug gotcha.** macOS loads the shim at startup. Linux pcscd only loads a driver when a matching USB device shows up (Info.plist VID/PID), and our reader is virtual. So use the vsmartcard/vpcd pattern: a `reader.conf` entry (`FRIENDLYNAME` / `DEVICENAME` / `LIBPATH`) makes pcscd load the `.so` with no USB device. `DEVICENAME` carries the per-user socket path (`$XDG_RUNTIME_DIR/tapbridge/ifd.sock`), so no world-writable `/var/tmp` socket and no hardcoded path. **Spike this first (step 1): it decides whether the reader works at all.**
3. **Virtual FIDO key = `/dev/uhid` CTAPHID device**, relayed to the phone session. Reuse `host::ctaphid::Engine`, `ctap_nfc::relay`, `ctap_pin::proxy` untouched. Split `desktop/src/security_key.rs` into a platform-neutral request loop (wait for card, wake phone, relay, TAP_TIMEOUT) and a `hid` backend: IOKit on mac, uhid on Linux.
3b. Ideas taken from `BryanJacobs/fido2-hid-bridge` (MIT, Python/UHID):
   - HID report descriptor: usage page `0xF1D0`, usage `0x01`, 64-byte input + output reports.
   - UHID `Bus::USB`, with open/close counting. When the open count hits 0, drop all channel state and release the card. Do the same with our session claim.
   - Evict idle channels after 30 s, cap simultaneous channels (~100), random non-zero non-broadcast CID on broadcast INIT.
   - Reply with CTAPHID ERR codes for unknown channel (0x0B), bad sequence (0x04), bad length (0x03), unknown cmd (0x01).
   - `systemd` unit ordered `After=pcscd.service`.
   - Do NOT copy: it goes through python-fido2 + PC/SC. Ours is the phone session, so no PC/SC in the middle. Our `ctaphid.rs` already has the state machine; only check it against that list of error cases.
4. **State protection on Linux:** Secret Service (`secret-service` crate / libsecret) seals the TBID2 record like keychain/DPAPI, with a 0600 file fallback when no keyring is running (headless). Log which one is in use. Never silently downgrade after the first sealed write.
5. **GUI:** keep gpui + tray if step 1's feasibility check passes. Fallback: tray-less `tapbridge --daemon` with the Settings window opened via CLI. Decide by the end of step 1, not before.
6. **Nix:** one package, two thin modules. NixOS module owns system pieces (pcscd, reader.conf, udev, polkit). Home Manager module owns the per-user service + settings. Both are optional on their own: HM-only works for the tray + phone link + WebAuthn if udev is set up some other way, and says so in its docs.

## Steps (each ends in a committed, verified state; branch off origin/main in a worktree, never main)

### 1. Feasibility spike: DONE
- `cargo check -p tapbridge-desktop` on Linux: gpui + gpui-glass + tray-icon all compile once the devShell has the Linux libs (gtk3, libayatana-appindicator, wayland, xkbcommon, vulkan-loader, udev, pcsclite, pkg-config; added to `flake.nix`). The only 2 errors left are the expected missing Linux arms: `autostart.rs` (`imp` unresolved) and `app_state.rs:1571` (`crate::macos::IFD_SOCKET_PATH`). So no headless-daemon fallback needed.
- reader.conf-only loading WORKS. A `reader.conf.d/` entry (`FRIENDLYNAME`/`DEVICENAME`/`LIBPATH`/`CHANNELID`) makes pcscd 2.4.1 load `ifd_shim.c` built as a plain `.so` with no USB device. `pcsc_scan` showed `tapbridge`, "Card inserted", and the ATR from a stub unix-socket server speaking the real ifd framing. This ran in a throwaway user+mount namespace, not as real root pcscd, so polkit and the real service user are still untested (step 5's VM test covers them).
- Shim change: the socket path now comes from `DEVICENAME` (`IFDHCreateChannelByName`), stored in a table keyed by `Lun >> 16` (pcscd's Lun is `reader_index << 16 | slot`). It falls back to the old `/var/tmp` path when the name is not a path, so macOS is unchanged.

### 2. Shared PC/SC reader (half a day)
- Remove `cfg(macos)` from `host/src/ifd.rs` (pure codec).
- Move the socket server and wake-on-PowerOn out of `desktop/src/macos.rs` into `desktop/src/pcsc_reader.rs`, used by mac and Linux. The socket path is a parameter, not a const (no global).
- `ifd_shim/build.sh`: `--linux` target producing `libtapbridge_ifd.so`. Verify the exported IFDHandler symbols as the mac script does.
- Test: codec unit tests stay. Add a socket-server test with a fake `ServiceHandle`.

### 3. Linux desktop shell (1 day)
- `state_dir()` -> `$XDG_STATE_HOME/tapbridge`.
- `autostart.rs`: XDG autostart `.desktop` (non-Nix users); the nix modules use a systemd user unit instead, so it reports "managed by Nix" in Settings (mirrors darwin module's behavior).
- `hostname.rs`, `login.rs` (smart-card login = PAM `pam_pkcs11`/`pam_p11`, out of scope; show nothing), `assets.rs`, tray.
- `host/state.rs`: Secret Service backend (decision 4).
- `usb_link.rs`: nusb AOA on Linux. Needs udev (step 5); handle permission-denied with an actionable log line ("install the udev rules").

### 4. Virtual security key via uhid (1 day)
- New `desktop/src/uhid_key.rs`: create the device through `/dev/uhid` (try the `uhid-virt` crate; if it drags in too much, ~150 lines of raw `libc` over the `uhid_event` struct). New VID/PID distinct from the reader's `0x1209/0x7A71`.
- Output reports -> `Engine`; Engine reports -> `UHID_INPUT2`. Keepalives while waiting for a tap.
- Apply 3b lifecycle rules (open count, idle eviction, channel cap).
- Missing `/dev/uhid` access: skip the device (log once), like mac does without its entitlement.
- Test: a unit test that drives the uhid backend over a mock `/dev/uhid` fd (socketpair) for INIT + CBOR GetInfo through the real `Engine`.

### 5. Packaging (1 day)
- `nix/package.nix`: Linux build (desktop binary, the `.so`, udev rules file, `.desktop`, icons). Add Linux to `packages`/`apps`/`checks` in `flake.nix`.
- `udev/70-tapbridge.rules`: `uhid` access via `TAG+="uaccess"`; the hidraw node for our VID/PID `TAG+="uaccess"` (browsers need it); the AOA phone PIDs for nusb.
- `nix/nixos-module.nix` (`services.tapbridge`): `enable`, `package`, `users` (list: gets reader.conf entries + group), `settings` (same options as the darwin module where they apply). Sets `services.pcscd.enable = true`, `services.pcscd.readerConfig` (one entry per user, `DEVICENAME` = that user's socket), `services.udev.packages`, a polkit rule allowing those users `org.debian.pcsc-lite.access_pcsc` (pcscd's polkit gate), and `systemd.user.services.tapbridge`.
- `nix/home-module.nix` (`programs.tapbridge`): `enable`, `package`, `settings`, `systemd.user.services.tapbridge` bound to `graphical-session.target`. Assertion/warning when the NixOS module isn't present on the host, since the reader needs system setup.
- Extend `options-doc.nix` / `splice-doc.py` so `docs/nixos.md` and `docs/home-manager.md` get generated tables, and the CI `docs` check covers them.

### 6. Verification + CI (half a day)
- NixOS VM test (`checks.x86_64-linux.vm`): pcscd + module enabled + a scripted fake phone -> `opensc-tool -r tapbridge --send-apdu ...` returns the scripted R-APDU; `fido2-token -L` lists the uhid key and `fido2-token -I` reaches GetInfo through to the fake card. This is the real end-to-end, not a mock layer.
- `build.yml`: add a Linux job: tests, clippy, `nix build .#tapbridge`, `nix flake check`.
- Gates run on real hardware (reported separately, not claimed): real phone over iroh, real phone over USB AOA, Firefox WebAuthn against a real site, pcscd with a real PIV card.

## Estimate
About 4 to 5 days total. Biggest uncertainty: step 1 (gpui/tray on Linux, reader.conf-only loading). If gpui doesn't build, add about 1 day for the headless-daemon fallback.

## Not in scope
Smart-card login on Linux (PAM), Wayland screenshot tooling, non-Nix distro packages (deb/rpm/AppImage). XDG autostart + udev rules file are provided so those are easy later.
