# tapbridge-host reader — implementation notes (verified output)

## Scope
Ground-up rewrite of the USB/IP CCID reader module in `crates/host`,
owned files: `Cargo.toml`, `src/lib.rs`, `src/reader.rs`, `src/usb.rs`,
`src/usbip.rs`, `src/ccid.rs`, `tests/*.rs`. Parent owns
`src/{state,app,main}.rs` (state tests inline) plus root/Android.

## Public interface (final)
```rust
pub async fn serve(
    listener: tokio::net::TcpListener,
    from_phone: tokio::sync::mpsc::Receiver<tapbridge_core::wire::Frame>,
    to_phone: tokio::sync::mpsc::Sender<tapbridge_core::wire::Frame>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()>
```
Re-exported from `lib.rs` (`pub use reader::serve;`).

## Module layout
- `ccid.rs` — pure CCID 1.1 codec: bulk-OUT parse (strict dwLength
  validation, no trailing garbage), bulk-IN builders (DataBlock,
  SlotStatus, Parameters), 2-byte NotifySlotChange (present=bit0,
  changed=bit1), bStatus register (time extension = 0b10<<6 = 0x80, NOT
  0xC0), ATR validation per ISO 7816-3 (TS, TD-chain walk, TCK XOR,
  historical-length check). Never invents an ATR.
- `usb.rs` — pure descriptors: VID 0x1209 / PID 0x7A71, 24-byte CCID class
  descriptor (dwFeatures 0x000400FE short-APDU, dwMaxCCIDMessageLength
  271, 1 busy slot, T=1), endpoints 01/OUT bulk, 82/IN bulk, 83/IN
  interrupt; EP0 control dispatch incl. class ABORT (returns
  `ControlReply::Abort { slot, seq }`), GET_CLOCK_FREQUENCIES /
  GET_DATA_RATES.
- `usbip.rs` — pure USB/IP wire codec: OP_REQ_DEVLIST/IMPORT, REP_DEVLIST
  (path 256 + busid 32 fields), REP_IMPORT (status + devid + device
  descriptor), CMD_SUBMIT/CMD_UNLINK headers, RET_SUBMIT (devid/direction/ep
  left 0, number_of_packets 0 — usbip-win2 quirk), RET_UNLINK with
  ECONNRESET (-104).
- `reader.rs` — the one stateful event loop:
  - `serve` owns the phone receiver and forwards frames to the single
    imported session through a registered sender (spawned connections, so
    concurrent connects never block USB enumeration).
  - One imported client (per-serve `Arc<AtomicBool>`; second import gets
    status != 0).
  - Dedicated cancellation-safe packet reader task (BufReader over the read
    half). SUBMIT vs UNLINK disambiguated by devid == EXPORTED_DEVID
    (0x00010001) at offset 4; each iteration reads exactly one complete
    command; any error kills the session (never restarts mid-packet).
  - All reader state in one `Session` owned by the select loop: card state,
    single in-flight APDU (`Option<InFlight>`, bounded wait-map of size 1),
    parked interrupt-IN URBs (max 4) + queued notifies (max 4), abort_armed.
  - One APDU in flight: second XfrBlock -> CCID CMD_SLOT_BUSY (0xE0).
  - Monotonic u64 APDU ids across imports (process-wide AtomicU64).
  - Phone Card semantics: EMPTY ATR = absent even if generation is lower
    than a previous present generation; absence/replacement completes a
    pending XfrBlock with error block 0x42/ICC_MUTE and clears in-flight;
    notify edge only on presence change.
  - Stale response generations/ids never satisfy an XfrBlock.
  - APDU deadline (2 s): CCID time extension (bStatus 0x80, bError=BWT
    multiplier) up to 10 extensions, then hard failure (0x42/ICC_MUTE).
  - Two-phase CCID abort: control ABORT arms (slot, seq); bulk
    PC_to_RDR_Abort with matching slot/seq cancels the pending XfrBlock
    (CMD_ABORTED to its URB, Cancel frame to phone) and replies SlotStatus
    with the abort's own seq. Distinct from USB UNLINK: unlink of a parked
    interrupt-IN URB returns RET_UNLINK -ECONNRESET; unlinking an unknown
    seqnum returns 0; the phone is not involved.
  - No silent bulk-IN tail loss: each reply carries the complete CCID
    message as the URB payload (a duplicate-payload framing bug was caught
    by the interrupt-IN test during TDD).
  - Loopback-only listener.

## TDD process
Each slice written RED first (confirmed unresolved-symbol/compile failures
with real cargo output), then implemented GREEN:
1. ccid codec — RED: 10 unresolved errors; GREEN 9/9.
2. usb descriptors — RED: 27 errors; GREEN 5/5.
3. usbip codec — RED: compile + 7 failures; GREEN 11/11.
4. reader/serve — RED: unresolved `serve`; GREEN 15/15 after fixing
   (a) missing 8-byte setup in ep0 submit reads, (b) duplicate payload in
   RET_SUBMIT framing, (c) phone-frame fan-out so a second connection is
   accepted while one import is live, (d) unlink header layout.

## Verified output (final)
```
cargo test -p tapbridge-host -- --test-threads=4
  ccid:    9 passed
  state:   9 passed   (parent-owned inline tests, not modified by me)
  reader: 15 passed
  usb:     5 passed
  usbip:  11 passed
cargo clippy -p tapbridge-host --all-targets   -> 0 warnings
cargo fmt -p tapbridge-host                    -> clean
```

## Build environment (Windows ARM64)
`crates/host/build-env.sh`: ring/blake3 need clang on
aarch64-pc-windows-msvc (`CC_aarch64_pc_windows_msvc=C:\Program
Files\LLVM\bin\clang.exe`, LLVM on PATH, MSVC 14.44 + SDK 10.0.26100
INCLUDE/LIB). Source before cargo. Target-scoped, no machine paths inside
crate sources.

## Dependencies added by me (per parent request)
`iroh.workspace = true`, `tempfile = "3"`, windows-sys 0.61 (cfg windows,
DPAPI features), `qrcode` still pending — parent asked for
`qrcode = { version = "0.14.1", default-features = false, features = ["svg"] }`;
not yet added because cargo info for the feature set was not confirmed
before this handoff. Parent should add to `crates/host/Cargo.toml`
[dependencies] if still needed.

## Notes for parent integration
- `serve` is integrated by main.rs; it never touches iroh (wire frames only).
- The reader ignores `Frame::Hello/Accepted/Rejected/ReadCard` (session
  owner's job) and acts on Card/Apdu-direction/Response/Failed/Cancel.
- Time-extension period is 2 s with max 10 extensions — tune if Windows
  usbccid needs different pacing.
- `interrupt IN` wMaxPacketSize is 8 (2-byte notify fits).
- No payload/APDU/ATR bytes are logged anywhere.
