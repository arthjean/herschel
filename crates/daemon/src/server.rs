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

use kori_core::display::FRAME_INTERVAL_MS;
use kori_core::ipc::{IpcError, PROTOCOL_VERSION, Request, Response, read_frame, write_frame};

use crate::state::Daemon;

/// Longest the display ticker sleeps between two looks at the clock.
///
/// It is what makes shutdown prompt at a cadence of one frame per second, and
/// it is also the coarsest an animation frame can land: a delay of 100 ms is
/// hit within one of these of its due instant, since the sleep is recomputed
/// from the deadline on every pass.
const TICK_POLL: Duration = Duration::from_millis(50);

/// Shortest that same sleep is allowed to be.
///
/// A deadline already in the past would otherwise compute a zero sleep, and a
/// loop that takes the state mutex without ever yielding is a spin against
/// every request handler.
const TICK_FLOOR: Duration = Duration::from_millis(1);

/// A running server and the handle used to stop it.
pub struct Server {
    listener: UnixListener,
    daemon: Arc<Mutex<Daemon>>,
    shutdown: Arc<AtomicBool>,
    frame_interval: Duration,
}

/// Bind the socket with owner-only permissions.
///
/// The caller holds the single-instance lock, so a file already at `path` was
/// left behind by a dead daemon and is removed. Probing it with a connect
/// first would not make this safer: the probe and the bind cannot be made
/// atomic, and two starters racing that sequence can both unlink and both
/// bind, which leaves one of them accepting on an inode no client can reach
/// (`unix(7)`). The lock removes the race the probe only narrowed.
pub fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

impl Server {
    /// Serve `daemon` on a listener that is already bound and reachable.
    pub fn attach(listener: UnixListener, daemon: Daemon) -> Self {
        Self {
            listener,
            daemon: Arc::new(Mutex::new(daemon)),
            shutdown: Arc::new(AtomicBool::new(false)),
            frame_interval: Duration::from_millis(FRAME_INTERVAL_MS),
        }
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
    ///
    /// This one thread carries both cadences: the telemetry redraw at
    /// `frame_interval`, and an animation on the clock its own file declares.
    /// One thread rather than two because the writes to the panel are
    /// serialized by construction here, and a second thread would put that back
    /// on the mutex to enforce.
    fn spawn_display_ticker(&self) -> std::thread::JoinHandle<()> {
        let daemon = Arc::clone(&self.daemon);
        let shutdown = Arc::clone(&self.shutdown);
        let interval = self.frame_interval;
        std::thread::spawn(move || {
            let mut next = Instant::now() + interval;
            // Nothing is playing until a preset says so, so the loop starts on
            // the idle cadence and only shortens once an animation asks it to.
            let mut nap = interval.min(TICK_POLL).max(TICK_FLOOR);
            while !shutdown.load(Ordering::SeqCst) {
                std::thread::sleep(nap);
                let now = Instant::now();

                let mut animation_due = None;
                if let Ok(mut daemon) = daemon.lock() {
                    // The animation is asked first: it runs at up to a dozen
                    // frames a second against the telemetry redraw's one, so it
                    // is what the sleep below is sized against.
                    animation_due = daemon.tick_animation(now);

                    if now >= next {
                        let mut missed = 0u32;
                        while next + interval <= now {
                            next += interval;
                            missed += 1;
                        }
                        next += interval;

                        for _ in 0..missed {
                            daemon.drop_display_frame();
                        }
                        daemon.tick_display();
                    }
                }

                // Sleep to whichever comes first, but never past TICK_POLL, so
                // shutdown stays prompt even when nothing is due for a second.
                let wake = match animation_due {
                    Some(due) => due.min(next),
                    None => next,
                };
                nap = wake
                    .saturating_duration_since(Instant::now())
                    .clamp(TICK_FLOOR, TICK_POLL);
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
