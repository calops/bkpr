use super::*;

pub(super) struct SendInput {
    pub to: String,
    pub text: String,
    pub idempotency_key: String,
    pub scope: AgentScope,
    pub delivery: AgentDelivery,
    pub receiver_ready: bool,
}
pub(super) struct SendPayload<'a> {
    pub to: &'a str,
    pub text: &'a str,
    pub scope: AgentScope,
    pub delivery: AgentDelivery,
}
pub(super) fn retry(
    store: &Store,
    id: &str,
    input: SendPayload<'_>,
    idempotency_key: &str,
    token: &str,
) -> Result<Option<Value>> {
    let SendPayload {
        to,
        text,
        scope,
        delivery,
    } = input;
    Agents::require_live(store, id)?;
    ensure!(
        !idempotency_key.is_empty() && idempotency_key.len() <= 256,
        "invalid idempotency key"
    );
    ensure!(
        !text.trim().is_empty() && text.len() <= 65536 && !text.contains(token),
        "invalid message text"
    );
    let key = hash(idempotency_key.as_bytes());
    if let Some(m) = store
        .messages
        .values()
        .find(|m| m.from == id && m.key == key)
    {
        ensure!(
            m.digest == payload(to, text, scope) && m.delivery == delivery,
            "idempotency key conflicts with original recipient, text, scope or delivery"
        );
        return Ok(Some(receipt(m, true)));
    }
    Ok(None)
}
pub(super) fn send(
    store: &mut Store,
    id: &str,
    own: &ContextFact,
    contexts: &BTreeMap<String, ContextFact>,
    input: SendInput,
    token: &str,
) -> Result<Value> {
    if let Some(receipt) = retry(
        store,
        id,
        SendPayload {
            to: &input.to,
            text: &input.text,
            scope: input.scope,
            delivery: input.delivery,
        },
        &input.idempotency_key,
        token,
    )? {
        return Ok(receipt);
    }
    let SendInput {
        to,
        text,
        idempotency_key,
        scope,
        delivery,
        receiver_ready,
    } = input;
    ensure!(to != id, "cannot message self");
    require_scope(scope, own, store)?;
    if let Some(retired) = store
        .agents
        .get(&to)
        .filter(|agent| agent.exited_at.is_some())
    {
        let successors = contexts
            .values()
            .filter(|candidate| {
                candidate.launch_id != id
                    && candidate.pane.id == retired.pane_id
                    && candidate.active
                    && store
                        .agents
                        .get(&candidate.launch_id)
                        .is_some_and(|agent| agent.exited_at.is_none())
                    && in_scope(scope, own, candidate, store)
            })
            .map(|candidate| candidate.launch_id.as_str())
            .take(2)
            .collect::<Vec<_>>();
        if let [successor] = successors.as_slice() {
            bail!(
                "recipient launch is retired; current same-pane launch_id={successor}; rediscover and explicitly address a new message; nothing was redirected or sent"
            );
        }
        bail!(
            "recipient launch is retired; rediscover within the selected scope; nothing was sent"
        );
    }
    let recipient = contexts.get(&to).context("recipient is not a live agent")?;
    Agents::require_live(store, &to)?;
    ensure!(
        in_scope(scope, own, recipient, store),
        "recipient outside selected live scope"
    );
    ensure!(
        store
            .messages
            .values()
            .filter(|m| m.to == to && pending(m))
            .count()
            < MAX_INBOX,
        "recipient inbox full"
    );
    store.sequence = store
        .sequence
        .checked_add(1)
        .context("message sequence exhausted")?;
    let n = (delivery == AgentDelivery::Notify)
        .then(|| store.agents[&to].notification.as_ref())
        .flatten();
    let digest = payload(&to, &text, scope);
    let message = Message {
        id: Uuid::new_v4().to_string(),
        from: id.to_owned(),
        to,
        text: Some(Arc::new(text)),
        key: hash(idempotency_key.as_bytes()),
        digest,
        scope,
        delivery,
        sequence: store.sequence,
        created_at: now(),
        read_at: None,
        acknowledged_at: None,
        expired_at: None,
        notification_id: n.map(|n| n.id.clone()),
        generation: n.map(|n| n.generation),
        receiver_id: n.map(|n| n.receiver_id.clone()),
        wake_claimed_at: n.map(|n| n.claimed_at),
        wake_state: if delivery == AgentDelivery::Quiet {
            "quiet".into()
        } else {
            n.map_or_else(
                || if receiver_ready { "pending" } else { "blocked" }.into(),
                |n| n.state.clone(),
            )
        },
        wake_reason: if delivery == AgentDelivery::Quiet || receiver_ready {
            n.and_then(|n| n.reason.clone())
        } else {
            n.map_or_else(
                || Some("awaiting subscribed native receiver".into()),
                |n| n.reason.clone(),
            )
        },
        wake_failure: None,
    };
    let receipt = receipt(&message, false);
    store.messages.insert(message.id.clone(), message);
    Ok(receipt)
}
pub(super) fn page_limit(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(10);
    ensure!((1..=10).contains(&limit), "limit must be between 1 and 10");
    Ok(limit)
}
pub(super) fn read(
    store: &mut Store,
    id: &str,
    after: Option<&str>,
    limit: Option<usize>,
    ack_ids: &[String],
    include_read: bool,
    timestamp: u64,
) -> Result<(Value, bool)> {
    Agents::require_live(store, id)?;
    // Only IDs explicitly processed before this call may be acknowledged here.
    validate_ack(store, id, ack_ids, true)?;
    let limit = page_limit(limit)?;
    let (acknowledgement, mut changed) = ack(store, id, ack_ids, timestamp)?;
    let ids = inbox_ids(store, id, after, limit, include_read)?;
    let mut values = Vec::with_capacity(ids.len());
    for message_id in &ids {
        let m = store.messages.get_mut(message_id).unwrap();
        changed |= m.read_at.is_none();
        m.read_at.get_or_insert(timestamp);
        values.push(inbox_message(m));
    }
    changed |= clear_read_notification(store, id);
    let mut value = json!({
        "messages":values,"has_more":false,
        "acknowledged":acknowledgement["acknowledged"]
    });
    let cursor = ids.last().map(String::as_str).or(after);
    let has_more = !inbox_ids(store, id, cursor, 1, include_read)?.is_empty();
    value["has_more"] = json!(has_more);
    if has_more {
        value["next_cursor"] = json!(cursor);
    }
    Ok((value, changed))
}
const INBOX_SNIPPET_BYTES: usize = 192;
const MESSAGE_PAGE_BYTES: usize = 2048;
const MAX_MESSAGE_PAGE_BYTES: usize = 4096;

fn body_end(text: &str, offset: usize, max_bytes: usize) -> usize {
    let mut end = offset.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn inbox_ids(
    store: &Store,
    id: &str,
    after: Option<&str>,
    limit: usize,
    include_read: bool,
) -> Result<Vec<String>> {
    let mut messages = store
        .messages
        .values()
        .filter(|m| m.to == id && pending(m) && (include_read || unread(m)))
        .collect::<Vec<_>>();
    messages.sort_unstable_by_key(|m| m.sequence);
    let start = cursor_start(&messages, store, id, after, true)?;
    Ok(messages[start..]
        .iter()
        .take(limit)
        .map(|m| m.id.clone())
        .collect())
}

fn inbox_message(m: &Message) -> Value {
    let text = m.text.as_ref().expect("pending message body");
    let end = preview_end(text, INBOX_SNIPPET_BYTES);
    let mut value = json!({
        "id":m.id,"from":m.from,"text":&text[..end],
        "body_complete":end == text.len(),"body_bytes":text.len(),
        "created_at":m.created_at,"read_at":m.read_at
    });
    if end < text.len() {
        value["next_offset"] = json!(end);
    }
    value
}

fn clear_read_notification(store: &mut Store, id: &str) -> bool {
    let Some(notification) = &store.agents[id].notification else {
        return false;
    };
    if store
        .messages
        .values()
        .any(|m| m.to == id && unread(m) && m.generation == Some(notification.generation))
    {
        return false;
    }
    store.agents.get_mut(id).unwrap().notification = None;
    true
}

pub(super) fn read_message(
    store: &mut Store,
    id: &str,
    message_id: &str,
    offset: Option<usize>,
    max_bytes: Option<usize>,
    timestamp: u64,
) -> Result<(Value, bool)> {
    Agents::require_live(store, id)?;
    let m = store
        .messages
        .get_mut(message_id)
        .context("unknown message")?;
    ensure!(m.to == id, "foreign recipient message body");
    let text = m
        .text
        .as_ref()
        .context("message body is acknowledged or expired")?;
    let offset = offset.unwrap_or(0);
    let max_bytes = max_bytes.unwrap_or(MESSAGE_PAGE_BYTES);
    ensure!(
        (4..=MAX_MESSAGE_PAGE_BYTES).contains(&max_bytes),
        "max_bytes must be between 4 and 4096"
    );
    ensure!(
        offset <= text.len() && text.is_char_boundary(offset),
        "offset must be a UTF-8 byte boundary within the body"
    );
    let end = body_end(text, offset, max_bytes);
    let end = offset + preview_end(&text[offset..end], MAX_MESSAGE_PAGE_BYTES);
    let changed = m.read_at.is_none();
    m.read_at.get_or_insert(timestamp);
    let mut value = json!({
        "id":m.id,"text":&text[offset..end],"offset":offset,
        "body_bytes":text.len(),"body_complete":offset == 0 && end == text.len(),
        "has_more":end < text.len(),"read_at":m.read_at
    });
    if end < text.len() {
        value["next_offset"] = json!(end);
    }
    Ok((value, changed | clear_read_notification(store, id)))
}

fn validate_ack(store: &Store, id: &str, ids: &[String], previously_read: bool) -> Result<()> {
    ensure!(ids.len() <= MAX_INBOX, "too many ack IDs");
    for message_id in ids {
        if let Some(m) = store.messages.get(message_id) {
            ensure!(m.to == id, "foreign recipient acknowledgement");
            ensure!(
                !previously_read || !pending(m) || m.read_at.is_some(),
                "read_inbox ack_ids must name messages read before this call"
            );
        }
    }
    Ok(())
}
pub(super) fn ack(
    store: &mut Store,
    id: &str,
    ids: &[String],
    timestamp: u64,
) -> Result<(Value, bool)> {
    Agents::require_live(store, id)?;
    validate_ack(store, id, ids, false)?;
    let mut count = 0;
    for message_id in ids {
        if let Some(m) = store.messages.get_mut(message_id)
            && m.acknowledged_at.is_none()
            && m.expired_at.is_none()
        {
            m.acknowledged_at = Some(timestamp);
            m.text = None;
            count += 1;
        }
    }
    let mut changed = count > 0;
    if !store.messages.values().any(|m| m.to == id && unread(m)) {
        changed |= store
            .agents
            .get_mut(id)
            .unwrap()
            .notification
            .take()
            .is_some();
    }
    Ok((json!({"acknowledged":count}), changed))
}
pub(super) fn outcome(
    store: &mut Store,
    id: &str,
    receiver_id: &str,
    notification_id: &str,
    generation: u64,
    outcome: AgentWakeOutcome,
    token: &str,
) -> Result<Value> {
    Agents::require_live(store, id)?;
    ensure!(
        store.messages.values().any(|m| m.to == id
            && m.receiver_id.as_deref() == Some(receiver_id)
            && m.notification_id.as_deref() == Some(notification_id)
            && m.generation == Some(generation)),
        "unknown or stale notification callback"
    );
    let (state, reason) = match outcome {
        AgentWakeOutcome::Accepted => ("accepted", None),
        AgentWakeOutcome::Failed { reason } => ("failed", Some(reason)),
        AgentWakeOutcome::Unknown { reason } => ("unknown", Some(reason)),
    };
    ensure!(
        reason.as_ref().is_none_or(|r| r.len() <= 2048
            && !r.chars().any(char::is_control)
            && !r.contains(token)),
        "invalid safe wake reason"
    );
    if let Some(n) = store
        .agents
        .get_mut(id)
        .unwrap()
        .notification
        .as_mut()
        .filter(|n| {
            n.receiver_id == receiver_id
                && n.id == notification_id
                && n.generation == generation
                && n.state != "accepted"
        })
    {
        n.state = state.into();
        n.reason = reason.clone();
    }
    for m in store.messages.values_mut().filter(|m| {
        m.to == id
            && m.receiver_id.as_deref() == Some(receiver_id)
            && m.notification_id.as_deref() == Some(notification_id)
            && m.generation == Some(generation)
            && m.wake_state != "accepted"
    }) {
        m.wake_state = state.into();
        m.wake_reason = reason.clone();
        if let Some(reason) = &reason {
            m.wake_failure = Some(WakeFailure {
                state: state.into(),
                reason: reason.clone(),
                notification_id: notification_id.into(),
                generation,
                recorded_at: now(),
            });
        }
    }
    Ok(json!({"recorded":true}))
}
pub(super) fn claim(
    store: &mut Store,
    id: &str,
    receiver_id: &str,
    timestamp: u64,
) -> Result<Option<Notification>> {
    Agents::require_live(store, id)?;
    if store.agents[id].notification.is_some() {
        return Ok(None);
    }
    let Some(message_id) = store
        .messages
        .values()
        .filter(|m| m.to == id && claimable(m))
        .min_by_key(|m| m.sequence)
        .map(|m| m.id.clone())
    else {
        return Ok(None);
    };
    let message_count = store
        .messages
        .values()
        .filter(|m| m.to == id && claimable(m))
        .count();
    let unread_count = store
        .messages
        .values()
        .filter(|m| m.to == id && unread(m))
        .count();
    let agent = store.agents.get_mut(id).unwrap();
    agent.generation = agent
        .generation
        .checked_add(1)
        .context("notification generation exhausted")?;
    let notification = Notification {
        id: message_id,
        generation: agent.generation,
        receiver_id: receiver_id.to_owned(),
        state: "pending".into(),
        reason: None,
        claimed_at: timestamp,
        message_count,
        unread_count,
    };
    // Claim is committed before exposing any native pointer. No ambiguous dispatch replay.
    for m in store
        .messages
        .values_mut()
        .filter(|m| m.to == id && claimable(m))
    {
        m.notification_id = Some(notification.id.clone());
        m.generation = Some(notification.generation);
        m.receiver_id = Some(receiver_id.to_owned());
        m.wake_state = "pending".into();
        m.wake_reason = None;
        m.wake_claimed_at = Some(timestamp);
    }
    agent.notification = Some(notification.clone());
    Ok(Some(notification))
}
pub(super) fn unchanged(store: &Store, id: &str, request: &AgentRequest) -> Result<Option<Value>> {
    match request {
        AgentRequest::AckMessage { ids } => {
            ensure!(ids.len() <= MAX_INBOX, "too many ack IDs");
            for message_id in ids {
                if let Some(m) = store.messages.get(message_id) {
                    ensure!(m.to == id, "foreign recipient acknowledgement");
                }
            }
            if ids.iter().all(|message_id| {
                store
                    .messages
                    .get(message_id)
                    .is_none_or(|m| m.acknowledged_at.is_some() || m.expired_at.is_some())
            }) && store.agents[id].notification.is_none()
            {
                return Ok(Some(json!({"acknowledged":0})));
            }
        }
        AgentRequest::ReadInbox {
            after,
            limit,
            ack_ids,
            include_read,
        } if ack_ids.is_empty() => {
            let ids = inbox_ids(
                store,
                id,
                after.as_deref(),
                page_limit(*limit)?,
                *include_read,
            )?;
            if ids
                .iter()
                .all(|message_id| store.messages[message_id].read_at.is_some())
                && store.agents[id].notification.is_none()
            {
                let values = ids
                    .iter()
                    .map(|message_id| inbox_message(&store.messages[message_id]))
                    .collect::<Vec<_>>();
                let cursor = ids.last().map(String::as_str).or(after.as_deref());
                let has_more = !inbox_ids(store, id, cursor, 1, *include_read)?.is_empty();
                let mut value = json!({"messages":values,"has_more":has_more,"acknowledged":0});
                if has_more {
                    value["next_cursor"] = json!(cursor);
                }
                return Ok(Some(value));
            }
        }
        _ => {}
    }
    Ok(None)
}
