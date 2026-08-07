// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The local Unix socket server.
//!
//! One listener, owner-only permissions, no TCP or UDP endpoint anywhere. Each
//! connection is authenticated against the peer's credentials, must complete a
//! version handshake, and is served under the state mutex so commands cannot
//! interleave.

use std::io::{BufReader, BufWriter};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nzxt_core::display::FRAME_INTERVAL_MS;
use nzxt_core::ipc::{IpcError, PROTOCOL_VERSION, Request, Response, read_frame, write_frame};

use crate::state::Daemon;

/// A running server and the handle used to stop it.
pub struct Server {
    listener: UnixListener,
    daemon: Arc<Mutex<Daemon>>,
    shutdown: Arc<AtomicBool>,
    frame_interval: Duration,
}

impl Server {
    /// Bind the socket with owner-only permissions.
    ///
    /// A socket left behind by a dead daemon is removed first. A socket owned
    /// by a live daemon cannot be taken over, because that daemon still holds
    /// the per-device locks and this one would have failed to acquire them.
    pub fn bind(path: &Path, daemon: Daemon) -> std::io::Result<Self> {
        if path.exists() {
            match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("another daemon is already listening on {}", path.display()),
                    ));
                }
                Err(_) => std::fs::remove_file(path)?,
            }
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

        Ok(Self {
            listener,
            daemon: Arc::new(Mutex::new(daemon)),
            shutdown: Arc::new(AtomicBool::new(false)),
            frame_interval: Duration::from_millis(FRAME_INTERVAL_MS),
        })
    }

    /// Redraw the panel at `interval` instead of once a second.
    ///
    /// Only the cadence changes. A test exercises the same streaming code
    /// without spending a real second per frame.
    pub fn with_frame_interval(mut self, interval: Duration) -> Self {
        self.frame_interval = interval;
        self
    }

    pub fn daemon(&self) -> Arc<Mutex<Daemon>> {
        Arc::clone(&self.daemon)
    }

    /// A handle that stops the accept loop.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            flag: Arc::clone(&self.shutdown),
            socket: self.local_path(),
        }
    }

    fn local_path(&self) -> Option<std::path::PathBuf> {
        self.listener
            .local_addr()
            .ok()
            .and_then(|addr| addr.as_pathname().map(Path::to_path_buf))
    }

    /// Redraw the panel forever, on its own cadence.
    ///
    /// The panel belongs to the daemon, not to a client: a preset keeps its
    /// readings current with the window closed, which is the whole reason the
    /// rendering lives on this side of the socket.
    ///
    /// A tick that runs late does not catch up. The missed frames are counted
    /// and discarded, because the next tick draws the current reading and a
    /// backlog of old ones has nothing to offer a panel.
    fn spawn_display_ticker(&self) -> std::thread::JoinHandle<()> {
        let daemon = Arc::clone(&self.daemon);
        let shutdown = Arc::clone(&self.shutdown);
        let interval = self.frame_interval;
        std::thread::spawn(move || {
            let mut next = Instant::now() + interval;
            while !shutdown.load(Ordering::SeqCst) {
                // Woken often enough that shutdown is prompt even at one frame
                // per second.
                std::thread::sleep(interval.min(Duration::from_millis(50)));
                let now = Instant::now();
                if now < next {
                    continue;
                }

                let mut missed = 0u32;
                while next + interval <= now {
                    next += interval;
                    missed += 1;
                }
                next += interval;

                if let Ok(mut daemon) = daemon.lock() {
                    for _ in 0..missed {
                        daemon.drop_display_frame();
                    }
                    daemon.tick_display();
                }
            }
        })
    }

    /// Serve connections until the shutdown handle fires.
    pub fn run(self) {
        let ticker = self.spawn_display_ticker();

        for stream in self.listener.incoming() {
            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };

            let daemon = Arc::clone(&self.daemon);
            // One thread per client. The state mutex, not the thread count,
            // is what serializes commands.
            std::thread::spawn(move || serve(stream, daemon));
        }

        // The ticker holds the same lock the request handlers do, so it is
        // joined before the socket is removed: a frame half written while the
        // daemon is being torn down is exactly the state this product refuses
        // to leave the hardware in.
        let _ = ticker.join();

        if let Some(path) = self.local_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Stops a running [`Server`].
pub struct ShutdownHandle {
    flag: Arc<AtomicBool>,
    socket: Option<std::path::PathBuf>,
}

impl ShutdownHandle {
    /// Set the flag, then poke the listener so `accept` returns.
    pub fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(path) = &self.socket {
            let _ = UnixStream::connect(path);
        }
    }
}

/// Serve one connection.
fn serve(stream: UnixStream, daemon: Arc<Mutex<Daemon>>) {
    let peer = match peer_credentials(&stream) {
        Ok(peer) => peer,
        Err(reason) => {
            reject(&stream, IpcError::PeerRejected { reason }, &daemon);
            return;
        }
    };

    // The socket lives in a directory only this user can enter, and the
    // credential check makes that a guarantee rather than a side effect.
    let own_uid = rustix::process::getuid().as_raw();
    if peer.uid != own_uid {
        let reason = format!("peer uid {} does not own this daemon", peer.uid);
        if let Ok(mut daemon) = daemon.lock() {
            daemon.record_client_rejected(reason.clone());
        }
        reject(&stream, IpcError::PeerRejected { reason }, &daemon);
        return;
    }

    if let Ok(mut daemon) = daemon.lock() {
        daemon.record_client_accepted(peer.uid, peer.pid);
    }

    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut writer = BufWriter::new(write_half);
    let mut handshake_done = false;

    loop {
        let request: Request = match read_frame(&mut reader) {
            Ok(request) => request,
            Err(error) => {
                // A framing failure leaves the stream unusable: answer once
                // with the typed reason, then close.
                if let Some(ipc_error) = error.as_ipc_error() {
                    if let Ok(mut daemon) = daemon.lock() {
                        daemon.record_client_disconnected(error.to_string());
                    }
                    let _ = write_frame(&mut writer, &Response::Error(ipc_error));
                } else if let Ok(mut daemon) = daemon.lock() {
                    daemon.record_client_disconnected("client closed the connection".to_string());
                }
                return;
            }
        };

        // Nothing but the handshake is served before the handshake.
        if !handshake_done {
            match &request {
                Request::Hello { protocol_version } if *protocol_version == PROTOCOL_VERSION => {
                    handshake_done = true;
                }
                Request::Hello { protocol_version } => {
                    let _ = write_frame(
                        &mut writer,
                        &Response::Error(IpcError::UnsupportedProtocol {
                            requested: *protocol_version,
                            supported: PROTOCOL_VERSION,
                        }),
                    );
                    return;
                }
                _ => {
                    let _ = write_frame(&mut writer, &Response::Error(IpcError::HandshakeRequired));
                    return;
                }
            }
        }

        let response = match daemon.lock() {
            Ok(mut daemon) => daemon.handle(request),
            Err(_) => Response::Error(IpcError::Io {
                detail: "daemon state is unavailable".to_string(),
            }),
        };

        if write_frame(&mut writer, &response).is_err() {
            return;
        }
    }
}

struct Peer {
    uid: u32,
    pid: i32,
}

fn peer_credentials(stream: &UnixStream) -> Result<Peer, String> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)
        .map_err(|error| format!("peer credentials are unavailable: {error}"))?;
    Ok(Peer {
        uid: credentials.uid.as_raw(),
        pid: credentials.pid.as_raw_nonzero().get(),
    })
}

fn reject(stream: &UnixStream, error: IpcError, daemon: &Arc<Mutex<Daemon>>) {
    if let Ok(mut daemon) = daemon.lock() {
        daemon.record_client_rejected(error.to_string());
    }
    if let Ok(write_half) = stream.try_clone() {
        let mut writer = BufWriter::new(write_half);
        let _ = write_frame(&mut writer, &Response::Error(error));
    }
}
