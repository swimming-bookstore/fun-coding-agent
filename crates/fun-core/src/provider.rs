use crate::grok::Grok;
use crate::session::Entry;
use crate::tool::{Abort, Tool};
use crate::vertex::Vertex;
use anyhow::Result;

/// Shared inputs for one completion. Callbacks stay outside so each retry can
/// reborrow them.
pub struct CompleteReq<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub entries: &'a [Entry],
    pub tools: &'a [Tool],
    pub abort: &'a Abort,
}

/// Stream progress shared by a completion and its retries.
pub struct StreamSink<'a> {
    pub started: &'a mut bool,
    pub on_delta: &'a mut dyn FnMut(&str),
    pub on_think: &'a mut dyn FnMut(&str),
}

/// Chat backend. The agent loop talks only to this.
/// The model id picks the backend. A `vertex/` prefix or a Gemini id uses Vertex.
#[derive(Clone)]
pub enum Provider {
    Grok(Grok),
    Vertex(Vertex),
}

/// Short name shown in the model popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Xai,
    Vertex,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::Vertex => "vertex",
        }
    }
}

/// One row in the model picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: &'static str,
    pub provider: Backend,
}

/// Models the picker offers.
pub fn model_catalog() -> Vec<ModelChoice> {
    vec![
        ModelChoice {
            id: "grok-4.7",
            provider: Backend::Xai,
        },
        ModelChoice {
            id: "gemini-3.5-flash",
            provider: Backend::Vertex,
        },
        ModelChoice {
            id: "gemini-3.1-pro-preview",
            provider: Backend::Vertex,
        },
        ModelChoice {
            id: "gemini-2.5-pro",
            provider: Backend::Vertex,
        },
        ModelChoice {
            id: "gemini-2.5-flash",
            provider: Backend::Vertex,
        },
        ModelChoice {
            id: "gemini-4-argon",
            provider: Backend::Vertex,
        },
    ]
}

/// `vertex/…`, `google/…`, and a bare Gemini id use Vertex (`GOOGLE_CLOUD_PROJECT`).
/// Anything else is xAI.
pub fn backend_for(model: &str) -> Backend {
    let raw = model.trim();
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("xai/") || lower.starts_with("grok/") {
        return Backend::Xai;
    }
    if lower.starts_with("vertex/")
        || lower.starts_with("google-vertex/")
        || lower.starts_with("google/")
    {
        return Backend::Vertex;
    }
    if is_gemini_model(raw) {
        return Backend::Vertex;
    }
    Backend::Xai
}

pub fn is_gemini_model(model: &str) -> bool {
    let id = model.trim().to_ascii_lowercase();
    let id = id
        .strip_prefix("google/")
        .or_else(|| id.strip_prefix("vertex/"))
        .or_else(|| id.strip_prefix("google-vertex/"))
        .unwrap_or(id.as_str());
    id.contains("gemini")
}

/// Model id sent on the wire. Strips a `provider/` prefix.
pub fn wire_model(model: &str) -> &str {
    let raw = model.trim();
    raw.split_once('/')
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(raw)
}

impl Provider {
    /// Open the backend that owns `model`.
    pub async fn open(model: &str, effort: &str) -> Result<Self> {
        match backend_for(model) {
            Backend::Vertex => Ok(Self::Vertex(Vertex::client(effort.to_string())?)),
            Backend::Xai => {
                let (mut grok, _) = Grok::client().await?;
                if !effort.trim().is_empty() {
                    grok.effort = effort.to_string();
                }
                Ok(Self::Grok(grok))
            }
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

    pub async fn complete(
        &self,
        req: CompleteReq<'_>,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        match self {
            Self::Grok(g) => g.complete(req, on_delta, on_think).await,
            Self::Vertex(v) => v.complete(req, on_delta, on_think).await,
        }
    }

    pub async fn complete_with_effort(
        &self,
        req: CompleteReq<'_>,
        effort: &str,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        match self {
            Self::Grok(g) => g.complete_with_effort(req, effort, on_delta, on_think).await,
            Self::Vertex(v) => v.complete_with_effort(req, effort, on_delta, on_think).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_pick_the_backend() {
        assert_eq!(backend_for("grok-4.7"), Backend::Xai);
        assert_eq!(backend_for("gemini-2.5-pro"), Backend::Vertex);
        assert_eq!(backend_for("google/gemini-2.5-flash"), Backend::Vertex);
        assert_eq!(backend_for("gemini-4-argon"), Backend::Vertex);
        assert_eq!(backend_for("vertex/gemini-2.5-pro"), Backend::Vertex);
        assert_eq!(wire_model("google/gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(wire_model("gemini-3.5-flash"), "gemini-3.5-flash");
        assert!(model_catalog().iter().any(|c| c.provider == Backend::Vertex));
        assert!(model_catalog().iter().all(|c| c.provider != Backend::Xai || c.id.contains("grok")));
    }
}
