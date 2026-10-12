# nusb snapshot

Source: crates.io `nusb` 0.2.7 (https://github.com/kevinmehall/nusb), license
Apache-2.0 OR MIT (`LICENSE-APACHE`, `LICENSE-MIT`).

One change: `Cargo.toml` pins `core-foundation = "=0.10.0"` (upstream asks for
`^0.10.1`) because the vendored GPUI-CE pins `=0.10.0` and Cargo cannot
resolve both, and adds `[lints.rust] dead_code = "allow"` (one unused upstream
helper warned in every build). nusb builds and works against 0.10.0 unchanged. Also dropped:
`examples/`, `.github/`, `.cargo/`, `Cargo.lock` (not needed to build).

Drop this copy and depend on crates.io `nusb` once GPUI-CE moves to
`core-foundation` 0.10.1 or later.
