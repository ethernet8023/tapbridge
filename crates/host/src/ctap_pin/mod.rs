//! CTAP2 clientPIN proxy (see the module docs in `proxy`).

pub mod cbor;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod protocol;
