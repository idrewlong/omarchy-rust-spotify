//! The client socket: NDJSON over a Unix stream socket, owner-only. Clients
//! `sub` to topics and get a `snap` then `ev` deltas; `cmd`s go straight to
//! librespot. See docs/PLAN.md §1.4.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use skinamp_proto::{ClientMsg, Command, PROTO_VERSION, PlayerState, ServerMsg};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

use crate::library::Library;
use crate::viz::Tap;

/// The visualizer: the tap to switch on while watched, and its frames.
#[derive(Clone)]
pub struct VizFeed {
    pub tap: Arc<Tap>,
    pub frames: broadcast::Sender<Arc<ServerMsg>>,
}
use crate::state::Update;

/// Bind the socket (0700 directory, 0600 socket). Done before anything else
/// so the daemon only reports ready once clients can connect.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("IPC: {}", path.display());
    Ok(listener)
}

pub async fn run(
    listener: UnixListener,
    state: watch::Receiver<(u64, PlayerState)>,
    updates: broadcast::Sender<Arc<Update>>,
    cmds: mpsc::UnboundedSender<Command>,
    library: Arc<Library>,
    viz: VizFeed,
) -> anyhow::Result<()> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    loop {
        let (stream, _) = listener.accept().await?;
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {}
            _ => {
                tracing::warn!("rejected IPC peer from another user");
                continue;
            }
        }
        let (state, updates, cmds, library, viz) = (
            state.clone(),
            updates.clone(),
            cmds.clone(),
            library.clone(),
            viz.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = serve(stream, state, updates, cmds, library, viz).await {
                tracing::debug!("IPC client ended: {e:#}");
            }
        });
    }
}

async fn serve(
    stream: UnixStream,
    state: watch::Receiver<(u64, PlayerState)>,
    updates: broadcast::Sender<Arc<Update>>,
    cmds: mpsc::UnboundedSender<Command>,
    library: Arc<Library>,
    viz: VizFeed,
) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();

    // One writer task per client, so a slow client never blocks the daemon.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let mut line = serde_json::to_vec(&msg)?;
            line.push(b'\n');
            write.write_all(&line).await?;
        }
        anyhow::Ok(())
    });

    let seq = state.borrow().0;
    out_tx.send(ServerMsg::Hello {
        proto: PROTO_VERSION,
        daemon: env!("CARGO_PKG_VERSION").into(),
        caps: vec![],
        seq,
    })?;

    let mut lines = BufReader::new(read).lines();
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut viz_task: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(line) = lines.next_line().await? {
        let msg: ClientMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                out_tx.send(ServerMsg::Err {
                    id: 0,
                    code: "bad_request".into(),
                    message: e.to_string(),
                })?;
                continue;
            }
        };
        match msg {
            ClientMsg::Sub { id, topics } => {
                // Each sub states the full set of topics: start or stop
                // the visualizer feed to match.
                let wants_viz = topics.iter().any(|t| t == "viz");
                if wants_viz && viz_task.is_none() {
                    viz_task = Some(tokio::spawn(forward_viz(
                        viz.frames.subscribe(),
                        viz.tap.watch(),
                        out_tx.clone(),
                    )));
                } else if !wants_viz && let Some(t) = viz_task.take() {
                    t.abort();
                }
                if !topics.iter().any(|t| t == "player") {
                    out_tx.send(ServerMsg::Ok { id })?;
                    continue;
                }
                // Subscribe before reading the snapshot, then drop anything
                // the snapshot already covers: no gap, no duplicate.
                let rx = updates.subscribe();
                let (snap_seq, snap) = state.borrow().clone();
                out_tx.send(ServerMsg::Snap {
                    topic: "player".into(),
                    seq: snap_seq,
                    state: snap,
                })?;
                out_tx.send(ServerMsg::Ok { id })?;
                if let Some(f) = forwarder.take() {
                    f.abort();
                }
                forwarder = Some(tokio::spawn(forward(
                    rx,
                    snap_seq,
                    state.clone(),
                    out_tx.clone(),
                )));
            }
            // Answered on their own task: a slow Web API call must not hold
            // up commands on this connection.
            ClientMsg::Req { id, req } => {
                let (library, out) = (library.clone(), out_tx.clone());
                tokio::spawn(async move {
                    let _ = out.send(library.answer(id, req).await);
                });
            }
            ClientMsg::Cmd { id, cmd } => {
                let reply = match cmds.send(cmd) {
                    Ok(()) => ServerMsg::Ok { id },
                    Err(_) => ServerMsg::Err {
                        id,
                        code: "shutting_down".into(),
                        message: String::new(),
                    },
                };
                out_tx.send(reply)?;
            }
        }
    }
    if let Some(f) = forwarder {
        f.abort();
    }
    if let Some(t) = viz_task {
        t.abort();
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

async fn forward(
    mut rx: broadcast::Receiver<Arc<Update>>,
    mut after_seq: u64,
    state: watch::Receiver<(u64, PlayerState)>,
    out: mpsc::UnboundedSender<ServerMsg>,
) {
    loop {
        match rx.recv().await {
            Ok(u) if u.seq <= after_seq => {}
            Ok(u) => {
                after_seq = u.seq;
                let msg = ServerMsg::Ev {
                    topic: "player".into(),
                    seq: u.seq,
                    mono_ns: u.mono_ns,
                    delta: u.delta.clone(),
                };
                if out.send(msg).is_err() {
                    return;
                }
            }
            // Fell behind: resend the whole state rather than a gappy stream.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let (seq, snap) = state.borrow().clone();
                after_seq = seq;
                if out
                    .send(ServerMsg::Snap {
                        topic: "player".into(),
                        seq,
                        state: snap,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Spectrum frames to one client. Holding `_watch` keeps the tap on; the
/// task is aborted (dropping it) when the client unsubscribes or leaves.
async fn forward_viz(
    mut rx: broadcast::Receiver<Arc<ServerMsg>>,
    _watch: crate::viz::Watch,
    out: mpsc::UnboundedSender<ServerMsg>,
) {
    loop {
        match rx.recv().await {
            Ok(frame) => {
                if out.send((*frame).clone()).is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
