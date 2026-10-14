use crate::grok::Grok;
use crate::session::Entry;
use crate::tool::{Abort, Tool};
use crate::vertex::Vertex;
use anyhow::Result;

/// Chat backend. The agent loop talks only to this.
/// The model id picks the backend. There is no provider environment variable.
#[derive(Clone)]
pub enum Provider {
    Grok(Grok),
    Vertex(Vertex),
}

/// One row in the model picker. The id is enough; the backend follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: &'static str,
}

/// Models the picker offers.
pub fn model_catalog() -> Vec<ModelChoice> {
    vec![
        ModelChoice { id: "grok-4.7" },
        ModelChoice { id: "gemini-4-argon" },
    ]
}

pub fn is_vertex_model(model: &str) -> bool {
    let id = model.trim().to_ascii_lowercase();
    id.contains("gemini") || id.contains("argon")
}

impl Provider {
    /// Open the backend that owns `model`.
    pub async fn open(model: &str, effort: &str) -> Result<Self> {
        if is_vertex_model(model) {
            Ok(Self::Vertex(Vertex::client(effort.to_string())?))
        } else {
            let (mut grok, _) = Grok::client().await?;
            if !effort.trim().is_empty() {
                grok.effort = effort.to_string();
            }
            Ok(Self::Grok(grok))
        }
    }
}

impl Provider {
    pub fn effort(&self) -> &str {
        match self {
            Self::Grok(g) => &g.effort,
            Self::Vertex(v) => &v.effort,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete(
        &self,
        model: &str,
        system: &str,
        entries: &[Entry],
        tools: &[Tool],
        abort: &Abort,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        match self {
            Self::Grok(g) => {
                g.complete(model, system, entries, tools, abort, on_delta, on_think)
                    .await
            }
            Self::Vertex(v) => {
                v.complete(model, system, entries, tools, abort, on_delta, on_think)
                    .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_with_effort(
        &self,
        model: &str,
        system: &str,
        entries: &[Entry],
        tools: &[Tool],
        abort: &Abort,
        effort: &str,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        match self {
            Self::Grok(g) => {
                g.complete_with_effort(
                    model, system, entries, tools, abort, effort, on_delta, on_think,
                )
                .await
            }
            Self::Vertex(v) => {
                v.complete_with_effort(
                    model, system, entries, tools, abort, effort, on_delta, on_think,
                )
                .await
            }
        }
    }
}
