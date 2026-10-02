mod app;
mod invitation;
mod options;
mod state;
mod trace;

use options::Command;
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};
use tapbridge_core::{pairing::PairingGate, transport};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("tapbridge: {error}");
        std::process::exit(1);
    }
}

async fn run() -> io::Result<()> {
    let options = options::parse(std::env::args().skip(1)).map_err(io::Error::other)?;
    if options.command == Command::Help {
        println!(
            "tapbridge-host [run|pair|status|forget] [--state-dir PATH] [--usbip 127.0.0.1:3240] [--stop-after SECONDS] [--trace-apdu]\n\n  pair    Create a private QR invitation and run the reader.\n  run     Accept only the paired phone.\n  status  Show host identity and pairing status.\n  --trace-apdu  Print payload-free APDU summaries (INS, CTAP command, status) to stderr.
  forget  Revoke the paired phone; host identity is retained.\n\nUSB/IP is loopback-only. Install usbip-win2 separately, then attach busid 1-1.\nAndroid reader mode requires opening the app; background notification does not bypass the lockscreen."
        );
        return Ok(());
    }
    let directory = match options.state_dir {
        Some(path) => path,
        None => default_state_dir()?,
    };
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("host.lock"))?;
    lock.try_lock()
        .map_err(|_| io::Error::other("another host owns this state directory; stop it first"))?;
    let mut store = state::Store::open(&directory)?;
    if options.command == Command::Status {
        println!(
            "host: {}\npaired: {}",
            store.key.public(),
            store
                .peer
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".into())
        );
        return Ok(());
    }
    if options.command == Command::Forget {
        store.set_peer(None)?;
        println!("phone revoked; run tapbridge-host pair to pair again");
        return Ok(());
    }
    if options.command == Command::Pair && store.peer.is_some() {
        return Err(io::Error::other(
            "a phone is already paired; use forget explicitly before re-pairing",
        ));
    }
    if options.command == Command::Run && store.peer.is_none() {
        return Err(io::Error::other("no phone paired; use tapbridge-host pair"));
    }
    let listener = TcpListener::bind(options.usbip).await?;
    let endpoint = transport::production_builder(&store.key)
        .bind()
        .await
        .map_err(io::Error::other)?;
    println!(
        "host: {}\nUSB/IP: {} (busid 1-1)",
        endpoint.id(),
        listener.local_addr()?
    );
    let mut gate = PairingGate::new();
    let mut pairing_page = None;
    if options.command == Command::Pair {
        let invitation = gate.mint(endpoint.id());
        let mut file = tempfile::Builder::new()
            .prefix("pair-")
            .suffix(".html")
            .tempfile_in(&directory)?;
        file.write_all(invitation::page(&invitation)?.as_bytes())?;
        file.flush()?;
        println!("open pairing page: {}", file.path().display());
        pairing_page = Some(file);
    }
    let (to_phone, from_reader) = mpsc::channel(8);
    let (to_reader, from_phone) = mpsc::channel(8);
    let (stop, shutdown) = watch::channel(false);
    let mut reader = tokio::spawn(tapbridge_host::serve(
        listener,
        from_phone,
        to_phone,
        shutdown.clone(),
    ));
    let mut phone = tokio::spawn(app::serve_phone(
        endpoint,
        store,
        gate,
        from_reader,
        to_reader,
        shutdown,
        options.trace_apdu,
    ));
    let deadline = async {
        match options.stop_after {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending().await,
        }
    };
    let result = tokio::select! {
        result = &mut reader => result.map_err(io::Error::other).and_then(|r| r),
        result = &mut phone => result.map_err(io::Error::other).and_then(|r| r),
        result = tokio::signal::ctrl_c() => result,
        _ = deadline => Ok(()),
    };
    let _ = stop.send(true);
    for task in [&mut reader, &mut phone] {
        if !task.is_finished()
            && tokio::time::timeout(Duration::from_secs(5), &mut *task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    drop(pairing_page);
    result
}

fn default_state_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")));
    root.map(|path| path.join("tapbridge")).ok_or_else(|| {
        io::Error::other(
            "set --state-dir explicitly; platform application directory is unavailable",
        )
    })
}
