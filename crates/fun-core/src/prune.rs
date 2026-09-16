//! Context prune policy.
//!
//! Each prune turn records **keep** (still on the wire) and **drop** (off the
//! wire). [`PruneView`] classifies the current prefix; the ledger is the history
//! the prune model and the agent both read to decide the next send.

use crate::session::{Call, Entry, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Prune when the live model context is at least this many estimated tokens.
pub const PRUNE_TOKEN_THRESHOLD: u64 = 20_000;
/// Or when remaining visible chars reach this (chars ≈ 4 tokens).
pub const PRUNE_CHAR_THRESHOLD: usize = 32_000;
/// Minimum newest messages shown to the prune model but never droppable.
/// The current user turn is also locked, even if it is longer than this.
pub const PRUNE_KEEP_TAIL: usize = 2;
/// Need this many droppable messages or the prune call never happens.
pub const PRUNE_MIN_CANDIDATES: usize = 2;
/// At most this many latest write/edit paths are restored as compact stubs.
const PRUNE_RESTORE_MAX: usize = 40;
const CHARS_PER_TOKEN: usize = 4;
const LISTING_CHAR_CAP: usize = 80_000;

/// One prune turn’s keep/drop bookkeeping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneTurn {
    /// `entries.len()` when this prune ran.
    pub at: usize,
    /// Ids still sent after this turn (prefix that stayed visible).
    pub keep: Vec<usize>,
    /// Ids newly hidden this turn.
    pub drop: Vec<usize>,
    /// Hidden write/edit ids sent as compact stubs after this turn.
    #[serde(default)]
    pub restored: Vec<usize>,
}

/// Where an index sits for one prune pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneBucket {
    /// Newest messages (current user turn, at least [`PRUNE_KEEP_TAIL`]). Never drop.
    Live,
    /// User lines, previous-turn reads/conclusions, latest writes/edits. Never drop.
    Keep,
    /// Visible, before the live tail, not Keep. The model may name these.
    Candidate,
    /// Already hidden. Not listed as candidates again.
    Hidden,
}

/// Snapshot of Live / Keep / Candidate / Hidden plus the keep/drop ledger.
pub struct PruneView<'a> {
    entries: &'a [Entry],
    hidden: &'a BTreeSet<usize>,
    live_from: usize,
    locked: BTreeSet<usize>,
    ledger: &'a [PruneTurn],
}

impl<'a> PruneView<'a> {
    pub fn new(entries: &'a [Entry], hidden: &'a BTreeSet<usize>) -> Self {
        Self::with_ledger(entries, hidden, &[])
    }

    pub fn with_ledger(
        entries: &'a [Entry],
        hidden: &'a BTreeSet<usize>,
        ledger: &'a [PruneTurn],
    ) -> Self {
        let live_from = prune_live_from(entries);
        let locked = prune_locked(entries, live_from);
        Self {
            entries,
            hidden,
            live_from,
            locked,
            ledger,
        }
    }

    pub fn live_from(&self) -> usize {
        self.live_from
    }

    pub fn bucket(&self, i: usize) -> PruneBucket {
        if i >= self.live_from {
            PruneBucket::Live
        } else if self.hidden.contains(&i) {
            PruneBucket::Hidden
        } else if self.cannot_hide(i) {
            PruneBucket::Keep
        } else {
            PruneBucket::Candidate
        }
    }

    fn cannot_hide(&self, i: usize) -> bool {
        i < self.entries.len() && (self.locked.contains(&i) || is_prune_keep(&self.entries[i]))
    }

    pub fn keep(&self) -> Vec<usize> {
        (0..self.live_from)
            .filter(|&i| self.bucket(i) == PruneBucket::Keep)
            .collect()
    }

    pub fn candidates(&self) -> Vec<usize> {
        (0..self.live_from)
            .filter(|&i| self.bucket(i) == PruneBucket::Candidate)
            .collect()
    }

    /// Text the prune model sees, plus the candidate ids it is allowed to name.
    pub fn listing(&self) -> (Vec<usize>, String) {
        let n = self.entries.len();
        let mut out = String::new();
        if !self.ledger.is_empty() {
            out.push_str("## Ledger (previous keep/drop — next send is keep minus later drop)\n");
            for t in self.ledger {
                out.push_str(&format!(
                    "@{} keep {} drop {}{}\n",
                    t.at,
                    join_ids(&t.keep),
                    join_ids(&t.drop),
                    if t.restored.is_empty() {
                        String::new()
                    } else {
                        format!(" restored {}", join_ids(&t.restored))
                    }
                ));
            }
            out.push('\n');
        }
        out.push_str("## Live (do not drop — current task)\n");
        for i in self.live_from..n {
            out.push_str(&format!("[{i}] {}\n", prune_line(&self.entries[i])));
        }
        let kept = self.keep();
        if !kept.is_empty() {
            out.push_str("\n## Keep (do not drop — user messages, latest writes/edits, previous-turn reads and conclusions)\n");
            for i in &kept {
                out.push_str(&format!("[{i}] {}\n", prune_line(&self.entries[*i])));
            }
        }
        out.push_str("\n## Candidates (may drop)\n");
        let mut ids = Vec::new();
        for i in 0..self.live_from {
            if self.bucket(i) != PruneBucket::Candidate {
                continue;
            }
            ids.push(i);
            out.push_str(&format!("[{i}] {}\n", prune_line(&self.entries[i])));
            if out.len() > LISTING_CHAR_CAP {
                out.push_str("… (listing truncated)\n");
                break;
            }
        }
        (ids, out)
    }

    /// Hide named candidates (plus tool pairs). Live / Keep stay.
    pub fn apply(&self, drop: &[usize]) -> BTreeSet<usize> {
        let extra = expand_hidden(self.entries, drop, self.live_from);
        let mut hidden = self.hidden.clone();
        for i in extra {
            if self.cannot_hide(i) {
                continue;
            }
            hidden.insert(i);
        }
        hidden.retain(|&i| i < self.live_from && i < self.entries.len() && !self.cannot_hide(i));
        restore_tool_pairs(self.entries, &mut hidden);
        hidden
    }

    /// Snapshot keep/drop for this apply, including compact restore ids.
    pub fn record(&self, hidden: &BTreeSet<usize>) -> PruneTurn {
        let added: Vec<usize> = hidden.difference(self.hidden).copied().collect();
        record_turn(self.entries.len(), hidden, added, prune_restored(self.entries, hidden))
    }
}

fn join_ids(ids: &[usize]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Fold the ledger: anything ever dropped stays off the wire (users stripped elsewhere).
pub fn hidden_from_ledger(ledger: &[PruneTurn]) -> BTreeSet<usize> {
    let mut hidden = BTreeSet::new();
    for t in ledger {
        hidden.extend(&t.drop);
    }
    hidden
}

/// Ids the next model call should send: last keep, plus messages after that turn,
/// minus later drops. Empty ledger → send everything.
pub fn send_ids(n: usize, ledger: &[PruneTurn]) -> BTreeSet<usize> {
    if let Some(last) = ledger.last() {
        let mut send: BTreeSet<usize> = last.keep.iter().copied().collect();
        send.extend(last.at..n);
        for t in ledger {
            for i in &t.drop {
                send.remove(i);
            }
        }
        send
    } else {
        (0..n).collect()
    }
}

pub fn record_turn(
    at: usize,
    hidden: &BTreeSet<usize>,
    drop: Vec<usize>,
    restored: HashMap<usize, Entry>,
) -> PruneTurn {
    let mut keep: Vec<usize> = (0..at).filter(|i| !hidden.contains(i)).collect();
    keep.sort_unstable();
    let mut drop = drop;
    drop.sort_unstable();
    drop.dedup();
    let mut restored: Vec<usize> = restored.into_keys().collect();
    restored.sort_unstable();
    PruneTurn {
        at,
        keep,
        drop,
        restored,
    }
}

pub fn entry_chars(e: &Entry) -> usize {
    match e {
        Entry::User { text } => text.len(),
        Entry::Assistant {
            text,
            thinking,
            calls,
        } => {
            text.len()
                + thinking.len()
                + calls
                    .iter()
                    .map(|c| c.name.len() + c.args.to_string().len())
                    .sum::<usize>()
        }
        Entry::Tool(t) => t.content.len() + t.name.len(),
    }
}

pub fn estimate_tokens(chars: usize) -> u64 {
    (chars / CHARS_PER_TOKEN) as u64
}

pub fn sanitize_hidden(entries: &[Entry], hidden: &BTreeSet<usize>) -> BTreeSet<usize> {
    hidden
        .iter()
        .copied()
        .filter(|&i| i < entries.len() && !is_prune_keep(&entries[i]))
        .collect()
}

/// Payload for the next complete: last keep ∪ later messages − later drop,
/// plus users and restore stubs. Empty ledger sends everything.
pub fn model_entries(entries: &[Entry], ledger: &[PruneTurn]) -> Vec<Entry> {
    let hidden = sanitize_hidden(entries, &hidden_from_ledger(ledger));
    let send = send_ids(entries.len(), ledger);
    let restored = prune_restored(entries, &hidden);
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if send.contains(&i) || is_prune_keep(e) {
                return Some(e.clone());
            }
            restored.get(&i).cloned()
        })
        .collect()
}

fn payload_from_hidden(entries: &[Entry], hidden: &BTreeSet<usize>) -> Vec<Entry> {
    let restored = prune_restored(entries, hidden);
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            if !hidden.contains(&i) || is_prune_keep(e) {
                return Some(e.clone());
            }
            restored.get(&i).cloned()
        })
        .collect()
}

/// First index of the live tail: the current user turn, at least `PRUNE_KEEP_TAIL` messages.
pub fn prune_live_from(entries: &[Entry]) -> usize {
    let n = entries.len();
    let min_tail = n.saturating_sub(PRUNE_KEEP_TAIL);
    let last_user = entries
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| matches!(e, Entry::User { .. }))
        .map(|(i, _)| i);
    match last_user {
        Some(i) => i.min(min_tail),
        None => min_tail,
    }
}

fn call_path<'a>(c: &'a Call, name: &str) -> Option<&'a str> {
    if c.name != name {
        return None;
    }
    c.args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn read_path(c: &Call) -> Option<&str> {
    call_path(c, "read")
}

fn mutation_path(c: &Call) -> Option<&str> {
    call_path(c, "write").or_else(|| call_path(c, "edit"))
}

fn compact_call(c: &Call) -> Call {
    let mut args = serde_json::Map::new();
    if let Some(path) = c.args.get("path").and_then(|v| v.as_str()) {
        args.insert("path".into(), Value::String(path.to_string()));
    }
    args.insert("restored".into(), Value::Bool(true));
    Call {
        id: c.id.clone(),
        name: c.name.clone(),
        args: Value::Object(args),
    }
}

fn compact_restored(e: &Entry, keep_ids: &HashSet<String>) -> Entry {
    match e {
        Entry::Assistant { text, calls, .. } => Entry::Assistant {
            text: clip_entry(text, 200).to_string(),
            thinking: String::new(),
            calls: calls
                .iter()
                .filter(|c| keep_ids.contains(&c.id))
                .map(compact_call)
                .collect(),
        },
        Entry::Tool(t) => Entry::Tool(ToolResult {
            id: t.id.clone(),
            name: t.name.clone(),
            content: clip_entry(&t.content, 160).to_string(),
            is_error: t.is_error,
        }),
        other => other.clone(),
    }
}

/// Latest successful write/edit per path, even if prune hid the fat call.
/// Payload gets a compact stub (path only) so a later review still sees that
/// work and does not delete it as "dead code".
pub fn prune_restored(entries: &[Entry], hidden: &BTreeSet<usize>) -> HashMap<usize, Entry> {
    let latest = latest_mutations(entries, entries.len());
    let keep_ids: HashSet<String> = latest.iter().map(|(_, _, id)| id.clone()).collect();
    let mut out = HashMap::new();
    for (asst, tool, _) in latest {
        for idx in [asst, tool] {
            if hidden.contains(&idx)
                && !is_prune_keep(&entries[idx])
                && let Some(e) = entries.get(idx)
            {
                out.insert(idx, compact_restored(e, &keep_ids));
            }
        }
    }
    out
}

/// User prompts always stay. Old tool-less conclusions are droppable so a
/// later constraint (“only diagrams 1–3”) is not contradicted by an earlier “17 diagrams”.
pub fn is_prune_keep(e: &Entry) -> bool {
    matches!(e, Entry::User { .. })
}

fn is_prune_conclusion(e: &Entry) -> bool {
    matches!(e, Entry::Assistant { calls, .. } if calls.is_empty())
}

/// Tool-less assistant replies in the previous completed turn.
pub fn prune_protected_conclusions(entries: &[Entry], until: usize) -> BTreeSet<usize> {
    let (start, until) = previous_turn_range(entries, until);
    (start..until)
        .filter(|&i| is_prune_conclusion(&entries[i]))
        .collect()
}

fn latest_mutations(entries: &[Entry], until: usize) -> Vec<(usize, usize, String)> {
    let until = until.min(entries.len());
    let mut path_by_id: HashMap<String, String> = HashMap::new();
    let mut last: HashMap<String, (usize, usize, String)> = HashMap::new();
    for (i, e) in entries.iter().enumerate().take(until) {
        match e {
            Entry::Assistant { calls, .. } => {
                for c in calls {
                    if let Some(path) = mutation_path(c) {
                        path_by_id.insert(c.id.clone(), path.to_string());
                    }
                }
            }
            Entry::Tool(t)
                if !t.is_error && !t.id.is_empty() && (t.name == "write" || t.name == "edit") =>
            {
                if let Some(path) = path_by_id.get(&t.id) {
                    let asst = (0..i)
                        .rev()
                        .find(|&j| match &entries[j] {
                            Entry::Assistant { calls, .. } => calls.iter().any(|c| c.id == t.id),
                            _ => false,
                        })
                        .unwrap_or(i);
                    last.insert(path.clone(), (asst, i, t.id.clone()));
                }
            }
            _ => {}
        }
    }
    let mut latest: Vec<(usize, usize, String)> = last.into_values().collect();
    latest.sort_unstable_by_key(|(asst, tool, _)| (*tool, *asst));
    if latest.len() > PRUNE_RESTORE_MAX {
        latest.drain(0..latest.len() - PRUNE_RESTORE_MAX);
    }
    latest
}

/// Latest successful write/edit per path. Never suffix-drop these — that is
/// what made “open again” wipe the origami engine.
pub fn prune_protected_mutations(entries: &[Entry], until: usize) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    for (asst, tool, _) in latest_mutations(entries, until) {
        out.insert(asst);
        out.insert(tool);
    }
    out
}

fn prune_locked(entries: &[Entry], until: usize) -> BTreeSet<usize> {
    let mut out = prune_protected_reads(entries, until);
    out.extend(prune_protected_mutations(entries, until));
    out.extend(prune_protected_conclusions(entries, until));
    out
}

/// Previous completed turn: after the user message before `until`, up to `until`.
fn previous_turn_range(entries: &[Entry], until: usize) -> (usize, usize) {
    let until = until.min(entries.len());
    let prev_user = entries
        .iter()
        .enumerate()
        .take(until)
        .rev()
        .find(|(_, e)| matches!(e, Entry::User { .. }))
        .map(|(i, _)| i);
    let start = prev_user.map(|i| i.saturating_add(1)).unwrap_or(0);
    (start, until)
}

/// Latest successful `read` of each path in the previous turn (and that tool-call group).
/// Older unique reads are droppable — keeping every crop/path forever refills context
/// and the agent forgets the user's later constraints.
pub fn prune_protected_reads(entries: &[Entry], until: usize) -> BTreeSet<usize> {
    let (start, until) = previous_turn_range(entries, until);
    let mut path_by_id: HashMap<String, String> = HashMap::new();
    let mut last_id: HashMap<String, String> = HashMap::new();
    for e in entries.iter().take(until).skip(start) {
        match e {
            Entry::Assistant { calls, .. } => {
                for c in calls {
                    if let Some(path) = read_path(c) {
                        path_by_id.insert(c.id.clone(), path.to_string());
                    }
                }
            }
            Entry::Tool(t) if t.name == "read" && !t.is_error && !t.id.is_empty() => {
                if let Some(path) = path_by_id.get(&t.id) {
                    last_id.insert(path.clone(), t.id.clone());
                }
            }
            _ => {}
        }
    }
    if last_id.is_empty() {
        return BTreeSet::new();
    }
    let latest: HashSet<String> = last_id.into_values().collect();
    let mut protect_ids: HashSet<String> = HashSet::new();
    let mut out = BTreeSet::new();
    for (i, e) in entries.iter().enumerate().take(until).skip(start) {
        if let Entry::Assistant { calls, .. } = e
            && calls.iter().any(|c| latest.contains(&c.id))
        {
            out.insert(i);
            for c in calls {
                if !c.id.is_empty() {
                    protect_ids.insert(c.id.clone());
                }
            }
        }
    }
    for (i, e) in entries.iter().enumerate().take(until).skip(start) {
        if let Entry::Tool(t) = e
            && protect_ids.contains(&t.id)
        {
            out.insert(i);
        }
    }
    out
}

fn restore_tool_pairs(entries: &[Entry], hidden: &mut BTreeSet<usize>) {
    loop {
        let n = hidden.len();
        let mut live_ids: HashSet<String> = HashSet::new();
        for (i, e) in entries.iter().enumerate() {
            if hidden.contains(&i) {
                continue;
            }
            match e {
                Entry::Assistant { calls, .. } => {
                    for c in calls {
                        if !c.id.is_empty() {
                            live_ids.insert(c.id.clone());
                        }
                    }
                }
                Entry::Tool(t) if !t.id.is_empty() => {
                    live_ids.insert(t.id.clone());
                }
                _ => {}
            }
        }
        for (i, e) in entries.iter().enumerate() {
            match e {
                Entry::Assistant { calls, .. } => {
                    if calls.iter().any(|c| live_ids.contains(&c.id)) {
                        hidden.remove(&i);
                    }
                }
                Entry::Tool(t) if live_ids.contains(&t.id) => {
                    hidden.remove(&i);
                }
                _ => {}
            }
        }
        if hidden.len() == n {
            break;
        }
    }
}

pub fn should_prune(
    entries: &[Entry],
    hidden: &BTreeSet<usize>,
    last_input_tokens: u64,
) -> Option<usize> {
    let view = PruneView::new(entries, hidden);
    let droppable = view.candidates().len();
    if droppable < PRUNE_MIN_CANDIDATES {
        return None;
    }
    let model: usize = payload_from_hidden(entries, hidden)
        .iter()
        .map(entry_chars)
        .sum();
    let tokens = last_input_tokens.max(estimate_tokens(model));
    if tokens < PRUNE_TOKEN_THRESHOLD && model < PRUNE_CHAR_THRESHOLD {
        return None;
    }
    Some(droppable)
}

pub fn prune_inspect(entries: &[Entry], hidden: &BTreeSet<usize>) -> Vec<String> {
    prune_inspect_ids(entries, hidden.iter().copied())
}

pub fn prune_inspect_ids(entries: &[Entry], ids: impl IntoIterator<Item = usize>) -> Vec<String> {
    let mut ids: Vec<usize> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|i| entries.get(i).map(|e| format!("[{i}] {}", prune_line(e))))
        .collect()
}

fn prune_line(e: &Entry) -> String {
    match e {
        Entry::User { text } => format!("user {}", clip_entry(text, 240)),
        Entry::Assistant { text, calls, .. } => {
            if calls.is_empty() {
                format!("assistant {}", clip_entry(text, 240))
            } else {
                let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                format!(
                    "assistant tools {} {}",
                    names.join(","),
                    clip_entry(text, 120)
                )
            }
        }
        Entry::Tool(t) => {
            let flag = if t.is_error { "error" } else { "ok" };
            format!("tool {} ({flag}) {}", t.name, clip_entry(&t.content, 200))
        }
    }
}

/// Expand a drop list so assistant tool calls stay paired with their results.
pub fn expand_hidden(entries: &[Entry], drop: &[usize], protect_from: usize) -> BTreeSet<usize> {
    let mut hidden: BTreeSet<usize> = drop
        .iter()
        .copied()
        .filter(|i| *i < entries.len() && *i < protect_from)
        .collect();
    let mut by_id: HashSet<String> = HashSet::new();
    for i in &hidden {
        if let Entry::Assistant { calls, .. } = &entries[*i] {
            for c in calls {
                if !c.id.is_empty() {
                    by_id.insert(c.id.clone());
                }
            }
        }
        if let Entry::Tool(t) = &entries[*i]
            && !t.id.is_empty()
        {
            by_id.insert(t.id.clone());
        }
    }
    if by_id.is_empty() {
        return hidden;
    }
    for (i, e) in entries.iter().enumerate() {
        if i >= protect_from {
            break;
        }
        match e {
            Entry::Assistant { calls, .. } => {
                if calls.iter().any(|c| by_id.contains(&c.id)) {
                    hidden.insert(i);
                }
            }
            Entry::Tool(t) if by_id.contains(&t.id) => {
                hidden.insert(i);
            }
            _ => {}
        }
    }
    hidden
}

pub fn parse_drop_ids(text: &str, allowed: &[usize]) -> Vec<usize> {
    let allow: HashSet<usize> = allowed.iter().copied().collect();
    let mut found = BTreeSet::new();
    let mut n = String::new();
    let push = |n: &mut String, found: &mut BTreeSet<usize>| {
        if let Ok(i) = n.parse::<usize>()
            && allow.contains(&i)
        {
            found.insert(i);
        }
        n.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() {
            n.push(c);
        } else {
            push(&mut n, &mut found);
        }
    }
    push(&mut n, &mut found);
    found.into_iter().collect()
}

fn clip_entry(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{empty_args, Call, Entry, ToolResult};
    use std::collections::BTreeSet;

    fn user(s: &str) -> Entry {
        Entry::User { text: s.into() }
    }

    fn assistant(s: &str) -> Entry {
        Entry::Assistant {
            text: s.into(),
            thinking: String::new(),
            calls: Vec::new(),
        }
    }

    fn assistant_call(id: &str, name: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: name.into(),
                args: empty_args(),
            }],
        }
    }

    fn assistant_read(id: &str, path: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: "read".into(),
                args: serde_json::json!({"path": path}),
            }],
        }
    }

    fn tool(id: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: "read".into(),
            content: content.into(),
            is_error: false,
        })
    }

    #[test]
    fn should_prune_needs_tokens_and_progress() {
        let none = BTreeSet::new();
        let small = vec![
            user("a"),
            assistant("b"),
            user("c"),
            assistant("d"),
            user("e"),
            assistant("f"),
        ];
        assert!(should_prune(&small, &none, 1_000).is_none());
        let fat = vec![
            user("a"),
            assistant_call("c1", "bash"),
            tool("c1", &"x".repeat(40_000)),
            user("now"),
            assistant("ok"),
            user("next"),
            assistant("still"),
        ];
        assert!(should_prune(&fat, &none, 1_000).is_some());
        let hidden: BTreeSet<usize> = (0..fat.len().saturating_sub(2)).collect();
        assert!(should_prune(&fat, &hidden, 30_000).is_none());
    }

    #[test]
    fn model_entries_skip_hidden() {
        let entries = vec![
            user("old"),
            assistant_call("c1", "bash"),
            tool("c1", "noise"),
            user("new"),
            assistant("b"),
        ];
        let out = payload_from_hidden(&entries, &BTreeSet::new());
        assert_eq!(out.len(), 5);
        let mut hidden = BTreeSet::new();
        hidden.insert(1);
        hidden.insert(2);
        let out = payload_from_hidden(&entries, &hidden);
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], Entry::User { text } if text == "old"));
        assert!(matches!(&out[1], Entry::User { text } if text == "new"));
        assert!(matches!(&out[2], Entry::Assistant { text, .. } if text == "b"));
    }

    fn assistant_write(id: &str, path: &str, content: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: "write".into(),
                args: serde_json::json!({"path": path, "content": content}),
            }],
        }
    }

    fn tool_named(id: &str, name: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: name.into(),
            content: content.into(),
            is_error: false,
        })
    }

    #[test]
    fn model_entries_restores_hidden_writes() {
        let entries = vec![
            user("make okta work"),
            assistant_write("w1", "src/login.rs", &"fn okta() {}".repeat(200)),
            tool_named("w1", "write", "wrote 4000 bytes to src/login.rs"),
            assistant_call("c1", "bash"),
            tool("c1", &"noise".repeat(1000)),
            user("final code review"),
            assistant("ok"),
        ];
        let hidden = BTreeSet::from([1, 2, 3, 4]);
        let out = payload_from_hidden(&entries, &hidden);
        assert!(matches!(&out[0], Entry::User { text } if text == "make okta work"));
        assert!(
            matches!(&out[1], Entry::Assistant { calls, .. } if calls.len() == 1 && calls[0].name == "write"),
            "latest write stub comes back"
        );
        if let Entry::Assistant { calls, thinking, .. } = &out[1] {
            assert!(thinking.is_empty());
            assert_eq!(calls[0].args["path"], "src/login.rs");
            assert_eq!(calls[0].args["restored"], true);
            assert!(calls[0].args.get("content").is_none());
        }
        assert!(matches!(&out[2], Entry::Tool(t) if t.name == "write"));
        assert!(!out.iter().any(|e| matches!(e, Entry::Assistant { calls, .. } if calls.iter().any(|c| c.name == "bash"))));
        assert!(out.iter().any(|e| matches!(e, Entry::User { text } if text == "final code review")));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn model_entries_restores_only_latest_write_per_path() {
        let entries = vec![
            user("v1"),
            assistant_write("w1", "src/login.rs", "old"),
            tool_named("w1", "write", "wrote old"),
            user("v2"),
            assistant_write("w2", "src/login.rs", "new okta"),
            tool_named("w2", "write", "wrote new"),
            user("review"),
        ];
        let hidden = BTreeSet::from([1, 2, 4, 5]);
        let out = payload_from_hidden(&entries, &hidden);
        let writes: Vec<&str> = out
            .iter()
            .filter_map(|e| match e {
                Entry::Tool(t) if t.name == "write" => Some(t.content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(writes, vec!["wrote new"]);
    }

    #[test]
    fn model_entries_still_sends_wrongly_hidden_users() {
        let entries = vec![
            user("goal"),
            assistant_call("c1", "bash"),
            tool("c1", "noise"),
            user("draw assets"),
            assistant("ok"),
        ];
        let hidden = BTreeSet::from([0, 1, 2, 3]);
        let out = payload_from_hidden(&entries, &hidden);
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], Entry::User { text } if text == "goal"));
        assert!(matches!(&out[1], Entry::User { text } if text == "draw assets"));
        assert!(matches!(&out[2], Entry::Assistant { text, .. } if text == "ok"));
        let cleaned = sanitize_hidden(&entries, &hidden);
        assert_eq!(cleaned, BTreeSet::from([1, 2]));
    }

    #[test]
    fn parse_drop_and_expand_tool_pairs() {
        let ids = parse_drop_ids("{\"drop\":[0, 2]}", &[0, 1, 2, 3]);
        assert_eq!(ids, vec![0, 2]);
        let entries = vec![
            user("old"),
            assistant_call("c1", "read"),
            tool("c1", "file"),
            user("next"),
        ];
        let hidden = expand_hidden(&entries, &[1], entries.len());
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&2));
        assert!(!hidden.contains(&0));
        let protected = expand_hidden(&entries, &[1], 1);
        assert!(protected.is_empty());
    }

    #[test]
    fn prune_listing_shows_live_and_candidates() {
        let entries = vec![
            user("old goal"),
            assistant_call("c1", "bash"),
            tool("c1", "file"),
            user("also run clippy"),
            assistant("working"),
        ];
        let (ids, listing) = PruneView::new(&entries, &BTreeSet::new()).listing();
        assert_eq!(ids, vec![1, 2]);
        assert!(listing.contains("## Live"));
        assert!(listing.contains("[3] user also run clippy"));
        assert!(listing.contains("## Candidates"));
        assert!(listing.contains("[0] user old goal"));
        assert!(listing.contains("## Keep"));
        assert!(!ids.contains(&0), "user messages stay in Keep");
        assert!(!ids.contains(&3));
        assert!(!ids.contains(&4));
    }

    #[test]
    fn prune_locks_current_user_turn() {
        let entries = vec![
            user("old"),
            assistant_call("c0", "bash"),
            user("fix it"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", &"x".repeat(100)),
            assistant_read("c2", "src/b.rs"),
            tool("c2", &"y".repeat(100)),
        ];
        assert_eq!(prune_live_from(&entries), 2);
        let (ids, listing) = PruneView::new(&entries, &BTreeSet::new()).listing();
        assert_eq!(ids, vec![1]);
        assert!(listing.contains("[2] user fix it"));
        assert!(!ids.contains(&0), "user messages stay in Keep");
        assert!(!ids.contains(&3));
        assert!(!ids.contains(&4));
        assert!(!ids.contains(&5));
        assert!(!ids.contains(&6));
    }

    #[test]
    fn prune_keeps_latest_read_per_path() {
        let entries = vec![
            user("look"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", "old a"),
            assistant_read("c2", "src/a.rs"),
            tool("c2", "new a"),
            assistant_read("c3", "src/b.rs"),
            tool("c3", "b"),
            user("continue"),
            assistant("working"),
        ];
        let (ids, listing) = PruneView::new(&entries, &BTreeSet::new()).listing();
        assert!(!ids.contains(&0), "user messages stay in Keep");
        assert!(ids.contains(&1), "older unique read of a.rs is droppable");
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3), "previous-turn read of a.rs stays");
        assert!(!ids.contains(&4));
        assert!(!ids.contains(&5), "previous-turn read of b.rs stays");
        assert!(!ids.contains(&6));
        assert!(listing.contains("## Keep"));
        assert!(listing.contains("[3]"));
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1, 2, 3, 4, 5, 6]);
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&2));
        assert!(!hidden.contains(&3));
        assert!(!hidden.contains(&4));
        assert!(!hidden.contains(&5));
        assert!(!hidden.contains(&6));
        assert!(!hidden.contains(&0));
        assert!(!hidden.contains(&7));
        assert!(!hidden.contains(&8));
    }

    #[test]
    fn apply_prune_drops_named_candidates_only() {
        let entries = vec![
            user("goal"),
            assistant("plan"),
            assistant_call("c1", "bash"),
            tool("c1", "fail"),
            assistant_call("c2", "bash"),
            tool("c2", "fail"),
            user("continue"),
            assistant("working"),
        ];
        // Naming 4 pairs with 5. Later/earlier candidates stay.
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[4]);
        assert!(!hidden.contains(&0));
        assert!(!hidden.contains(&1));
        assert!(!hidden.contains(&2));
        assert!(!hidden.contains(&3));
        assert!(hidden.contains(&4));
        assert!(hidden.contains(&5));
        assert!(!hidden.contains(&6));
        assert!(!hidden.contains(&7));
        let kept = payload_from_hidden(&entries, &hidden);
        assert!(matches!(&kept[0], Entry::User { text } if text == "goal"));
        assert!(matches!(&kept[1], Entry::Assistant { text, .. } if text == "plan"));
    }

    #[test]
    fn apply_prune_keeps_later_user_messages() {
        let entries = vec![
            user("goal"),
            assistant_call("c1", "bash"),
            tool("c1", "old dump"),
            user("draw assets"),
            assistant_call("c2", "bash"),
            tool("c2", "more dump"),
            user("give the cuts back"),
            assistant("ok"),
        ];
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1]);
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&2));
        assert!(!hidden.contains(&4), "later tool noise is not suffix-wiped");
        assert!(!hidden.contains(&5));
        assert!(!hidden.contains(&0));
        assert!(!hidden.contains(&3), "later user constraint stays");
        assert!(!hidden.contains(&6));
        assert!(!hidden.contains(&7));
    }

    fn assistant_edit(id: &str, path: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: "edit".into(),
                args: serde_json::json!({"path": path, "old": "a", "new": "b"}),
            }],
        }
    }

    fn listing_buckets(
        entries: &[Entry],
        hidden: &BTreeSet<usize>,
    ) -> (usize, Vec<usize>, Vec<usize>) {
        let view = PruneView::new(entries, hidden);
        (view.live_from(), view.keep(), view.candidates())
    }

    fn origami() -> Vec<Entry> {
        vec![
            user("17 diagrams"),
            assistant("crane lesson is 17 diagrams"),
            assistant_write("w1", "src/catalog.rs", "seventeen"),
            tool_named("w1", "write", "wrote catalog"),
            user("only keep 1-3"),
            assistant("only tsuru 1-3"),
            assistant_edit("e1", "src/fold.rs"),
            tool_named("e1", "edit", "edited fold.rs"),
            assistant_call("b1", "bash"),
            tool("b1", &"x".repeat(40_000)),
            user("open again"),
            assistant("viewer open"),
        ]
    }

    #[test]
    fn apply_prune_does_not_suffix_wipe_later_edits() {
        let entries = vec![
            user("17 diagrams"),
            assistant("crane lesson is 17 diagrams"),
            assistant_write("w1", "src/catalog.rs", "seventeen"),
            tool_named("w1", "write", "wrote catalog"),
            user("only keep 1-3"),
            assistant("only tsuru 1-3"),
            assistant_edit("e1", "src/fold.rs"),
            tool_named("e1", "edit", "edited fold.rs"),
            assistant_call("b1", "bash"),
            tool("b1", &"x".repeat(40_000)),
            user("open again"),
            assistant("viewer open"),
        ];
        let (ids, listing) = PruneView::new(&entries, &BTreeSet::new()).listing();
        assert!(ids.contains(&1), "old conclusion is droppable");
        assert!(!ids.contains(&5), "previous-turn conclusion stays");
        assert!(!ids.contains(&6), "latest edit stays");
        assert!(!ids.contains(&7));
        assert!(ids.contains(&8));
        assert!(ids.contains(&9));
        assert!(listing.contains("## Keep"));
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1, 8]);
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&8));
        assert!(hidden.contains(&9));
        assert!(!hidden.contains(&6), "later engine edit survives an early drop");
        assert!(!hidden.contains(&7));
        assert!(!hidden.contains(&2), "latest write of catalog.rs is locked");
        assert!(!hidden.contains(&3));
    }

    #[test]
    fn prune_does_not_protect_stale_unique_reads() {
        let entries = vec![
            user("look"),
            assistant_read("c1", "old.png"),
            tool("c1", "old crop"),
            user("now this"),
            assistant_read("c2", "new.png"),
            tool("c2", "new crop"),
            user("continue"),
            assistant("working"),
        ];
        let (ids, _) = PruneView::new(&entries, &BTreeSet::new()).listing();
        assert!(ids.contains(&1), "stale unique read is a candidate");
        assert!(ids.contains(&2));
        assert!(!ids.contains(&4), "previous-turn read stays");
        assert!(!ids.contains(&5));
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1]);
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&2));
        assert!(!hidden.contains(&3));
        assert!(!hidden.contains(&4));
        assert!(!hidden.contains(&5));
    }

    #[test]
    fn should_prune_ignores_protected_reads() {
        let none = BTreeSet::new();
        let entries = vec![
            user("goal"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", &"a".repeat(40_000)),
            user("next"),
            assistant("still"),
        ];
        assert!(should_prune(&entries, &none, 30_000).is_none());
        let fat = vec![
            user("goal"),
            assistant_call("c0", "bash"),
            tool("c0", "noise"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", &"a".repeat(40_000)),
            user("next"),
            assistant("still"),
        ];
        assert!(should_prune(&fat, &none, 30_000).is_some());
    }

    #[test]
    fn prune_inspect_lists_hidden() {
        let entries = vec![user("goal"), assistant("dump"), user("next")];
        let hidden = BTreeSet::from([1]);
        let lines = prune_inspect(&entries, &hidden);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("[1] assistant"));
    }

    #[test]
    fn prune_buckets_cover_prefix_without_overlap() {
        let entries = origami();
        let hidden = BTreeSet::new();
        let view = PruneView::new(&entries, &hidden);
        assert_eq!(view.bucket(10), PruneBucket::Live);
        assert_eq!(view.bucket(11), PruneBucket::Live);
        assert_eq!(view.bucket(0), PruneBucket::Keep);
        assert_eq!(view.bucket(5), PruneBucket::Keep);
        assert_eq!(view.bucket(1), PruneBucket::Candidate);
        assert_eq!(view.bucket(8), PruneBucket::Candidate);
        let (live_from, keep, candidates) = listing_buckets(&entries, &hidden);
        assert_eq!(live_from, 10);
        assert_eq!(keep, vec![0, 2, 3, 4, 5, 6, 7]);
        assert_eq!(candidates, vec![1, 8, 9]);
        let mut seen = BTreeSet::new();
        for i in keep.iter().chain(candidates.iter()).copied() {
            assert!(i < live_from);
            assert!(seen.insert(i), "index {i} in two buckets");
        }
        for i in 0..live_from {
            assert!(seen.contains(&i), "index {i} missing from keep+candidates");
        }
        for i in live_from..entries.len() {
            assert!(!candidates.contains(&i));
            assert!(!keep.contains(&i));
        }
    }

    #[test]
    fn prune_listing_sections_match_buckets() {
        let entries = origami();
        let hidden = BTreeSet::new();
        let (live_from, keep, candidates) = listing_buckets(&entries, &hidden);
        let listing = PruneView::new(&entries, &hidden).listing().1;
        let live_block = listing.split("## Keep").next().unwrap();
        let keep_block = listing
            .split("## Keep")
            .nth(1)
            .unwrap()
            .split("## Candidates")
            .next()
            .unwrap();
        let cand_block = listing.split("## Candidates").nth(1).unwrap();
        for i in live_from..entries.len() {
            assert!(live_block.contains(&format!("[{i}]")));
            assert!(!keep_block.contains(&format!("[{i}]")));
            assert!(!cand_block.contains(&format!("[{i}]")));
        }
        for i in &keep {
            assert!(keep_block.contains(&format!("[{i}]")));
            assert!(!cand_block.contains(&format!("[{i}]")));
        }
        for i in &candidates {
            assert!(cand_block.contains(&format!("[{i}]")));
            assert!(!keep_block.contains(&format!("[{i}]")));
        }
    }

    #[test]
    fn apply_prune_only_hides_named_candidates_and_pairs() {
        let entries = origami();
        let (live_from, keep, candidates) = listing_buckets(&entries, &BTreeSet::new());
        assert!(candidates.contains(&1));
        assert!(candidates.contains(&8));
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1, 8, live_from, keep[0]]);
        for i in &keep {
            assert!(!hidden.contains(i), "keep {i} must stay");
        }
        for i in live_from..entries.len() {
            assert!(!hidden.contains(&i), "live {i} must stay");
        }
        assert!(hidden.contains(&1));
        assert!(hidden.contains(&8));
        assert!(hidden.contains(&9), "tool pair of 8");
        assert!(!hidden.contains(&2));
    }

    #[test]
    fn already_hidden_ids_are_neither_keep_nor_candidates() {
        let entries = origami();
        let already = BTreeSet::from([1, 8, 9]);
        let view = PruneView::new(&entries, &already);
        assert_eq!(view.bucket(1), PruneBucket::Hidden);
        assert_eq!(view.bucket(8), PruneBucket::Hidden);
        assert_eq!(view.bucket(9), PruneBucket::Hidden);
        let (live_from, keep, candidates) = listing_buckets(&entries, &already);
        assert!(!candidates.contains(&1));
        assert!(!candidates.contains(&8));
        assert!(!candidates.contains(&9));
        assert!(!keep.contains(&1));
        assert_eq!(live_from, 10);
        let listing = PruneView::new(&entries, &already).listing().1;
        let cand_block = listing.split("## Candidates").nth(1).unwrap();
        assert!(!cand_block.contains("[1]"));
        assert!(!cand_block.contains("[8]"));
    }

    #[test]
    fn should_prune_counts_candidates_not_keep() {
        let none = BTreeSet::new();
        let only_keep = vec![
            user("goal"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", &"a".repeat(40_000)),
            user("next"),
            assistant("still"),
        ];
        let (_, keep, candidates) = listing_buckets(&only_keep, &none);
        assert!(candidates.is_empty());
        assert!(!keep.is_empty());
        assert!(should_prune(&only_keep, &none, 30_000).is_none());

        let fat = vec![
            user("goal"),
            assistant_call("c0", "bash"),
            tool("c0", "noise"),
            assistant_read("c1", "src/a.rs"),
            tool("c1", &"a".repeat(40_000)),
            user("next"),
            assistant("still"),
        ];
        let (_, _, candidates) = listing_buckets(&fat, &none);
        assert!(candidates.len() >= 2);
        assert_eq!(should_prune(&fat, &none, 30_000), Some(candidates.len()));
    }

    #[test]
    fn parse_drop_ids_ignores_live_and_keep_digits() {
        let entries = origami();
        let (_, keep, candidates) = listing_buckets(&entries, &BTreeSet::new());
        let drop = parse_drop_ids("drop 0, 1, 5, 8, 10, 99", &candidates);
        assert_eq!(drop, vec![1, 8]);
        assert!(!drop.iter().any(|i| keep.contains(i)));
    }

    #[test]
    fn ledger_records_keep_and_drop_each_turn() {
        let entries = origami();
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1, 8]);
        let turn = record_turn(
            entries.len(),
            &hidden,
            hidden.iter().copied().collect(),
            prune_restored(&entries, &hidden),
        );
        assert_eq!(turn.at, entries.len());
        assert_eq!(turn.drop, vec![1, 8, 9]);
        assert!(!turn.keep.contains(&1));
        assert!(!turn.keep.contains(&8));
        assert!(turn.keep.contains(&0));
        assert!(turn.keep.contains(&2));
        assert!(turn.keep.contains(&10));
        let send = send_ids(entries.len(), &[turn.clone()]);
        assert!(!send.contains(&1));
        assert!(!send.contains(&8));
        assert!(send.contains(&0));
        assert!(send.contains(&10));
        assert_eq!(hidden_from_ledger(&[turn]), hidden);
    }

    #[test]
    fn later_turn_keep_plus_new_messages_minus_later_drop() {
        let mut entries = vec![
            user("goal"),
            assistant_call("c0", "bash"),
            tool("c0", "noise"),
            user("next"),
            assistant("still"),
        ];
        let h1 = PruneView::new(&entries, &BTreeSet::new()).apply(&[1]);
        let t1 = record_turn(entries.len(), &h1, vec![1, 2], prune_restored(&entries, &h1));
        assert_eq!(t1.drop, vec![1, 2]);
        assert_eq!(t1.keep, vec![0, 3, 4]);

        entries.push(assistant_call("c1", "bash"));
        entries.push(tool("c1", "more"));
        entries.push(user("again"));
        entries.push(assistant("working"));
        let h2 = PruneView::new(&entries, &h1).apply(&[5]);
        let t2 = record_turn(
            entries.len(),
            &h2,
            h2.difference(&h1).copied().collect(),
            prune_restored(&entries, &h2),
        );
        assert_eq!(t2.drop, vec![5, 6]);
        let send = send_ids(entries.len(), &[t1.clone(), t2.clone()]);
        assert!(!send.contains(&1));
        assert!(!send.contains(&2));
        assert!(!send.contains(&5));
        assert!(!send.contains(&6));
        assert!(send.contains(&0));
        assert!(send.contains(&3));
        assert!(send.contains(&7));
        assert!(send.contains(&8));
        let from_hidden = payload_from_hidden(&entries, &h2);
        let from_ledger = model_entries(&entries, &[t1, t2]);
        assert_eq!(from_hidden.len(), from_ledger.len());
    }

    #[test]
    fn listing_shows_previous_keep_drop() {
        let entries = origami();
        let hidden = PruneView::new(&entries, &BTreeSet::new()).apply(&[1, 8]);
        let turn = record_turn(
            entries.len(),
            &hidden,
            hidden.iter().copied().collect(),
            prune_restored(&entries, &hidden),
        );
        let listing = PruneView::with_ledger(&entries, &hidden, &[turn]).listing().1;
        assert!(listing.contains("## Ledger"));
        assert!(listing.contains("keep "));
        assert!(listing.contains("drop "));
    }

    #[test]
    fn restore_stubs_are_on_the_ledger() {
        let entries = vec![
            user("make okta"),
            assistant_write("w1", "src/login.rs", "okta"),
            tool_named("w1", "write", "wrote okta"),
            assistant_call("c1", "bash"),
            tool("c1", "noise"),
            user("review"),
            assistant("done"),
        ];
        let hidden = BTreeSet::from([1, 2, 3, 4]);
        let restored = prune_restored(&entries, &hidden);
        assert!(restored.contains_key(&1));
        assert!(restored.contains_key(&2));
        assert!(!restored.contains_key(&3));
        let turn = record_turn(entries.len(), &hidden, vec![1, 2, 3, 4], restored);
        assert_eq!(turn.restored, vec![1, 2]);
        let send = model_entries(&entries, &[turn]);
        assert!(send.iter().any(
            |e| matches!(e, Entry::Assistant { calls, .. } if calls.iter().any(|c| c.name == "write"))
        ));
        assert!(!send.iter().any(
            |e| matches!(e, Entry::Assistant { calls, .. } if calls.iter().any(|c| c.name == "bash"))
        ));
    }
}
