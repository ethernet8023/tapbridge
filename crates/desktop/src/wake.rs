//! Wake client: POST /v1/wake to the relay. Tokens are never logged.
//!
//! Relay base URL: `<state_dir>/relay.txt` at runtime, else
//! `TAPBRIDGE_RELAY_URL` at compile time, else the project's hosted relay.
use crate::app_log;
use std::{path::Path, time::Duration};

pub enum WakeResult {
    /// 202: relay accepted the wake.
    Sent,
    /// 410: FCM no longer knows the token; the phone app must reconnect once
    /// to send a fresh one.
    Unregistered,
    /// 400 malformed, 429 rate limited, 502 FCM failure, anything else.
    Rejected(&'static str),
    Network(&'static str),
}

/// The hosted relay (relay/wrangler.toml custom domain).
const DEFAULT_RELAY: &str = "https://tapbridge-relay.ethernet.computer";

/// Relay base URL baked in at build time: `TAPBRIDGE_RELAY_URL` in the build
/// environment (cargo rebuilds when it changes), else [`DEFAULT_RELAY`], so
/// a build that forgets the variable still wakes the phone.
const BUILT_IN_RELAY: Option<&str> = match option_env!("TAPBRIDGE_RELAY_URL") {
    Some(url) => Some(url),
    None => Some(DEFAULT_RELAY),
};

/// Relay base URL: `<state_dir>/relay.txt` when present (self-hosted relay),
/// otherwise the built-in URL. `None` when neither is set.
pub fn relay_url(state_dir: &Path) -> Option<String> {
    let file = std::fs::read_to_string(state_dir.join("relay.txt")).ok();
    choose_relay(file.as_deref(), BUILT_IN_RELAY)
}

fn choose_relay(file: Option<&str>, built_in: Option<&str>) -> Option<String> {
    [file, built_in]
        .into_iter()
        .flatten()
        .map(|url| url.trim().trim_end_matches('/'))
        .find(|url| !url.is_empty())
        .map(str::to_owned)
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// POST `{token, host, hint}` to `<relay>/v1/wake`; runs on a caller thread.
pub fn send_wake(relay: &str, token: &str, host: &str, hint: &str) -> WakeResult {
    if token.len() > 4096 || host.len() > 128 || hint.len() > 1024 {
        return WakeResult::Rejected("wake payload out of contract bounds");
    }
    let body = format!(
        r#"{{"token":"{}","host":"{}","hint":"{}"}}"#,
        json_escape(token),
        json_escape(host),
        json_escape(hint)
    );
    let url = format!("{relay}/v1/wake");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        // Relay statuses are answers, not transport failures.
        .http_status_as_error(false)
        .build()
        .into();
    match agent
        .post(url)
        .header("content-type", "application/json")
        .send(body.as_bytes())
    {
        Ok(response) => match response.status().as_u16() {
            202 => WakeResult::Sent,
            410 => WakeResult::Unregistered,
            400 => WakeResult::Rejected("relay rejected the wake (malformed)"),
            429 => WakeResult::Rejected("wake rate limited (10/min)"),
            502 => WakeResult::Rejected("relay FCM failure"),
            status => {
                app_log!("wake: unexpected relay status {status}");
                WakeResult::Rejected("unexpected relay response")
            }
        },
        Err(error) => {
            app_log!("wake: relay request failed ({error})");
            WakeResult::Network("relay unreachable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_body_escaping() {
        assert_eq!(json_escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    /// One-shot loopback HTTP server answering `status`; returns its base URL
    /// and the request it received.
    fn relay_answering(status: u16) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            // Read headers, then the declared body.
            loop {
                let read = socket.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            write!(
                socket,
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, server)
    }

    #[test]
    fn maps_real_relay_statuses() {
        for (status, expected) in [
            (202, "sent"),
            (410, "unregistered"),
            (429, "rejected"),
            (502, "rejected"),
        ] {
            let (url, server) = relay_answering(status);
            let result = send_wake(&url, "tok", "host", "hint");
            let request = server.join().unwrap();
            assert!(request.starts_with("POST /v1/wake "), "{request}");
            assert!(request.ends_with(r#"{"token":"tok","host":"host","hint":"hint"}"#));
            let got = match result {
                WakeResult::Sent => "sent",
                WakeResult::Unregistered => "unregistered",
                WakeResult::Rejected(_) => "rejected",
                WakeResult::Network(_) => "network",
            };
            assert_eq!(got, expected, "status {status}");
        }
    }

    #[test]
    fn unreachable_relay_is_a_network_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(matches!(
            send_wake(&url, "tok", "host", "hint"),
            WakeResult::Network(_)
        ));
    }

    #[test]
    fn relay_file_overrides_built_in_and_blank_values_are_ignored() {
        let built_in = Some("https://relay.example/");
        assert_eq!(
            choose_relay(Some("https://mine.example/\n"), built_in).as_deref(),
            Some("https://mine.example")
        );
        assert_eq!(
            choose_relay(Some("  \n"), built_in).as_deref(),
            Some("https://relay.example")
        );
        assert_eq!(choose_relay(None, None), None);
    }

    #[test]
    fn builds_without_the_env_var_use_the_hosted_relay() {
        let dir = tempfile::tempdir().unwrap();
        let expected = option_env!("TAPBRIDGE_RELAY_URL").unwrap_or(DEFAULT_RELAY);
        assert_eq!(
            relay_url(dir.path()).as_deref(),
            Some(expected.trim_end_matches('/'))
        );
    }

    #[test]
    fn bounds_are_enforced() {
        let long = "x".repeat(4097);
        assert!(matches!(
            send_wake("https://relay.invalid", &long, "h", "hint"),
            WakeResult::Rejected(_)
        ));
    }
}
