//! HTTP server for real-time session observability.
//!
//! Exposes three endpoints on `http://127.0.0.1:{port}`:
//! - `GET /api/graph`  — static graph specification (JSON)
//! - `GET /api/state`  — live run state (rounds, active nodes, elapsed)
//! - `GET /api/events` — SSE stream of `SchedulerEvent` objects

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::{get, post},
    Router,
};
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
use tower_http::cors::CorsLayer;

use eureka::graph::GraphSpec;
use eureka::scheduler::SchedulerEvent;
use eureka::{
    graph::artifact::Artifact,
    manager::{CreateRunRequest, RunManager},
    persistence::{RunCheckpoint, RunPersistence, RunRecord},
};

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
    run_store: Option<Arc<dyn RunPersistence>>,
    /// Run ID used by the durable status endpoint.
    run_id: Option<uuid::Uuid>,
    /// Optional application run manager for lifecycle endpoints.
    manager: Option<RunManager>,
    /// Shared atomic counter for live per-request token tracking.
    /// Read by `state_handler` to return up-to-date totals.
    live_tokens: Option<Arc<AtomicU64>>,
    /// Shared atomic for live input token tracking.
    live_input_tokens: Option<Arc<AtomicU64>>,
    /// Shared atomic for live output token tracking.
    live_output_tokens: Option<Arc<AtomicU64>>,
    /// Shared mutex for live per-request cost tracking in USD.
    live_cost: Option<Arc<std::sync::Mutex<f64>>>,
}

/// Budget limits snapshot, exposed in `/api/state` for the dashboard.
#[derive(Debug, Default, Clone, Serialize)]
pub struct BudgetSnapshot {
    /// Maximum number of rounds (or `None` for unbounded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// Maximum total tokens (or `None` for unbounded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Maximum total cost in USD (or `None` for unbounded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    /// Maximum wall-clock seconds (or `None` for unbounded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wallclock_secs: Option<u64>,
}

/// Per-round cost and quality metrics for the `GET /api/metrics` time series.
#[derive(Debug, Default, Clone, Serialize)]
pub struct RoundMetrics {
    /// Round number (0-based).
    pub round: u32,
    /// Wall-clock seconds elapsed when this round completed.
    pub elapsed_secs: f64,
    /// Tokens consumed during this round.
    pub tokens: u64,
    /// Cost (USD) incurred during this round.
    pub cost_usd: f64,
    /// Cumulative tokens at this round boundary.
    pub total_tokens: u64,
    /// Cumulative cost (USD) at this round boundary.
    pub total_cost_usd: f64,
    /// Highest hypothesis score from the ranker at this round (Elo rating).
    pub top_score: Option<f64>,
}

/// Live run state snapshot served at `GET /api/state`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LiveState {
    /// Research goal string.
    pub goal: String,
    /// Run identifier (for lifecycle controls like pause/resume/cancel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Budget limits for this run.
    #[serde(default)]
    pub budget: BudgetSnapshot,
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
    /// Per-round cost and quality metrics (populated at each `CycleCompleted`).
    pub round_metrics: Vec<RoundMetrics>,
    /// Cumulative tokens consumed so far (updated on every `ActivationCompleted`).
    pub live_tokens: u64,
    /// Cumulative input tokens (prompt).
    pub live_input_tokens: u64,
    /// Cumulative output tokens (completion).
    pub live_output_tokens: u64,
    /// Cumulative cost (USD) so far (updated on every `ActivationCompleted`).
    pub live_cost_usd: f64,
}

/// Spawn a background task that subscribes to the broadcast channel
/// and keeps `live` up-to-date from incoming scheduler events.
///
/// The returned handle must be kept alive for the duration of the run.
#[allow(clippy::too_many_lines)]
pub fn track_live_state(
    mut rx: broadcast::Receiver<SchedulerEvent>,
    live: Arc<Mutex<LiveState>>,
) -> tokio::task::JoinHandle<()> {
    let started_at = Instant::now();
    let mut round_tokens: u64 = 0;
    let mut round_cost: f64 = 0.0;
    let mut pending_top_score: Option<f64> = None;

    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let mut s = live.lock().await;
                    match &event {
                        SchedulerEvent::ActivationStarted { node_id, .. } => {
                            s.active_nodes.insert(node_id.clone());
                        }
                        SchedulerEvent::ActivationCompleted { node_id, outputs, usage, .. } => {
                            s.active_nodes.remove(node_id.as_str());
                            if !outputs.is_empty() {
                                s.node_outputs.insert(node_id.clone(), outputs.clone());
                            }
                            round_tokens += usage.total_tokens;
                            round_cost += usage.cost_usd;
                            s.live_tokens += usage.total_tokens;
                            s.live_input_tokens += usage.input_tokens;
                            s.live_output_tokens += usage.output_tokens;
                            s.live_cost_usd += usage.cost_usd;
                            // Scan all outputs for the highest hypothesis score,
                            // regardless of which node produced it.
                            for o in outputs {
                                if let Some(hyps) = o["data"]["hypotheses"].as_array() {
                                    for h in hyps {
                                        if let Some(score) = h["score"].as_f64() {
                                            match pending_top_score {
                                                None => pending_top_score = Some(score),
                                                Some(existing) if score > existing => {
                                                    pending_top_score = Some(score);
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SchedulerEvent::ActivationFailed { node_id, .. } => {
                            s.active_nodes.remove(node_id.as_str());
                        }
                        SchedulerEvent::CycleCompleted { round } => {
                            let cumulative_tokens: u64 =
                                s.round_metrics.iter().map(|m| m.tokens).sum::<u64>()
                                    + round_tokens;
                            let cumulative_cost: f64 =
                                s.round_metrics.iter().map(|m| m.cost_usd).sum::<f64>()
                                    + round_cost;
                            s.round_metrics.push(RoundMetrics {
                                round: *round,
                                elapsed_secs: started_at.elapsed().as_secs_f64(),
                                tokens: round_tokens,
                                cost_usd: round_cost,
                                total_tokens: cumulative_tokens,
                                total_cost_usd: cumulative_cost,
                                top_score: pending_top_score,
                            });
                            round_tokens = 0;
                            round_cost = 0.0;
                            pending_top_score = None;
                            s.rounds = *round;
                        }
                        SchedulerEvent::RunHalted { total_rounds, .. } => {
                            // Flush partial-round accumulators so metrics are complete.
                            if round_tokens > 0 || round_cost > 0.0 {
                                let cumulative_tokens: u64 =
                                    s.round_metrics.iter().map(|m| m.tokens).sum::<u64>()
                                        + round_tokens;
                                let cumulative_cost: f64 =
                                    s.round_metrics.iter().map(|m| m.cost_usd).sum::<f64>()
                                        + round_cost;
                                s.round_metrics.push(RoundMetrics {
                                    round: *total_rounds,
                                    elapsed_secs: started_at.elapsed().as_secs_f64(),
                                    tokens: round_tokens,
                                    cost_usd: round_cost,
                                    total_tokens: cumulative_tokens,
                                    total_cost_usd: cumulative_cost,
                                    top_score: pending_top_score,
                                });
                                round_tokens = 0;
                                round_cost = 0.0;
                                pending_top_score = None;
                            }
                            s.rounds = *total_rounds;
                            s.finished = true;
                        }
                        SchedulerEvent::RunPaused { round } => {
                            // Flush partial-round accumulators before pausing.
                            if round_tokens > 0 || round_cost > 0.0 {
                                let cumulative_tokens: u64 =
                                    s.round_metrics.iter().map(|m| m.tokens).sum::<u64>()
                                        + round_tokens;
                                let cumulative_cost: f64 =
                                    s.round_metrics.iter().map(|m| m.cost_usd).sum::<f64>()
                                        + round_cost;
                                s.round_metrics.push(RoundMetrics {
                                    round: *round,
                                    elapsed_secs: started_at.elapsed().as_secs_f64(),
                                    tokens: round_tokens,
                                    cost_usd: round_cost,
                                    total_tokens: cumulative_tokens,
                                    total_cost_usd: cumulative_cost,
                                    top_score: pending_top_score,
                                });
                                round_tokens = 0;
                                round_cost = 0.0;
                                pending_top_score = None;
                            }
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
    run_store: Option<Arc<dyn RunPersistence>>,
    run_id: Option<uuid::Uuid>,
) -> tokio::task::JoinHandle<()> {
    start_server_with_manager(
        spec, event_tx, live, port, run_store, run_id, None, None, None, None, None,
    )
}

/// Start the UI server with an optional manager and run store.
#[allow(clippy::too_many_arguments)]
pub fn start_server_with_manager(
    spec: GraphSpec,
    event_tx: broadcast::Sender<SchedulerEvent>,
    live: Arc<Mutex<LiveState>>,
    port: u16,
    run_store: Option<Arc<dyn RunPersistence>>,
    run_id: Option<uuid::Uuid>,
    manager: Option<RunManager>,
    live_tokens: Option<Arc<AtomicU64>>,
    live_input_tokens: Option<Arc<AtomicU64>>,
    live_output_tokens: Option<Arc<AtomicU64>>,
    live_cost: Option<Arc<std::sync::Mutex<f64>>>,
) -> tokio::task::JoinHandle<()> {
    if port == 0 {
        return tokio::spawn(async {});
    }
    let spec = Arc::new(spec);
    let frontend_path = spec.frontend.clone();
    let state = ServerState {
        spec,
        event_tx,
        live,
        started_at: Instant::now(),
        run_store,
        run_id,
        manager,
        live_tokens,
        live_input_tokens,
        live_output_tokens,
        live_cost,
    };

    let mut app = Router::new()
        .route("/api/graph", get(graph_handler))
        .route("/api/state", get(state_handler))
        .route("/api/run", get(run_handler))
        .route("/api/events/history", get(event_history_handler))
        .route("/api/events", get(events_handler))
        .route("/api/metrics", get(metrics_handler))
        .route("/runs", post(create_run_handler).get(list_runs_handler))
        .route("/runs/{id}", get(get_run_handler))
        .route("/runs/{id}/pause", post(pause_run_handler))
        .route("/runs/{id}/resume", post(resume_run_handler))
        .route("/runs/{id}/cancel", post(cancel_run_handler))
        .route("/runs/{id}/input", post(input_run_handler))
        .route("/runs/{id}/checkpoint", get(get_run_checkpoint_handler))
        .route("/runs/{id}/events", get(get_run_events_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    if let Some(path) = frontend_path {
        app = app.fallback_service(
            tower_http::services::ServeDir::new(path).append_index_html_on_directories(true),
        );
    }

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
    snapshot.elapsed_secs = s.started_at.elapsed().as_secs_f64();
    // Override with live per-request counters so the UI reflects cost/tokens
    // immediately after each LLM completion or embedding request. Use max of
    // event-driven and per-request values so that zero atomics (no per-request
    // wiring) don't overwrite event-driven accumulation.
    if let Some(ref t) = s.live_tokens {
        snapshot.live_tokens = snapshot.live_tokens.max(t.load(Ordering::Relaxed));
    }
    if let Some(ref t) = s.live_input_tokens {
        snapshot.live_input_tokens = snapshot.live_input_tokens.max(t.load(Ordering::Relaxed));
    }
    if let Some(ref t) = s.live_output_tokens {
        snapshot.live_output_tokens = snapshot.live_output_tokens.max(t.load(Ordering::Relaxed));
    }
    if let Some(ref c) = s.live_cost {
        if let Ok(guard) = c.lock() {
            let cost = *guard;
            if cost > 0.0 {
                snapshot.live_cost_usd = cost;
            }
        }
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

/// `GET /api/events/history` — return durable scheduler events from `SQLite`.
async fn event_history_handler(
    State(s): State<ServerState>,
) -> Result<Json<Vec<eureka::persistence::RunEvent>>, axum::http::StatusCode> {
    let (Some(store), Some(id)) = (s.run_store, s.run_id) else {
        return Err(axum::http::StatusCode::NOT_FOUND);
    };
    store.load_events(id).await.map(Json).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

/// Request body for creating a new run.
#[derive(Debug, serde::Deserialize)]
struct CreateRunBody {
    /// Initial goal artifact payload.
    goal: serde_json::Value,
}

/// Request body for injecting an input into a run.
#[derive(Debug, serde::Deserialize)]
struct InputBody {
    /// Target node ID.
    node_id: String,
    /// Target input port name.
    port: String,
    /// Artifact to inject.
    artifact: Artifact,
}

/// Extract the `RunManager` from the server state, returning 404 if absent.
fn manager_or_404(state: &ServerState) -> Result<RunManager, axum::http::StatusCode> {
    state.manager.clone().ok_or(axum::http::StatusCode::NOT_FOUND)
}

/// `POST /runs` — create and start a managed run.
async fn create_run_handler(
    State(state): State<ServerState>,
    Json(body): Json<CreateRunBody>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    let id = manager
        .create_run(CreateRunRequest { goal: body.goal })
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((axum::http::StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))))
}

/// `GET /runs` — list managed runs.
async fn list_runs_handler(
    State(state): State<ServerState>,
) -> Result<Json<Vec<RunRecord>>, axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    manager.list_runs().await.map(Json).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

/// `GET /runs/:id` — retrieve one managed run.
async fn get_run_handler(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<RunRecord>, axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    manager
        .get_run(id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

/// `POST /runs/:id/pause` — request a pause.
macro_rules! signal_handler {
    ($name:ident, $method:ident) => {
        async fn $name(
            State(state): State<ServerState>,
            Path(id): Path<uuid::Uuid>,
        ) -> Result<axum::http::StatusCode, axum::http::StatusCode> {
            manager_or_404(&state)?
                .$method(id)
                .await
                .map(|()| axum::http::StatusCode::ACCEPTED)
                .map_err(|_| axum::http::StatusCode::CONFLICT)
        }
    };
}

signal_handler!(pause_run_handler, pause_run);
signal_handler!(resume_run_handler, resume_run);
signal_handler!(cancel_run_handler, cancel_run);

/// `POST /runs/:id/input` — inject a typed artifact into a paused run.
async fn input_run_handler(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<InputBody>,
) -> Result<Json<RunCheckpoint>, axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    manager
        .submit_input(id, body.node_id, body.port, body.artifact)
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::CONFLICT)
}

/// `GET /runs/:id/checkpoint` — load the latest durable checkpoint for a run.
async fn get_run_checkpoint_handler(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<RunCheckpoint>, axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    manager
        .get_checkpoint(id)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

/// `GET /runs/:id/events` — load the durable scheduler event history for a run.
async fn get_run_events_handler(
    State(state): State<ServerState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Vec<eureka::persistence::RunEvent>>, axum::http::StatusCode> {
    let manager = manager_or_404(&state)?;
    manager
        .get_events(id)
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

/// `GET /api/metrics` — per-round cost and quality time series.
async fn metrics_handler(State(s): State<ServerState>) -> Json<Vec<RoundMetrics>> {
    Json(s.live.lock().await.round_metrics.clone())
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

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
}
