//! Win32 string helper shared by the modules that call `*W` APIs.

/// NUL-terminated UTF-16 for `PCWSTR` parameters. Never pass a Rust `&str`
/// pointer to a `*W` function: it is UTF-8 and not NUL-terminated.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn wide_is_utf16_and_nul_terminated() {
        assert_eq!(super::wide("Aé"), vec![0x41, 0xe9, 0]);
    }
}
