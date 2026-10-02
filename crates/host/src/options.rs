//! Command-line parsing without side effects or panics on user input.

use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run,
    Pair,
    Status,
    Forget,
    Help,
}

#[derive(Debug)]
pub struct Options {
    pub command: Command,
    pub state_dir: Option<PathBuf>,
    pub usbip: SocketAddr,
    pub stop_after: Option<u64>,
    pub trace_apdu: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let mut options = Options {
        command: Command::Run,
        state_dir: None,
        usbip: SocketAddr::from(([127, 0, 0, 1], 3240)),
        stop_after: None,
        trace_apdu: false,
    };
    let mut command_seen = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => options.command = Command::Help,
            "run" | "pair" | "status" | "forget" => {
                if command_seen {
                    return Err("choose one command".into());
                }
                command_seen = true;
                options.command = match arg.as_str() {
                    "pair" => Command::Pair,
                    "status" => Command::Status,
                    "forget" => Command::Forget,
                    _ => Command::Run,
                };
            }
            "--state-dir" => {
                options.state_dir = Some(PathBuf::from(
                    args.next().ok_or("--state-dir requires a path")?,
                ));
            }
            "--usbip" => {
                options.usbip = args
                    .next()
                    .ok_or("--usbip requires an address")?
                    .parse()
                    .map_err(|_| "invalid USB/IP address")?;
                if !options.usbip.ip().is_loopback() {
                    return Err("USB/IP must bind to loopback".into());
                }
            }
            "--trace-apdu" => options.trace_apdu = true,
            "--stop-after" => {
                let seconds = args
                    .next()
                    .ok_or("--stop-after requires seconds")?
                    .parse::<u64>()
                    .map_err(|_| "invalid stop duration")?;
                if !(1..=86400).contains(&seconds) {
                    return Err("stop duration must be 1..86400 seconds".into());
                }
                options.stop_after = Some(seconds);
            }
            _ => return Err("unknown command or option; use --help".into()),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn defaults_to_loopback_run() {
        let options = parse(args(&[])).unwrap();
        assert_eq!(options.command, Command::Run);
        assert_eq!(options.usbip, "127.0.0.1:3240".parse().unwrap());
        assert!(options.state_dir.is_none());
        assert!(options.stop_after.is_none());
        assert!(!options.trace_apdu);
    }

    #[test]
    fn trace_flag_is_opt_in() {
        assert!(parse(args(&["run", "--trace-apdu"])).unwrap().trace_apdu);
    }

    #[test]
    fn accepts_pair_with_explicit_isolated_state() {
        let options = parse(args(&[
            "pair",
            "--state-dir",
            "temporary-state",
            "--usbip",
            "127.0.0.1:0",
            "--stop-after",
            "2",
        ]))
        .unwrap();
        assert_eq!(options.command, Command::Pair);
        assert_eq!(options.state_dir, Some(PathBuf::from("temporary-state")));
        assert_eq!(options.usbip.port(), 0);
        assert_eq!(options.stop_after, Some(2));
    }
}
