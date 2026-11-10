//! Launch/conversation-bound presentation pages, not durable operation receipts.
//! Hashlink supplies maintained ordered storage; expiry follows LRU order without a timer.
use anyhow::{Context, Result, ensure};
use hashlink::LinkedHashMap;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(super) const INLINE_BYTES: usize = 8 * 1024;
const CACHE_BYTES: usize = 32 * 1024 * 1024;
const PAGE_BYTES: usize = 2048;
const TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::harness) struct Owner {
    launch: String,
    active: bool,
    conversation: Option<(String, String, u64)>,
}
impl Owner {
    pub(in crate::harness) fn from_identity(identity: &Value) -> Result<Self> {
        let launch = identity["launch_id"]
            .as_str()
            .context("missing authenticated launch identity")?
            .to_owned();
        let conversation = identity
            .get("conversation")
            .filter(|value| !value.is_null())
            .map(|value| {
                Ok::<_, anyhow::Error>((
                    value["id"]
                        .as_str()
                        .context("missing native conversation ID")?
                        .to_owned(),
                    value["source"]
                        .as_str()
                        .context("missing native conversation source")?
                        .to_owned(),
                    value["generation"]
                        .as_u64()
                        .context("missing native conversation generation")?,
                ))
            })
            .transpose()?;
        Ok(Self {
            launch,
            active: identity["attached"]
                .as_bool()
                .context("missing live attachment fact")?,
            conversation,
        })
    }
    fn bytes(&self) -> usize {
        self.launch.capacity()
            + self
                .conversation
                .as_ref()
                .map_or(0, |(id, source, _)| id.capacity() + source.capacity())
    }
}
#[derive(Clone, Copy)]
struct Outcome {
    error: bool,
    unknown: bool,
}
struct Cached {
    owner: Owner,
    text: Arc<String>,
    outcome: Outcome,
    expires: Instant,
    weight: usize,
}
#[derive(Default)]
struct State {
    entries: LinkedHashMap<String, Cached>,
    bytes: usize,
}
impl State {
    fn expire(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|(_, entry)| entry.expires <= now)
        {
            self.evict();
        }
    }
    fn evict(&mut self) {
        if let Some((_, entry)) = self.entries.pop_front() {
            self.bytes -= entry.weight;
        }
    }
}
pub(in crate::harness) struct Pages {
    state: Mutex<State>,
    limit: usize,
    ttl: Duration,
}
impl Default for Pages {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            limit: CACHE_BYTES,
            ttl: TTL,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    result_id: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    max_bytes: Option<usize>,
}
struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= self.limit)
            .ok_or_else(|| std::io::Error::other("MCP presentation byte allowance exceeded"))?;
        if length > self.bytes.capacity() {
            let capacity = length
                .checked_next_power_of_two()
                .unwrap_or(self.limit)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn envelope(value: Value, text: String, error: bool) -> Value {
    json!({"content":[{"type":"text","text":text}],"structuredContent":value,"isError":error})
}
fn small(value: Value, error: bool) -> Value {
    let text = value.to_string();
    debug_assert!(text.len() <= INLINE_BYTES);
    envelope(value, text, error)
}
fn summary(value: &Value) -> Value {
    let mut summary = serde_json::Map::new();
    for key in [
        "pane_id",
        "window_id",
        "workspace_id",
        "message_id",
        "revision",
        "registry_revision",
        "window_revision",
        "workspace_revision",
        "accepted",
        "durable",
        "closed",
        "duplicate",
    ] {
        if let Some(value) = value.get(key)
            && (value.is_boolean()
                || value.is_number()
                || value.as_str().is_some_and(|text| text.len() <= 256))
        {
            summary.insert(key.into(), value.clone());
        }
    }
    if let Some(created) = value.get("created_panes").and_then(Value::as_object) {
        summary.insert("created_panes_count".into(), json!(created.len()));
    }
    Value::Object(summary)
}
impl Pages {
    pub(super) fn present(
        &self,
        result: Result<Value>,
        origin: Option<&Owner>,
        authenticate: impl FnOnce() -> Result<Owner>,
    ) -> Value {
        self.state.lock().unwrap().expire(Instant::now());
        let (value, outcome) = match result {
            Ok(value) => (
                value,
                Outcome {
                    error: false,
                    unknown: false,
                },
            ),
            Err(error) => {
                let unknown = super::super::outcome_unknown(&error);
                (
                    json!({"error":format!("{error:#}"),"outcome_unknown":unknown,"replayed":false}),
                    Outcome {
                        error: true,
                        unknown,
                    },
                )
            }
        };
        let preview = summary(&value);
        let mut encoded = BoundedJson {
            bytes: Vec::new(),
            limit: self.limit.saturating_sub(2048),
        };
        let serialized = serde_json::to_writer(&mut encoded, &value)
            .map_err(anyhow::Error::from)
            .and_then(|()| String::from_utf8(encoded.bytes).map_err(Into::into));
        let text = match serialized {
            Ok(text) => text,
            Err(_) => {
                return Self::unavailable(
                    preview,
                    outcome,
                    "result exceeds bounded presentation storage; inspect narrower targets",
                );
            }
        };
        if text.len() <= INLINE_BYTES {
            return envelope(value, text, outcome.error);
        }
        drop(value);
        let owner = match authenticate() {
            Ok(owner) if owner.active && origin == Some(&owner) => owner,
            _ => {
                return Self::unavailable(
                    preview,
                    outcome,
                    "result expansion authorization is no longer live",
                );
            }
        };
        let now = Instant::now();
        let result_id = format!("result:{}", uuid::Uuid::new_v4());
        let weight = text
            .capacity()
            .saturating_add(owner.bytes())
            .saturating_add(result_id.capacity())
            .saturating_add(std::mem::size_of::<Cached>() + 256);
        let bytes = text.len();
        let mut state = self.state.lock().unwrap();
        state.expire(now);
        if weight > self.limit {
            return Self::unavailable(
                preview,
                outcome,
                "result exceeds bounded presentation storage; inspect narrower targets",
            );
        }
        while state.bytes > self.limit - weight {
            state.evict();
        }
        if state.entries.try_reserve(1).is_err() {
            return Self::unavailable(preview, outcome, "result presentation memory unavailable");
        }
        state.entries.insert(
            result_id.clone(),
            Cached {
                owner,
                text: Arc::new(text),
                outcome,
                expires: now + self.ttl,
                weight,
            },
        );
        state.bytes += weight;
        drop(state);
        small(
            json!({"result_id":result_id,"paged":true,"bytes":bytes,"next_offset":0,"read_with":"read_result","summary":preview,"original_is_error":outcome.error,"outcome_unknown":outcome.unknown,"replayed":false}),
            outcome.error,
        )
    }
    fn unavailable(preview: Value, outcome: Outcome, reason: &str) -> Value {
        small(
            json!({"error":reason,"presentation_failed":true,"tool_completed":!outcome.error,"original_is_error":outcome.error,"outcome_unknown":outcome.unknown,"replayed":false,"summary":preview,"guidance":"Do not repeat a mutation to recover its output. Inspect existing targets instead."}),
            true,
        )
    }
    pub(in crate::harness) fn read(&self, owner: &Owner, arguments: Value) -> Result<Value> {
        let arguments: Read =
            serde_json::from_value(arguments).context("expected {result_id,offset?,max_bytes?}")?;
        self.read_at(owner, arguments, Instant::now())
    }
    fn read_at(&self, owner: &Owner, arguments: Read, now: Instant) -> Result<Value> {
        ensure!(
            owner.active,
            "result expansion requires a foreground-attached launch"
        );
        let max = arguments.max_bytes.unwrap_or(PAGE_BYTES);
        ensure!((4..=PAGE_BYTES).contains(&max), "max_bytes must be 4..2048");
        let (text, outcome) = {
            let mut state = self.state.lock().unwrap();
            state.expire(now);
            let entry = state.entries.get(&arguments.result_id).context(
                "result expired or unavailable; inspect existing targets, do not replay mutations",
            )?;
            ensure!(
                &entry.owner == owner,
                "result belongs to another launch or native conversation"
            );
            ensure!(
                arguments.offset <= entry.text.len()
                    && entry.text.is_char_boundary(arguments.offset),
                "offset is not a valid UTF-8 result boundary"
            );
            let entry = state
                .entries
                .to_back(&arguments.result_id)
                .expect("checked presentation entry");
            entry.expires = now + self.ttl;
            (entry.text.clone(), entry.outcome)
        };
        let end = text.floor_char_boundary(arguments.offset.saturating_add(max).min(text.len()));
        let next = (end < text.len()).then_some(end);
        Ok(
            json!({"result_id":arguments.result_id,"offset":arguments.offset,"text":&text[arguments.offset..end],"next_offset":next,"bytes":text.len(),"original_is_error":outcome.error,"outcome_unknown":outcome.unknown}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(generation: u64) -> Owner {
        Owner {
            launch: "launch".into(),
            active: true,
            conversation: Some(("native".into(), "fixture".into(), generation)),
        }
    }
    #[test]
    fn large_results_expand_losslessly_at_utf8_boundaries_without_changing_error_outcome() {
        for error in [false, true] {
            let pages = Pages::default();
            let value = json!({"payload":"🦀\u{0}\\\"".repeat(4000)});
            let result = if error {
                Err(super::super::super::request_error(value.to_string(), true))
            } else {
                Ok(value.clone())
            };
            let presented = pages.present(result, Some(&owner(1)), || Ok(owner(1)));
            assert_eq!(presented["isError"], error);
            assert!(presented["content"][0]["text"].as_str().unwrap().len() <= INLINE_BYTES);
            let id = presented["structuredContent"]["result_id"]
                .as_str()
                .unwrap();
            let mut offset = 0;
            let mut restored = String::new();
            loop {
                let page = pages
                    .read(
                        &owner(1),
                        json!({"result_id":id,"offset":offset,"max_bytes":17}),
                    )
                    .unwrap();
                assert!(page.to_string().len() <= INLINE_BYTES);
                assert_eq!(page["outcome_unknown"], error);
                restored.push_str(page["text"].as_str().unwrap());
                match page["next_offset"].as_u64() {
                    Some(next) => offset = next as usize,
                    None => break,
                }
            }
            let restored: Value = serde_json::from_str(&restored).unwrap();
            if error {
                assert_eq!(restored["error"], value.to_string());
                assert_eq!(restored["outcome_unknown"], true);
            } else {
                assert_eq!(restored, value);
            }
        }
    }
    #[test]
    fn expired_evicted_and_rebound_results_are_not_resurrected_or_authorized() {
        let pages = Pages {
            state: Mutex::default(),
            limit: 24 * 1024,
            ttl: Duration::from_secs(10),
        };
        let first = pages.present(
            Ok(json!({"data":"a".repeat(14000)})),
            Some(&owner(1)),
            || Ok(owner(1)),
        );
        let first_id = first["structuredContent"]["result_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            pages
                .read(&owner(2), json!({"result_id":first_id}))
                .is_err()
        );
        let second = pages.present(
            Ok(json!({"data":"b".repeat(14000)})),
            Some(&owner(1)),
            || Ok(owner(1)),
        );
        let second_id = second["structuredContent"]["result_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(first_id, second_id);
        assert!(
            pages
                .read(&owner(1), json!({"result_id":first_id}))
                .is_err()
        );
        assert!(
            pages
                .read(&owner(1), json!({"result_id":second_id,"offset":1}))
                .unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("data")
        );
        assert!(
            pages
                .read_at(
                    &owner(1),
                    Read {
                        result_id: second_id,
                        offset: 0,
                        max_bytes: None
                    },
                    Instant::now() + Duration::from_secs(11)
                )
                .is_err()
        );
    }
    #[test]
    fn presentation_failure_does_not_turn_a_completed_mutation_into_an_unknown_or_replayed_one() {
        let pages = Pages {
            state: Mutex::default(),
            limit: 16 * 1024,
            ttl: TTL,
        };
        let reply = pages.present(
            Ok(json!({"pane_id":"created-pane","data":"x".repeat(32000)})),
            Some(&owner(1)),
            || Ok(owner(1)),
        );
        let result = &reply["structuredContent"];
        assert_eq!(result["presentation_failed"], true);
        assert_eq!(result["tool_completed"], true);
        assert_eq!(result["outcome_unknown"], false);
        assert_eq!(result["replayed"], false);
        assert_eq!(result["summary"]["pane_id"], "created-pane");
    }
    #[test]
    fn rebind_during_dispatch_cannot_assign_previous_output_to_the_new_conversation() {
        let pages = Pages::default();
        let reply = pages.present(
            Ok(json!({"pane_id":"completed-pane","private":"old conversation data".repeat(1000)})),
            Some(&owner(1)),
            || Ok(owner(2)),
        );
        assert_eq!(reply["isError"], true);
        assert_eq!(reply["structuredContent"]["presentation_failed"], true);
        assert_eq!(reply["structuredContent"]["tool_completed"], true);
        assert_eq!(reply["structuredContent"]["outcome_unknown"], false);
        assert!(reply["structuredContent"].get("result_id").is_none());
        assert!(
            !reply["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("old conversation data")
        );
    }
    #[test]
    fn cached_output_is_not_available_to_a_background_launch_with_the_same_binding() {
        let pages = Pages::default();
        let reply = pages.present(
            Ok(json!({"context":"private".repeat(2000)})),
            Some(&owner(1)),
            || Ok(owner(1)),
        );
        let id = reply["structuredContent"]["result_id"].as_str().unwrap();
        let mut inactive = owner(1);
        inactive.active = false;
        assert!(pages.read(&inactive, json!({"result_id":id})).is_err());
        let rejected = pages.present(
            Ok(json!({"context":"private".repeat(2000)})),
            Some(&inactive),
            || Ok(inactive.clone()),
        );
        assert_eq!(rejected["structuredContent"]["presentation_failed"], true);
        assert!(rejected["structuredContent"].get("result_id").is_none());
    }
}
