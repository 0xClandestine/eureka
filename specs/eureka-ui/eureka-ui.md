# Spec: EurekaUI

> **Status:** Stable
> **Location:** `EurekaUI/`
> **Platform:** macOS 14+, SwiftUI
> **Build:** `cd EurekaUI && swift build`
> **Backend:** Connects to `http://127.0.0.1:7773` (eureka-cli UI server)

## Purpose

EurekaUI is a scientist-facing macOS SwiftUI app for monitoring and interacting with a running Eureka session. It connects to the eureka-cli HTTP/SSE server, displays live hypothesis rankings, reviews, timelines, and meta-review summaries.

Default window size: 1380×840.

---

## Layout

The top-level view (`ContentView.swift`) is a `NavigationSplitView` with:

- **Sidebar** — connection status, live agent activity cards, session stats
- **Detail** — 4-tab interface (`ScientistTab` enum)

### Sidebar (`Views/SidebarView.swift`)

- Connection status indicator (connected / disconnected to `http://127.0.0.1:7773`)
- `ActiveAgent` cards: one per currently-executing node, showing node name and elapsed time
- Session stats: round number, total hypotheses seen, total reviews seen

### Tabs (`ContentView.swift` — `ScientistTab` enum)

| Tab | View | Description |
|---|---|---|
| Hypotheses | `HypothesesView` (`Views/GraphCanvasView.swift`) | Elo leaderboard (left) + full hypothesis reader (right) |
| Timeline | `TimelineView` (`Views/ScoreChartView.swift`) | Round picker + hypothesis cards per round |
| Reviews | `ReviewsView` + `ReviewCard` (`Views/EventFeedView.swift`) | Expandable review cards with score/reasoning/strengths/weaknesses |
| Overview | `OverviewView` (`Views/OverviewView.swift`) | Meta-review summary text + numbered insights list |

---

## Views

### `HypothesesView` (`Views/GraphCanvasView.swift`)

Two-pane layout:

- **Left**: Elo leaderboard — ranked list of `RankedHypothesis` items, showing statement text and Elo score, sorted descending by score.
- **Right**: `HypothesisDetailView` (`Views/NodeOutputView.swift`) — full hypothesis reader showing all fields: `statement`, `rationale`, `assumptions`, `predictions`, `experiment`.

### `HypothesisDetailView` (`Views/NodeOutputView.swift`)

Displays all fields of a selected `Hypothesis`:
- `statement` — the core claim
- `rationale` — reasoning behind the hypothesis
- `assumptions` — listed assumptions
- `predictions` — testable predictions
- `experiment` — proposed experimental design

### `TimelineView` (`Views/ScoreChartView.swift`)

- Round picker: select which round to inspect
- Hypothesis cards for the selected round (from `hypothesisHistory`)
- Shows the state of hypotheses as of each round

### `ReviewsView` and `ReviewCard` (`Views/EventFeedView.swift`)

- List of `Review` items from the latest reflection node output
- Each `ReviewCard` is expandable, showing:
  - `score` (numeric)
  - `reasoning`
  - `strengths`
  - `weaknesses`
  - Full hypothesis pass-through (the reviewed hypothesis)

### `OverviewView` (`Views/OverviewView.swift`)

- `overviewText` — meta-review summary paragraph
- `insightsList` — numbered list of insights from the meta-review node

---

## Data Models (`Models/EurekaModels.swift`)

### `Hypothesis`

```
statement: String
rationale: String
assumptions: [String]
predictions: [String]
experiment: String
```

### `RankedHypothesis`

A `Hypothesis` with an associated Elo score:

```
hypothesis: Hypothesis
eloScore: Double
```

### `Review`

```
score: Double (or Int)
reasoning: String
strengths: [String]
weaknesses: [String]
hypothesis: Hypothesis   // full pass-through from reflection agent
```

### `HypothesisSnapshot`

A snapshot of the hypothesis list as of a given round, used for `TimelineView`:

```
round: Int
hypotheses: [Hypothesis]
```

### `ActiveAgent`

Represents a currently-executing node for display in the sidebar:

```
nodeId: String
name: String
startedAt: Date
```

---

## App State (`Models/AppState.swift`)

`AppState` is decorated with `@Observable`. It owns all mutable UI state and drives all views.

### Properties

| Property | Type | Description |
|---|---|---|
| `rankedHypotheses` | `[RankedHypothesis]` | Current Elo-ranked hypothesis list |
| `hypothesisHistory` | `[HypothesisSnapshot]` | Per-round hypothesis snapshots for Timeline |
| `latestReviews` | `[Review]` | Most recent reflection output |
| `overviewText` | `String` | Meta-review overview paragraph |
| `insightsList` | `[String]` | Meta-review insights (numbered in UI) |
| `activeAgents` | `[ActiveAgent]` | Nodes currently executing |
| `isConnected` | `Bool` | SSE connection status |

### SSE Parsing

SSE events arrive as `ActivationCompleted` events from the eureka-cli server (`GET /api/events`). Each event carries a `node_id` and `outputs` map of `{ kind, data }` artifacts.

| Source node | Artifact(s) parsed | Target properties |
|---|---|---|
| `ranking` | `Hypotheses` + `Ranking` artifacts | `rankedHypotheses`, `hypothesisHistory` |
| `reflection` | `Reviews` artifact | `latestReviews` |
| `meta_review` | `Overview` + `Insights` artifacts | `overviewText`, `insightsList` |

#### Ranking artifact format

The `Ranking` artifact `data` contains:

```json
{
  "elo_ratings": {
    "<hypothesis statement text>": 1342.5,
    ...
  }
}
```

`AppState` joins Elo scores to hypothesis objects by matching on statement text, then sorts descending by score to produce `rankedHypotheses`.

---

## Network Layer (`Services/EurekaClient.swift`)

Uses `URLSession` for REST calls and SSE streaming.

### REST endpoints

| Method | Path | Returns |
|---|---|---|
| `GET` | `/api/graph` | `GraphSpec` JSON |
| `GET` | `/api/state` | `LiveState` JSON (current node states) |

### SSE endpoint

`GET /api/events` — streams newline-delimited SSE events. Each event is a JSON object. `EurekaClient` parses incoming data lines and delivers parsed event objects to `AppState` via a callback or async stream.

### Connection management

- `EurekaClient` attempts to connect on app launch.
- `AppState.isConnected` is set to `true` when the SSE stream is established and `false` on disconnect or error.
- Reconnection is handled automatically (retry on error).

---

## Key Files

| File | Purpose |
|---|---|
| `ContentView.swift` | Root `NavigationSplitView` + `ScientistTab` tab bar |
| `Views/GraphCanvasView.swift` | `HypothesesView` — leaderboard + detail pane |
| `Views/NodeOutputView.swift` | `HypothesisDetailView` — full hypothesis reader |
| `Views/ScoreChartView.swift` | `TimelineView` — round picker + per-round cards |
| `Views/EventFeedView.swift` | `ReviewsView` + `ReviewCard` |
| `Views/OverviewView.swift` | `OverviewView` — meta-review summary + insights |
| `Views/SidebarView.swift` | `SidebarView` — connection status + active agents |
| `Models/EurekaModels.swift` | `Hypothesis`, `RankedHypothesis`, `Review`, `HypothesisSnapshot`, `ActiveAgent` |
| `Models/AppState.swift` | `@Observable` app state; SSE event parsing |
| `Services/EurekaClient.swift` | `URLSession` REST + SSE client |

---

## Invariants

- EurekaUI is read-only with respect to the session — it observes but does not mutate session state (no write endpoints used).
- All data flows through SSE events; REST endpoints are used for initial state load only.
- `AppState` is the single source of truth; views do not hold their own copies of model data.
- The app targets macOS 14+ and has no iOS or iPadOS target.
