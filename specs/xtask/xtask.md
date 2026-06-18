# Spec: xtask

> **Status:** Stable
> **Location:** `xtask/`
> **Invocation:** `cargo run --package xtask -- <command>`

## Purpose

`xtask` is the project's build automation binary. It provides commands that go beyond what `cargo` offers natively: graph validation across all graph packages, documentation generation, and lint enforcement. It is a standard Rust xtask (a `[[bin]]` in its own crate, not a cargo subcommand).

## Commands

### `validate-graphs [dir]`

Walk `<dir>` (default: `graphs/`) and validate every `graph.json` found in a subdirectory.

```
  ✓  graphs/coscientist/graph.json
  ✗  graphs/broken/graph.json:
       edge references unknown node "missing_node"

1 valid, 1 invalid
```

**Algorithm:**
1. Read the directory; for each subdirectory, check for `<subdir>/graph.json`.
2. Parse each `graph.json` as `GraphSpec`.
3. Call `validate_graph(&spec, &port_registry)` using `build_port_registry()`.
4. Print `✓` or `✗` with reason. Exit non-zero if any graph is invalid.

`build_port_registry()` registers all 10 standard node kinds — the same set as `build_registry_for_validation()` in `eureka-cli`. These two functions must be kept in sync.

### `doc [--open]`

Run `cargo doc --no-deps --workspace`. Pass `--open` to open the generated HTML in the browser after building.

### `lint`

Run the full lint bundle in this order:

1. `cargo fmt --check` — format check (no writes)
2. `cargo clippy --all-targets --all-features -- -D warnings` — clippy with all targets and features, warnings as errors

Each step is run sequentially; the bundle aborts on first failure and exits non-zero.

## Invocation

```bash
cargo run --package xtask -- validate-graphs
cargo run --package xtask -- validate-graphs graphs/
cargo run --package xtask -- doc
cargo run --package xtask -- doc --open
cargo run --package xtask -- lint
```

Note: `cargo xtask` is **not** a valid cargo subcommand — there is no `.cargo/config.toml` alias in this repo. Always use `cargo run --package xtask --`.

## Standard Port Registry (`build_port_registry`)

Defined in `xtask/src/main.rs`. Registers the same 10 node kinds as `build_registry_for_validation()` in `crates/eureka-cli/src/commands/validate.rs`:

| Kind | Inputs | Outputs |
|---|---|---|
| `generation` | `in:Goal`, `context:Insights?` | `out:Hypotheses` |
| `reflection` | `in:Hypotheses` | `out:Reviews` |
| `evolution` | `in:Hypotheses` | `out:Hypotheses` |
| `meta_review` | `in:Ranking` | `insights:Insights`, `overview:Overview` |
| `control.ranking` | `in:Reviews`, `cycle:Control?` | `top:Hypotheses`, `state:Ranking` |
| `control.proximity` | `in:Hypotheses` | `unique:Hypotheses` |
| `control.governor` | `in:Hypotheses` | `continue:Control`, `halt:Control` |
| `control.merge` | `in_a?`, `in_b?`, `in_c?:Hypotheses` | `out:Hypotheses`, `state:Ranking` |
| `control.router` | `in:Hypotheses` | `out:Hypotheses` |
| `control.human_gate` | `in:Control` | `out:Goal` |

## Invariants

- `build_port_registry()` in xtask and `build_registry_for_validation()` in `eureka-cli` must register identical port specs for all shared node kinds.
- `validate-graphs` exits non-zero if any `graph.json` fails validation.
- xtask has no dependencies on `eureka-*` crates beyond `eureka-graph` (the types it validates).

## Non-Goals

- xtask does not build or run the project (use `cargo build` / `cargo run`).
- xtask does not manage agent files — only graph topology is validated.
- xtask is not a CI replacement; it is a developer convenience tool.
