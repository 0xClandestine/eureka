# Spec: UI Server

> **Status:** Stable
> **Crate:** `eureka-cli`
> **Files:** `server.rs`

## Purpose

An embedded axum HTTP server that exposes the running graph state to the EurekaUI
Swift app over REST and Server-Sent Events (SSE). It runs concurrently with the
research session in the same process.

## Design

### Startup

`start_server(spec, event_tx, live, port)` is called in `run::execute()` before
`session.run()`. It receives:
- `spec: GraphSpec` — static copy of the graph spec (wrapped in `Arc` internally).
- `event_tx: broadcast::Sender<SchedulerEvent>` — handlers subscribe for their own receiver.
- `live: Arc<Mutex<LiveState>>` — shared live state updated by `track_live_state`.
- `port: u16` — TCP port to bind on `127.0.0.1`.

The function spawns a background `tokio::task` and returns its `JoinHandle`. If the
port is already in use, the error is logged and the task exits silently.

Port is configurable via `--port` (default: 7773). `--port 0` disables the server
entirely — neither `start_server` nor `track_live_state` is called, and the broadcast
sender is dropped.

### Shared State

`ServerState` (cloned into every handler via axum's `State` extractor):

| Field | Type | Description |
|---|---|---|
| `spec` | `Arc<GraphSpec>` | Static graph spec |
| `event_tx` | `broadcast::Sender<SchedulerEvent>` | Source for new SSE subscriptions |
| `live` | `Arc<Mutex<LiveState>>` | Current live run state |

### LiveState

`LiveState` (served at `GET /api/state`):

| Field | Type | Description |
|---|---|---|
| `rounds` | `u32` | Scheduler rounds completed so far |
| `elapsed_secs` | `f64` | Wall-clock seconds elapsed |
| `active_nodes` | `HashSet<String>` | Node IDs currently in-flight |
| `finished` | `bool` | Whether the session has halted |
| `node_outputs` | `HashMap<String, Vec<serde_json::Value>>` | Latest outputs per node |

`node_outputs` values are the `outputs` field from `ActivationCompleted` events — only
updated when the output list is non-empty. Format: `[{ port, kind, data }, ...]`.

### `track_live_state`

`track_live_state(rx, live)` is a standalone exported function that spawns a background
task subscribing to the broadcast channel and updating `LiveState`:

| Event | Effect on `LiveState` |
|---|---|
| `ActivationStarted { node_id }` | Insert `node_id` into `active_nodes` |
| `ActivationCompleted { node_id, outputs }` | Remove from `active_nodes`; update `node_outputs` if outputs non-empty |
| `ActivationFailed { node_id }` | Remove `node_id` from `active_nodes` |
| `CycleCompleted { round }` | Set `rounds = round` |
| `RunHalted { total_rounds }` | Set `rounds = total_rounds`; set `finished = true` |

If the broadcast channel lags (slow consumer), the tracker logs a warning and skips
the dropped events. When the channel closes the task exits.

### Endpoints

#### `GET /api/graph` → `GraphSpec`

Returns the full graph specification as JSON. Static — identical response for the
duration of the run.

```json
{
  "name": "AI Co-Scientist",
  "nodes": [...],
  "edges": [...]
}
```

#### `GET /api/state` → `LiveState`

Returns a snapshot of the current run state.

```json
{
  "rounds": 3,
  "elapsed_secs": 14.2,
  "active_nodes": ["ranking"],
  "finished": false,
  "node_outputs": {
    "ranking": [
      { "port": "top", "kind": "Hypotheses", "data": { "..." : "..." } },
      { "port": "state", "kind": "Ranking", "data": { "..." : "..." } }
    ]
  }
}
```

`node_outputs` holds the most recent outputs from each completed node. This allows a
UI client that connects mid-run to reconstruct the current state without replaying
all events.

#### `GET /api/events` → SSE stream

A persistent Server-Sent Events stream. Each event carries a JSON-encoded
`SchedulerEvent` as the `data` field. Lagged events are silently skipped (not
retransmitted). Keep-alive pings are sent every 15 seconds with text `"ping"`.

```
data: {"type":"activationStarted","nodeId":"generation","nodeKind":"generation","round":1}

data: {"type":"activationCompleted","nodeId":"generation","nodeKind":"generation","round":1,"emitCount":1,"outputs":[{"port":"out","kind":"Hypotheses","data":{...}}]}

data: {"type":"cycleCompleted","round":1}

data: {"type":"runHalted","reason":"max_rounds reached","totalRounds":5}
```

The client should reconnect automatically if the stream drops. The server does not
replay past events on reconnect — the client should call `GET /api/state` first to
get the current snapshot, then subscribe to the stream for live updates.

### CORS

All endpoints use `CorsLayer::permissive()` (allow `*` origin) to support development
scenarios where the UI runs on a different port.

## Invariants

- The server must be started before `session.run()` begins so clients connecting at
  the start of a run receive the first events.
- `live` uses `tokio::sync::Mutex` (not `std::sync::RwLock` or `tokio::sync::RwLock`).
- `node_outputs` in `LiveState` always holds the latest outputs per node, not a history.
- SSE events are JSON-encoded `SchedulerEvent` values — the exact discriminant field
  names are determined by `SchedulerEvent`'s `serde` derive.
- When `port == 0`, neither `start_server` nor `track_live_state` is called.

## Non-Goals

- The server does not authenticate connections.
- The server does not support POST endpoints for human-in-the-loop input.
- The server does not persist state to disk.
- The SSE stream does not replay history on reconnect.

## Open Questions

- Should `GET /api/state` include a full event history for late-joining clients?
- Should the server accept `POST /api/inject` for the human gate to resume?
- Should `node_outputs` accumulate per round (history) or only store the latest?
