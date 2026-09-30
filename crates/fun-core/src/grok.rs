use crate::session::{Call, Entry, empty_args};
use crate::tool::{Abort, Tool, aborted, is_abort};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::time::Duration;

const STREAM_IDLE: Duration = Duration::from_secs(90);

fn auth_err(e: anyhow::Error) -> anyhow::Error {
    if provider_grok::is_not_logged_in(&e) {
        e.context("not logged in — `fun login`")
    } else {
        e
    }
}

fn env_or(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.into())
}

fn reasoning_effort() -> String {
    match env_or("FUN_CODING_AGENT_EFFORT", "medium")
        .to_ascii_lowercase()
        .as_str()
    {
        "low" | "minimal" => "low".into(),
        "high" | "xhigh" | "x-high" => "high".into(),
        _ => "medium".into(),
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<WirePartOut>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WirePartOut {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireInput {
    Message {
        role: &'static str,
        content: WireContent,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

#[derive(Serialize)]
struct WireProp {
    #[serde(rename = "type")]
    kind: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct WireParams {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: BTreeMap<String, WireProp>,
    required: Vec<&'static str>,
    #[serde(rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'static str,
    description: &'static str,
    parameters: WireParams,
}

#[derive(Serialize)]
struct WireReasoning<'a> {
    effort: &'a str,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a [WireInput],
    tools: &'a [WireTool],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<WireReasoning<'a>>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    status: String,
    incomplete_details: Option<WireIncomplete>,
    #[serde(default)]
    output: Vec<WireOutput>,
    output_text: Option<String>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    input_tokens_details: WireInputDetails,
    #[serde(default)]
    output_tokens_details: WireOutputDetails,
}

#[derive(Deserialize, Default)]
struct WireInputDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Deserialize, Default)]
struct WireOutputDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

#[derive(Deserialize)]
struct WireIncomplete {
    reason: Option<String>,
}

#[derive(Deserialize)]
struct WireOutput {
    #[serde(rename = "type")]
    kind: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
    content: Option<Vec<WirePart>>,
}

#[derive(Deserialize)]
struct WirePart {
    text: Option<String>,
}

pub struct Reply {
    pub text: String,
    pub thinking: String,
    pub calls: Vec<Call>,
    pub truncated: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
}

fn to_input(e: &Entry) -> Vec<WireInput> {
    match e {
        Entry::User { text, images } => vec![WireInput::Message {
            role: "user",
            content: user_content(text, images),
        }],
        Entry::Assistant { text, calls, .. } => {
            let mut out = Vec::new();
            if !text.is_empty() {
                out.push(WireInput::Message {
                    role: "assistant",
                    content: WireContent::Text(text.clone()),
                });
            }
            for c in calls {
                out.push(WireInput::FunctionCall {
                    kind: "function_call",
                    call_id: c.id.clone(),
                    name: c.name.clone(),
                    arguments: serde_json::to_string(&c.args).unwrap_or_else(|_| "{}".into()),
                });
            }
            out
        }
        Entry::Tool(t) => vec![WireInput::FunctionCallOutput {
            kind: "function_call_output",
            call_id: t.id.clone(),
            output: t.content.clone(),
        }],
    }
}

fn user_content(text: &str, images: &[crate::session::Image]) -> WireContent {
    if images.is_empty() {
        return WireContent::Text(text.to_string());
    }
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(WirePartOut::InputText {
            text: text.to_string(),
        });
    }
    for image in images {
        parts.push(WirePartOut::InputImage {
            image_url: format!("data:{};base64,{}", image.media_type(), image.data),
        });
    }
    if parts.is_empty() {
        return WireContent::Text(String::new());
    }
    WireContent::Parts(parts)
}

fn tool_def(tool: &Tool) -> WireTool {
    let mut properties = BTreeMap::new();
    let mut required = Vec::new();
    for f in tool.properties {
        properties.insert(
            f.name.into(),
            WireProp {
                kind: f.r#type,
                description: f.description,
            },
        );
        if f.required {
            required.push(f.name);
        }
    }
    WireTool {
        kind: "function",
        name: tool.name,
        description: tool.description,
        parameters: WireParams {
            kind: "object",
            properties,
            required,
            additional_properties: false,
        },
    }
}

#[derive(Clone)]
pub struct Grok {
    base_url: String,
    http: reqwest::Client,
    pub effort: String,
}

impl Grok {
    pub async fn client() -> Result<(Self, String)> {
        let base_url = env_or("FUN_CODING_AGENT_BASE_URL", "https://api.x.ai/v1");
        let model = env_or("FUN_CODING_AGENT_MODEL", "grok-4.7");
        let effort = reasoning_effort();
        Ok((
            Self {
                base_url,
                http: reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(15))
                    .tcp_nodelay(true)
                    .tcp_keepalive(Duration::from_secs(20))
                    .pool_idle_timeout(Duration::from_secs(30))
                    .read_timeout(STREAM_IDLE)
                    .build()
                    .context("http client")?,
                effort,
            },
            model,
        ))
    }

    pub async fn from_env() -> Result<(Self, String)> {
        provider_grok::bearer().await.map_err(auth_err)?;
        Self::client().await
    }

    pub async fn complete(
        &self,
        model: &str,
        system: &str,
        entries: &[Entry],
        tools: &[Tool],
        abort: &Abort,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<Reply> {
        self.complete_with_effort(
            model,
            system,
            entries,
            tools,
            abort,
            &self.effort,
            on_delta,
            on_think,
        )
        .await
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
        mut on_delta: impl FnMut(&str),
        mut on_think: impl FnMut(&str),
    ) -> Result<Reply> {
        let mut delay = Duration::from_millis(250);
        let mut started = false;
        let mut attempt = 0u32;
        loop {
            match self
                .stream_once(
                    model,
                    system,
                    entries,
                    tools,
                    abort,
                    effort,
                    &mut started,
                    &mut on_delta,
                    &mut on_think,
                )
                .await
            {
                Ok(r) => return Ok(r),
                Err(e) if is_abort(&e) => return Err(e),
                Err(e) => {
                    attempt += 1;
                    if attempt >= 3 || started || !is_transient_stream(&e) {
                        return Err(e);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = abort.wait() => return Err(aborted()),
                    }
                    delay *= 2;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_once(
        &self,
        model: &str,
        system: &str,
        entries: &[Entry],
        tools: &[Tool],
        abort: &Abort,
        effort: &str,
        started: &mut bool,
        on_delta: &mut impl FnMut(&str),
        on_think: &mut impl FnMut(&str),
    ) -> Result<Reply> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let input: Vec<WireInput> = entries.iter().flat_map(to_input).collect();
        let defs: Vec<WireTool> = tools.iter().map(tool_def).collect();
        let body = WireRequest {
            model,
            instructions: system,
            input: &input,
            tools: &defs,
            stream: true,
            reasoning: Some(WireReasoning { effort }),
        };
        let mut token = provider_grok::bearer().await.map_err(auth_err)?;
        let send = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .header("Accept", "text/event-stream")
            .header("Accept-Encoding", "identity")
            .json(&body)
            .send();
        let mut resp = tokio::select! {
            r = send => r.context("grok request")?,
            _ = abort.wait() => return Err(aborted()),
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let refreshed = provider_grok::force_refresh(Some(&token))
                .await
                .map_err(auth_err)?;
            if refreshed == token {
                bail!(
                    "grok {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
            token = refreshed;
            let retry = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .header("Accept", "text/event-stream")
                .header("Accept-Encoding", "identity")
                .json(&body)
                .send();
            resp = tokio::select! {
                r = retry => r.context("grok request")?,
                _ = abort.wait() => return Err(aborted()),
            };
        }
        let status = resp.status();
        if !status.is_success() {
            bail!("grok {}: {}", status, resp.text().await.unwrap_or_default());
        }
        let mut buf = String::new();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut think_cb = |d: &str| {
            thinking.push_str(d);
            on_think(d);
        };
        let mut calls: Vec<PartialCall> = Vec::new();
        let mut truncated = false;
        let mut final_reply: Option<Reply> = None;
        loop {
            let chunk = tokio::select! {
                c = resp.chunk() => c,
                _ = abort.wait() => return Err(aborted()),
            };
            let chunk = match chunk {
                Ok(c) => c,
                Err(e)
                    if is_unclean_eof(&e)
                        && has_stream_content(final_reply.as_ref(), &text, &calls) =>
                {
                    break;
                }
                Err(e) => return Err(e).context("grok stream"),
            };
            let Some(chunk) = chunk else {
                break;
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            drain_sse(
                &mut buf,
                &mut SseState {
                    text: &mut text,
                    calls: &mut calls,
                    truncated: &mut truncated,
                    final_reply: &mut final_reply,
                    started,
                    on_delta,
                    on_think: &mut think_cb,
                },
            );
        }
        if !buf.trim().is_empty() {
            buf.push_str("\n\n");
            drain_sse(
                &mut buf,
                &mut SseState {
                    text: &mut text,
                    calls: &mut calls,
                    truncated: &mut truncated,
                    final_reply: &mut final_reply,
                    started,
                    on_delta,
                    on_think: &mut think_cb,
                },
            );
        }
        let streamed = Reply {
            text,
            thinking,
            calls: calls
                .into_iter()
                .map(|c| Call {
                    id: c.call_id,
                    name: c.name,
                    args: serde_json::from_str(&c.arguments).unwrap_or_else(|_| empty_args()),
                })
                .collect(),
            truncated,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        Ok(merge_reply(final_reply, streamed))
    }
}

fn is_unclean_eof(err: &reqwest::Error) -> bool {
    let msg = format!("{err:#}").to_ascii_lowercase();
    msg.contains("unexpected eof")
        || msg.contains("error reading a body")
        || msg.contains("error decoding response body")
        || msg.contains("connection reset")
        || msg.contains("connection closed")
        || msg.contains("broken pipe")
        || msg.contains("incomplete message")
}

fn is_transient_stream(err: &anyhow::Error) -> bool {
    if is_abort(err) {
        return false;
    }
    let msg = format!("{err:#}").to_ascii_lowercase();
    if msg.contains("not logged in") {
        return false;
    }
    if let Some(rest) = msg.strip_prefix("grok ")
        && rest.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return rest.starts_with('5');
    }
    msg.contains("error decoding response body")
        || msg.contains("error reading a body")
        || msg.contains("unexpected eof")
        || msg.contains("connection reset")
        || msg.contains("connection closed")
        || msg.contains("broken pipe")
        || msg.contains("timed out")
        || msg.contains("error sending request")
        || msg.contains("connection error")
        || msg.contains("grok stream")
        || msg.contains("grok request")
}

fn has_stream_content(final_reply: Option<&Reply>, text: &str, calls: &[PartialCall]) -> bool {
    final_reply.is_some() || !text.is_empty() || calls.iter().any(|c| !c.name.is_empty())
}

fn merge_reply(final_reply: Option<Reply>, streamed: Reply) -> Reply {
    let Some(mut r) = final_reply else {
        return streamed;
    };
    if r.text.is_empty() && !streamed.text.is_empty() {
        r.text = streamed.text;
    }
    if r.thinking.is_empty() && !streamed.thinking.is_empty() {
        r.thinking = streamed.thinking;
    }
    if r.calls.is_empty() && !streamed.calls.is_empty() {
        r.calls = streamed.calls;
    }
    r.truncated |= streamed.truncated;
    if r.input_tokens == 0 {
        r.input_tokens = streamed.input_tokens;
    }
    if r.output_tokens == 0 {
        r.output_tokens = streamed.output_tokens;
    }
    if r.cached_tokens == 0 {
        r.cached_tokens = streamed.cached_tokens;
    }
    if r.reasoning_tokens == 0 {
        r.reasoning_tokens = streamed.reasoning_tokens;
    }
    r
}

struct PartialCall {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

struct SseState<'a> {
    text: &'a mut String,
    calls: &'a mut Vec<PartialCall>,
    truncated: &'a mut bool,
    final_reply: &'a mut Option<Reply>,
    started: &'a mut bool,
    on_delta: &'a mut dyn FnMut(&str),
    on_think: &'a mut dyn FnMut(&str),
}

fn drain_sse(buf: &mut String, s: &mut SseState<'_>) {
    if buf.contains('\r') {
        *buf = buf.replace("\r\n", "\n").replace('\r', "\n");
    }
    while let Some((event, data)) = take_sse(buf) {
        if data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let typ = if event.is_empty() {
            v.get("type").and_then(|t| t.as_str()).unwrap_or("")
        } else {
            event.as_str()
        };
        apply_sse(typ, &v, s);
        if !s.text.is_empty() || s.calls.iter().any(|c| !c.name.is_empty()) {
            *s.started = true;
        }
    }
}

fn take_sse(buf: &mut String) -> Option<(String, String)> {
    loop {
        let sep = buf.find("\n\n")?;
        let block = buf[..sep].to_string();
        buf.replace_range(..sep + 2, "");
        let mut event = String::new();
        let mut data = String::new();
        for line in block.lines() {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().into();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(v.trim_start());
            } else if line.starts_with('{') {
                data = line.into();
            }
        }
        if !data.is_empty() {
            return Some((event, data));
        }
    }
}

fn is_think_delta(typ: &str) -> bool {
    let t = typ.to_ascii_lowercase();
    (t.contains("reason") || t.contains("think"))
        && (t.ends_with(".delta") || t.ends_with(".text") || t.contains("summary"))
        && !t.contains("function_call")
}

fn apply_sse(typ: &str, v: &Value, s: &mut SseState<'_>) {
    if is_think_delta(typ) {
        if let Some(d) = v
            .get("delta")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("text").and_then(|x| x.as_str()))
        {
            (s.on_think)(d);
        }
        return;
    }
    if typ.ends_with("output_text.delta") || typ.ends_with("text.delta") {
        if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
            s.text.push_str(d);
            (s.on_delta)(d);
        }
        return;
    }
    if typ.ends_with("output_item.added") || typ.ends_with("output_item.done") {
        if let Some(item) = v.get("item")
            && item.get("type").and_then(|t| t.as_str()) == Some("function_call")
        {
            let item_id = item
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !item_id.is_empty() && s.calls.iter().any(|c| c.item_id == item_id) {
                if let Some(args) = item.get("arguments").and_then(|x| x.as_str())
                    && let Some(c) = s.calls.iter_mut().find(|c| c.item_id == item_id)
                    && c.arguments.is_empty()
                {
                    c.arguments = args.into();
                }
                return;
            }
            s.calls.push(PartialCall {
                item_id,
                call_id: item
                    .get("call_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .into(),
                name: item
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .into(),
                arguments: item
                    .get("arguments")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .into(),
            });
        }
        return;
    }
    if typ.ends_with("function_call_arguments.delta") {
        if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
            let item_id = v.get("item_id").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(c) = s
                .calls
                .iter_mut()
                .rev()
                .find(|c| item_id.is_empty() || c.item_id == item_id)
            {
                c.arguments.push_str(d);
            }
        }
        return;
    }
    if typ.ends_with("completed") || typ.ends_with("incomplete") {
        if typ.ends_with("incomplete") {
            *s.truncated = true;
        }
        if let Some(resp) = v.get("response")
            && let Ok(wr) = serde_json::from_value::<WireResponse>(resp.clone())
        {
            let mut r = parse_response(wr);
            if *s.truncated {
                r.truncated = true;
            }
            if s.text.is_empty() && !r.text.is_empty() {
                (s.on_delta)(&r.text);
                s.text.clone_from(&r.text);
            }
            *s.final_reply = Some(r);
        }
    }
}

fn parse_response(v: WireResponse) -> Reply {
    let reason = v
        .incomplete_details
        .as_ref()
        .and_then(|d| d.reason.as_deref());
    let truncated = v.status == "incomplete" || reason == Some("max_output_tokens");
    let mut text = String::new();
    let mut calls = Vec::new();
    for item in v.output {
        match item.kind.as_deref() {
            Some("function_call") => calls.push(Call {
                id: item.call_id.unwrap_or_default(),
                name: item.name.unwrap_or_default(),
                args: serde_json::from_str(item.arguments.as_deref().unwrap_or("{}"))
                    .unwrap_or_else(|_| empty_args()),
            }),
            Some("message") => {
                if let Some(parts) = item.content {
                    for part in parts {
                        if let Some(t) = part.text {
                            text.push_str(&t);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if text.is_empty()
        && let Some(t) = v.output_text
    {
        text = t;
    }
    let (input_tokens, output_tokens, cached_tokens, reasoning_tokens) =
        v.usage.map_or((0, 0, 0, 0), |u| {
            let input = u.input_tokens;
            let output = if u.output_tokens > 0 {
                u.output_tokens
            } else {
                u.total_tokens.saturating_sub(input)
            };
            (
                input,
                output,
                u.input_tokens_details.cached_tokens,
                u.output_tokens_details.reasoning_tokens,
            )
        });
    Reply {
        text,
        thinking: String::new(),
        calls,
        truncated,
        input_tokens,
        output_tokens,
        cached_tokens,
        reasoning_tokens,
    }
}

pub async fn login() -> Result<()> {
    provider_grok::login().await
}

pub async fn logout() -> Result<()> {
    if provider_grok::logout().await? {
        println!("logged out");
    } else {
        println!("not logged in");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(raw: &str) -> (String, String, Vec<PartialCall>, bool) {
        let mut buf = raw.replace('\r', "");
        if !buf.ends_with("\n\n") {
            buf.push_str("\n\n");
        }
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = Vec::new();
        let mut truncated = false;
        let mut final_reply = None;
        let mut started = false;
        let mut think_cb = |d: &str| thinking.push_str(d);
        drain_sse(
            &mut buf,
            &mut SseState {
                text: &mut text,
                calls: &mut calls,
                truncated: &mut truncated,
                final_reply: &mut final_reply,
                started: &mut started,
                on_delta: &mut |_| {},
                on_think: &mut think_cb,
            },
        );
        let _ = (final_reply, started);
        (text, thinking, calls, truncated)
    }

    #[test]
    fn user_image_is_input_image_part() {
        use crate::session::Image;
        let entry = Entry::User {
            text: "what is this".into(),
            images: vec![Image {
                media: "png".into(),
                data: "AAAA".into(),
                name: String::new(),
            }],
        };
        let wire = to_input(&entry);
        let json = serde_json::to_value(&wire).unwrap();
        let content = &json[0]["content"];
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "what is this");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
        let plain = to_input(&Entry::User {
            text: "hi".into(),
            images: Vec::new(),
        });
        let plain_json = serde_json::to_value(&plain).unwrap();
        assert_eq!(plain_json[0]["content"], "hi");
    }

    #[test]
    fn sse_text_delta() {
        let (text, _, calls, trunc) =
            drain("event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n");
        assert_eq!(text, "hi");
        assert!(calls.is_empty());
        assert!(!trunc);
    }

    #[test]
    fn sse_think_delta() {
        let (_, think, _, _) =
            drain("event: response.reasoning.delta\ndata: {\"delta\":\"hmm\"}\n\n");
        assert_eq!(think, "hmm");
    }

    #[test]
    fn sse_function_call_and_args() {
        let raw = concat!(
            "event: response.output_item.added\n",
            "data: {\"item\":{\"id\":\"it1\",\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"item_id\":\"it1\",\"delta\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}\n\n",
        );
        let (_, _, calls, _) = drain(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].call_id, "c1");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn sse_incomplete_sets_truncated() {
        let raw = concat!(
            "event: response.incomplete\n",
            "data: {\"response\":{\"status\":\"incomplete\",\"output\":[],\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
        );
        let (_, _, _, trunc) = drain(raw);
        assert!(trunc);
    }

    #[test]
    fn abort_survives_context() {
        let e = aborted().context("grok stream");
        assert!(is_abort(&e));
        assert!(!is_transient_stream(&e));
    }
}
