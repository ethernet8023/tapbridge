# Spike: virtual FIDO HID security key on macOS (Option A)

Goal: tapbridge presents a virtual CTAPHID security key to the Mac; browsers
send CTAP2 to it; the daemon relays each request to the physical card on the
phone over the existing tunnel (CTAP over ISO-7816 NFC). Keys never leave the
card.

## Result so far (macOS 26.6.2, Xcode 27, SIP + AMFI on)

| Attempt | Result |
|---|---|
| `main.swift`: CoreHID `HIDVirtualDevice`, unsigned/ad-hoc, no entitlement | `init?(properties:)` returns **nil** |
| same, ad-hoc signed **with** `com.apple.developer.hid.virtual.device` | process **killed (SIGKILL)** at launch: restricted entitlement without a provisioning profile |
| `iokit.c`: `IOHIDUserDeviceCreateWithProperties`, no entitlement | returns **NULL**; the header says the same entitlement is required |

So no virtual HID device of any kind exists on this Mac without Apple's
`com.apple.developer.hid.virtual.device` (CoreHID "HID Virtual Device"
capability, requested per App ID on the developer portal).

Not yet answered: **whether Safari's WebAuthn accepts a virtual HID FIDO
device** (Chrome/Firefox read HID directly and very likely do).

## Ways to get a device to test with

1. **Request the capability** for App ID `computer.ethernet.tapbridge`
   (Certificates, Identifiers & Profiles → Identifiers → Capability Requests →
   "HID Virtual Device"). Then build with a Developer ID provisioning profile
   carrying it. Weeks, maybe; may be refused for this use.
2. **Local test without Apple**: disable AMFI on a test Mac
   (Recovery → `csrutil disable`, then
   `sudo nvram boot-args="amfi_get_out_of_my_way=1"`, reboot) so the ad-hoc
   entitled binary runs. Answers the Safari question in an afternoon. Do NOT
   ship or daily-drive this.
3. Karabiner's DriverKit virtual HID dext (already Apple-approved, carries
   the same entitlement): only exposes keyboard/pointing devices, not
   arbitrary report descriptors. Not usable for FIDO.

## Build

```sh
swiftc -parse-as-library -O -o fido-hid-spike main.swift
clang -framework IOKit -framework CoreFoundation -o iokit-spike iokit.c
codesign -f -s - --entitlements ent.plist fido-hid-spike   # AMFI-off machines only
```
