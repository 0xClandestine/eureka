//! HTTP server for real-time session observability.
//!
//! Exposes three endpoints on `http://127.0.0.1:{port}`:
//! - `GET /api/graph`  — static graph specification (JSON)
//! - `GET /api/state`  — live run state (rounds, active nodes, elapsed)
//! - `GET /api/events` — SSE stream of `SchedulerEvent` objects

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::get,
    Router,
};
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
use tower_http::cors::CorsLayer;

use eureka::graph::GraphSpec;
use eureka::run::{RunRecord, RunStore};
use eureka::scheduler::SchedulerEvent;

/// Shared state injected into every axum handler.
#[derive(Clone)]
pub struct ServerState {
    /// The static graph specification.
    spec: Arc<GraphSpec>,
    /// Broadcast sender — handlers subscribe to get their own receiver.
    event_tx: broadcast::Sender<SchedulerEvent>,
    /// Mutable live run state.
    live: Arc<Mutex<LiveState>>,
    /// Wall-clock start time for computing elapsed seconds.
    started_at: Instant,
    /// Optional durable run-record store.
    run_store: Option<Arc<dyn RunStore>>,
    /// Run ID used by the durable status endpoint.
    run_id: Option<uuid::Uuid>,
}

/// Live run state snapshot served at `GET /api/state`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LiveState {
    /// Research goal string.
    pub goal: String,
    /// Scheduler rounds completed so far.
    pub rounds: u32,
    /// Wall-clock seconds elapsed (computed dynamically by the handler).
    pub elapsed_secs: f64,
    /// Node IDs whose activations are currently in flight.
    pub active_nodes: HashSet<String>,
    /// Whether the session has finished.
    pub finished: bool,
    /// Latest outputs per node: `node_id` → `[{ port, kind, data }]`
    pub node_outputs: std::collections::HashMap<String, Vec<serde_json::Value>>,
}

/// Spawn a background task that subscribes to the broadcast channel
/// and keeps `live` up-to-date from incoming scheduler events.
///
/// The returned handle must be kept alive for the duration of the run.
pub fn track_live_state(
    mut rx: broadcast::Receiver<SchedulerEvent>,
    live: Arc<Mutex<LiveState>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let mut s = live.lock().await;
                    match &event {
                        SchedulerEvent::ActivationStarted { node_id, .. } => {
                            s.active_nodes.insert(node_id.clone());
                        }
                        SchedulerEvent::ActivationCompleted {
                            node_id, outputs, ..
                        } => {
                            s.active_nodes.remove(node_id.as_str());
                            if !outputs.is_empty() {
                                s.node_outputs.insert(node_id.clone(), outputs.clone());
                            }
                        }
                        SchedulerEvent::ActivationFailed { node_id, .. } => {
                            s.active_nodes.remove(node_id.as_str());
                        }
                        SchedulerEvent::CycleCompleted { round } => {
                            s.rounds = *round;
                        }
                        SchedulerEvent::RunHalted { total_rounds, .. } => {
                            s.rounds = *total_rounds;
                            s.finished = true;
                        }
                        SchedulerEvent::RunPaused { round } => {
                            s.rounds = *round;
                            s.finished = false;
                        }
                        SchedulerEvent::ToolCalled { .. } => {}
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "LiveState tracker lagged — some events lost");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Start the axum server on `127.0.0.1:{port}` and return its task handle.
///
/// Returns immediately; the server runs in a background task. If the port
/// is already in use the error is logged and the task exits silently.
pub fn start_server(
    spec: GraphSpec,
    event_tx: broadcast::Sender<SchedulerEvent>,
    live: Arc<Mutex<LiveState>>,
    port: u16,
) -> tokio::task::JoinHandle<()> {
    start_server_with_run_store(spec, event_tx, live, port, None, None)
}

/// Start the UI server with an optional durable run store.
///
/// When `run_store` and `run_id` are supplied, `GET /api/run` exposes the
/// persisted lifecycle record for the run. This is the small integration seam
/// application servers can use for polling status after a request returns.
pub fn start_server_with_run_store(
    spec: GraphSpec,
    event_tx: broadcast::Sender<SchedulerEvent>,
    live: Arc<Mutex<LiveState>>,
    port: u16,
    run_store: Option<Arc<dyn RunStore>>,
    run_id: Option<uuid::Uuid>,
) -> tokio::task::JoinHandle<()> {
    let state = ServerState {
        spec: Arc::new(spec),
        event_tx,
        live,
        started_at: Instant::now(),
        run_store,
        run_id,
    };

    let app = Router::new()
        .route("/api/graph", get(graph_handler))
        .route("/api/state", get(state_handler))
        .route("/api/run", get(run_handler))
        .route("/api/events", get(events_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tokio::spawn(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, port, "Failed to bind UI server — is the port in use?");
                return;
            }
        };
        tracing::info!(port, "Eureka UI server → http://127.0.0.1:{port}");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "UI server exited with error");
        }
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/graph` — return the static graph specification.
async fn graph_handler(State(s): State<ServerState>) -> Json<serde_json::Value> {
    Json(serde_json::to_value(s.spec.as_ref()).unwrap_or_default())
}

/// `GET /api/state` — return a snapshot of the live run state.
async fn state_handler(State(s): State<ServerState>) -> Json<LiveState> {
    let mut snapshot = s.live.lock().await.clone();
    if !snapshot.finished {
        snapshot.elapsed_secs = s.started_at.elapsed().as_secs_f64();
    }
    Json(snapshot)
}

/// `GET /api/run` — return the durable lifecycle record, when configured.
async fn run_handler(
    State(s): State<ServerState>,
) -> Result<Json<RunRecord>, axum::http::StatusCode> {
    let (Some(store), Some(id)) = (s.run_store, s.run_id) else {
        return Err(axum::http::StatusCode::NOT_FOUND);
    };
    store
        .get(id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

/// `GET /api/events` — SSE stream that replays every `SchedulerEvent`.
async fn events_handler(
    State(s): State<ServerState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = s.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        result.ok().map(|event| {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok(Event::default().data(data))
        })
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
