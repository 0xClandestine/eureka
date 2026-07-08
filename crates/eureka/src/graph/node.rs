//! The `Node` trait — the fundamental processing unit in the graph.

use std::ops::{Add, AddAssign};
use std::sync::Arc;

use async_trait::async_trait;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::artifact::Artifact;
use super::port::{PortId, PortSpec};
use crate::scheduler::SchedulerEvent;

/// A message arriving on an input port of a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMsg {
    /// The port on which this message arrived.
    pub port: PortId,
    /// The artifact carried by this message.
    pub artifact: Artifact,
}

/// An emission on an output port of a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emit {
    /// The port on which to emit.
    pub port: PortId,
    /// The artifact to emit.
    pub artifact: Artifact,
}

impl Emit {
    /// Create a new emission on the given port with the given artifact.
    #[must_use]
    pub fn new(port: impl Into<String>, artifact: Artifact) -> Self {
        Self {
            port: port.into(),
            artifact,
        }
    }
}

/// Runtime context provided to a node during processing.
///
/// Provides access to shared state stores, agent context, and lifecycle
/// primitives like cancellation.
#[derive(Debug, Clone)]
pub struct NodeCtx {
    /// The unique ID of the node being activated.
    pub node_id: String,
    /// The kind of node being activated.
    pub node_kind: String,
    /// The current round number.
    pub round: u32,
    /// A cancellation token for graceful shutdown.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Optional sender for emitting observability events (e.g. tool calls).
    pub event_tx: Option<mpsc::Sender<SchedulerEvent>>,
}

impl NodeCtx {
    /// Create a new node context.
    #[must_use]
    pub fn new(
        node_id: impl Into<String>,
        node_kind: impl Into<String>,
        round: u32,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            node_kind: node_kind.into(),
            round,
            cancel,
            event_tx: None,
        }
    }

    /// Check if the run has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Errors that can occur during node processing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum NodeError {
    /// An agent-specific error occurred.
    #[error("Agent error: {0}")]
    Agent(String),

    /// The artifact kind does not match the expected port kind.
    #[error("Port kind mismatch on port {port}: expected {expected:?}, got {got:?}")]
    PortKind {
        /// The port name.
        port: String,
        /// The expected artifact kind.
        expected: &'static str,
        /// The actual artifact kind.
        got: String,
    },

    /// A timeout occurred during processing.
    #[error("Node processing timed out: {0}")]
    Timeout(String),

    /// The node was cancelled.
    #[error("Node processing cancelled")]
    Cancelled,

    /// A critical internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Resource usage reported by a node for a single activation.
///
/// The scheduler accumulates these into [`crate::graph::control::RunStats`] so
/// the cost/token budget backstops can fire. LLM-backed nodes populate token
/// counts (and an optional best-effort cost when a per-model rate is
/// configured); subprocess and mock nodes return the zero default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeUsage {
    /// Prompt/input tokens consumed by this activation.
    pub input_tokens: u64,
    /// Completion/output tokens consumed by this activation.
    pub output_tokens: u64,
    /// Total tokens consumed by this activation. Equal to
    /// `input_tokens + output_tokens` when the provider reports both; may be
    /// the only non-zero field for providers that report a single aggregate.
    pub total_tokens: u64,
    /// Best-effort cost in USD for this activation. `0.0` when no per-model
    /// rate is configured (see `ProviderConfig::cost_per_million_tokens`).
    pub cost_usd: f64,
}

impl NodeUsage {
    /// Create a usage report from rig's normalized token counts and an
    /// optional cost-per-million-tokens rate.
    #[must_use]
    pub fn from_rig_usage(
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
        cost_per_million_tokens: Option<f64>,
    ) -> Self {
        // Fall back to input+output when the provider omits an aggregate.
        let total = if total_tokens == 0 {
            input_tokens + output_tokens
        } else {
            total_tokens
        };
        let cost_usd = cost_per_million_tokens
            .map(|rate| (total as f64 / 1_000_000.0) * rate)
            .unwrap_or(0.0);
        Self {
            input_tokens,
            output_tokens,
            total_tokens: total,
            cost_usd,
        }
    }
}

impl Add for NodeUsage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            total_tokens: self.total_tokens + rhs.total_tokens,
            cost_usd: self.cost_usd + rhs.cost_usd,
        }
    }
}

impl AddAssign for NodeUsage {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

/// The fundamental processing unit in the graph.
///
/// A `Node` reacts to artifacts on its input ports and emits artifacts
/// on its output ports. Nodes can represent agents, tools, proximity
/// steps, or control logic (governors, routers, merges).
#[async_trait]
pub trait Node: Send + Sync {
    /// Declared input/output ports and the `ArtifactKind` each carries.
    fn ports(&self) -> PortSpec;

    /// React to one or more input artifacts arriving on the node's input
    /// ports, then emit zero or more artifacts on named output ports.
    ///
    /// The scheduler joins inputs: when a node has all required inputs ready
    /// for a round, `process` is called once with every available input
    /// (required inputs plus any optional inputs that have arrived). For
    /// single-input nodes this is exactly one `PortMsg`; for multi-input
    /// nodes it is one per populated port.
    ///
    /// Returns the emitted artifacts alongside a [`NodeUsage`] report so the
    /// scheduler can accumulate cost/token budget accounting. Non-LLM nodes
    /// return [`NodeUsage::default`] (zero cost/tokens).
    async fn process(
        &self,
        ctx: &NodeCtx,
        inputs: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, NodeUsage), NodeError>;
}

/// A type-erased boxed node that supports cloning via Arc.
///
/// Used by the scheduler to hold heterogeneous nodes in a single registry.
#[derive(Clone)]
pub struct BoxedNode {
    /// The underlying type-erased node.
    inner: Arc<dyn Node>,
}

impl BoxedNode {
    /// Wrap a `Node` as a `BoxedNode`.
    #[must_use]
    pub fn new(node: impl Node + 'static) -> Self {
        Self {
            inner: Arc::new(node),
        }
    }

    /// Get the port specification.
    #[must_use]
    pub fn ports(&self) -> PortSpec {
        self.inner.ports()
    }

    /// Process an input message.
    ///
    /// # Errors
    ///
    /// Returns a `NodeError` if the underlying node fails to process the message.
    pub async fn process(
        &self,
        ctx: &NodeCtx,
        inputs: Vec<PortMsg>,
    ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
        self.inner.process(ctx, inputs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct EchoNode;

    #[async_trait]
    impl Node for EchoNode {
        fn ports(&self) -> PortSpec {
            PortSpec::new(vec![], vec![])
        }

        async fn process(
            &self,
            _ctx: &NodeCtx,
            inputs: Vec<PortMsg>,
        ) -> Result<(Vec<Emit>, NodeUsage), NodeError> {
            Ok((
                inputs
                    .into_iter()
                    .map(|m| Emit::new(m.port, m.artifact))
                    .collect(),
                NodeUsage::default(),
            ))
        }
    }

    #[tokio::test]
    async fn test_echo_node() {
        let node = BoxedNode::new(EchoNode);
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = NodeCtx::new("echo", "test", 0, cancel);
        let msg = PortMsg {
            port: "in".into(),
            artifact: Artifact {
                kind: "Goal".to_string(),
                data: serde_json::json!({ "goal": "Test" }),
            },
        };
        let (emits, _usage) = node.process(&ctx, vec![msg]).await.unwrap();
        assert_eq!(emits.len(), 1);
    }
}
