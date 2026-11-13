//! neko-webshell: raw WebSocket <-> PTY bridge for the client's xterm.js
//! terminal tab. Rust replacement for the former webshell.py — same wire
//! protocol, same behavior:
//!
//! - each WebSocket connection gets its own login shell (`bash -l`) running
//!   as the container user (supervisord starts this program with user=neko)
//! - either direction: binary WebSocket message = raw terminal bytes
//! - client -> server: JSON text {"type": "resize", "cols": N, "rows": M}
//! - ws path is ignored (the client connects to /ws)

use std::ffi::CString;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use nix::pty::openpty;
use nix::sys::signal::SIGHUP;
use nix::unistd::{ForkResult, execvpe, fork};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

const SHELL: &str = "/bin/bash";
const READ_BUF: usize = 65536;

/// Everything the forked child needs, allocated BEFORE fork — after fork
/// only raw syscalls run until exec (no allocation, no locks, no unwind).
struct ChildPlan {
    argv: Vec<CString>,
    env: Vec<CString>,
}

fn prepare_child(shell: &str) -> Result<ChildPlan> {
    let shell = CString::new(shell).context("shell cstring")?;
    let argv = vec![shell, CString::new("-l").unwrap()];

    // env: inherit everything, force a decent terminal
    let mut env: Vec<CString> = std::env::vars_os()
        .map(|(k, v)| CString::new(format!("{}={}", k.to_string_lossy(), v.to_string_lossy())))
        .collect::<std::result::Result<_, _>>()
        .context("env cstring")?;
    env.push(CString::new("TERM=xterm-256color").unwrap());
    env.push(CString::new("COLORTERM=truecolor").unwrap());

    Ok(ChildPlan { argv, env })
}

/// Child side of the fork: new session, slave as controlling terminal,
/// then exec the login shell. Never returns.
fn child_exec(slave: &OwnedFd, plan: &ChildPlan) -> ! {
    unsafe {
        if libc::setsid() == -1 {
            libc::_exit(1);
        }
        let s = slave.as_raw_fd();
        if libc::dup2(s, 0) == -1 || libc::dup2(s, 1) == -1 || libc::dup2(s, 2) == -1 {
            libc::_exit(1);
        }
        // make the slave our controlling terminal
        if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
            libc::_exit(1);
        }
    }

    // only returns on exec failure
    let _ = execvpe(&plan.argv[0], &plan.argv, &plan.env);
    unsafe { libc::_exit(127) }
}

#[derive(Debug)]
struct Args {
    host: String,
    port: u16,
    shell: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        host: "0.0.0.0".to_owned(),
        port: 7681,
        shell: SHELL.to_owned(),
    };

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => args.host = it.next().unwrap_or(args.host),
            "--port" => {
                args.port = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(args.port)
            }
            "--shell" => args.shell = it.next().unwrap_or(args.shell),
            other => {
                if let Some(v) = other.strip_prefix("--port=") {
                    args.port = v.parse().unwrap_or(args.port);
                } else if let Some(v) = other.strip_prefix("--host=") {
                    args.host = v.to_owned();
                } else if let Some(v) = other.strip_prefix("--shell=") {
                    args.shell = v.to_owned();
                }
            }
        }
    }
    args
}

fn set_winsize(master: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: master is a valid fd, ws is a plain struct
    if unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &ws) } == -1 {
        warn!("TIOCSWINSZ failed");
    }
}

/// Handle one accepted WebSocket connection: fork a shell, pump bytes.
async fn handle_connection(stream: tokio::net::TcpStream, peer: SocketAddr, shell: std::sync::Arc<str>) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut ws_sink, mut ws_stream) = ws.split();

    let pty = match openpty(None, None) {
        Ok(p) => p,
        Err(err) => {
            error!(?err, "openpty failed");
            return;
        }
    };
    let master_raw = pty.master.as_raw_fd();

    let plan = match prepare_child(&shell) {
        Ok(p) => p,
        Err(err) => {
            error!(?err, "prepare child failed");
            return;
        }
    };

    // reaper: one blocking waitpid per connection, off the async runtime
    let pid = match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            // child: never returns
            child_exec(&pty.slave, &plan)
        }
        Ok(ForkResult::Parent { child }) => {
            std::thread::spawn(move || loop {
                match nix::sys::wait::waitpid(child, None) {
                    Ok(nix::sys::wait::WaitStatus::Exited(_, _)) => break,
                    Ok(_) => continue, // e.g. stopped/continued: keep waiting
                    Err(_) => break,
                }
            });
            child
        }
        Err(err) => {
            error!(?err, "fork failed");
            return;
        }
    };

    // PTY master -> ws: blocking reads on a plain thread (EIO when the
    // child exits and is reaped, mirroring the python implementation)
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let mut reader_master = std::fs::File::from(pty.master.try_clone().expect("dup master fd"));
    std::thread::spawn(move || {
        let mut buf = [0u8; READ_BUF];
        loop {
            match reader_master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // ws side gone
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break, // EIO: child exited
            }
        }
    });

    // ws -> PTY master writes via an owned duplicate of the fd
    let mut writer_master = std::fs::File::from(pty.master.try_clone().expect("dup master fd"));

    info!(?peer, ?pid, "webshell session started");

    'pump: loop {
        tokio::select! {
            // PTY -> ws
            Some(data) = rx.recv() => {
                if ws_sink.send(Message::Binary(data.into())).await.is_err() {
                    break 'pump;
                }
            }
            // ws -> PTY
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        if writer_master.write_all(&data).is_err() {
                            break 'pump;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Some((cols, rows)) = parse_resize(&text) {
                            set_winsize(master_raw, cols, rows);
                        }
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => {
                        let _ = ws_sink.send(Message::Close(None)).await;
                        break 'pump;
                    }
                    // null bytes and anything unexpected: ignore like python did
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break 'pump,
                    None => break 'pump,
                }
            }
        }
    }

    // shell still running (client went away): hang it up; master closes on drop
    let _ = nix::sys::signal::kill(pid, SIGHUP);
    drop(writer_master);
    drop(pty.master);
    info!(?peer, ?pid, "webshell session ended");
}

/// Minimal JSON resize message: {"type":"resize","cols":N,"rows":M}
fn parse_resize(text: &str) -> Option<(u16, u16)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "resize" {
        return None;
    }
    let cols = value.get("cols")?.as_u64()? as u16;
    let rows = value.get("rows")?.as_u64()? as u16;
    Some((cols, rows))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = parse_args();
    let shell: std::sync::Arc<str> = std::sync::Arc::from(args.shell.as_str());

    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", args.host, args.port))?;
    let listener = TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;

    info!(%addr, shell = %args.shell, "[webshell] listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        let shell = shell.clone();
        tokio::spawn(async move {
            handle_connection(stream, peer, shell).await;
        });
    }
}
