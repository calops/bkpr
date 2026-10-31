use super::*;

pub(super) struct SendInput {
    pub to: String,
    pub text: String,
    pub idempotency_key: String,
    pub scope: AgentScope,
}
pub(super) fn retry(
    store: &Store,
    id: &str,
    to: &str,
    text: &str,
    idempotency_key: &str,
    scope: AgentScope,
    token: &str,
) -> Result<Option<Value>> {
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
            m.digest == payload(to, text, scope),
            "idempotency key conflicts with original recipient, text or scope"
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
        &input.to,
        &input.text,
        &input.idempotency_key,
        input.scope,
        token,
    )? {
        return Ok(receipt);
    }
    let SendInput {
        to,
        text,
        idempotency_key,
        scope,
    } = input;
    ensure!(to != id, "cannot message self");
    require_scope(scope, own, store)?;
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
    let n = store.agents[&to].notification.as_ref();
    let digest = payload(&to, &text, scope);
    let message = Message {
        id: Uuid::new_v4().to_string(),
        from: id.to_owned(),
        to,
        text: Some(Arc::new(text)),
        key: hash(idempotency_key.as_bytes()),
        digest,
        scope,
        sequence: store.sequence,
        created_at: now(),
        read_at: None,
        acknowledged_at: None,
        expired_at: None,
        notification_id: n.map(|n| n.id.clone()),
        generation: n.map(|n| n.generation),
        receiver_id: n.map(|n| n.receiver_id.clone()),
        wake_claimed_at: n.map(|n| n.claimed_at),
        wake_state: n.map_or_else(|| "blocked".into(), |n| n.state.clone()),
        wake_reason: n.map_or_else(
            || Some("awaiting subscribed native receiver".into()),
            |n| n.reason.clone(),
        ),
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
    timestamp: u64,
) -> Result<(Value, bool)> {
    Agents::require_live(store, id)?;
    let limit = page_limit(limit)?;
    let mut messages = store
        .messages
        .values()
        .filter(|m| m.to == id && pending(m))
        .collect::<Vec<_>>();
    messages.sort_unstable_by_key(|m| m.sequence);
    let start = cursor_start(&messages, store, id, after, true)?;
    let end = (start + limit).min(messages.len());
    let ids = messages[start..end]
        .iter()
        .map(|m| m.id.clone())
        .collect::<Vec<_>>();
    let has_more = end < messages.len();
    let mut values = Vec::with_capacity(ids.len());
    let mut changed = false;
    for message_id in &ids {
        let m = store.messages.get_mut(message_id).unwrap();
        changed |= m.read_at.is_none();
        m.read_at.get_or_insert(timestamp);
        values.push(json!({"id":m.id,"from":m.from,"to":m.to,"text":m.text,"created_at":m.created_at,"read_at":m.read_at,"untrusted":true}));
    }
    if let Some(notification) = &store.agents[id].notification {
        let remains = store
            .messages
            .values()
            .any(|m| m.to == id && unread(m) && m.generation == Some(notification.generation));
        if !remains {
            store.agents.get_mut(id).unwrap().notification = None;
            changed = true;
        }
    }
    Ok((
        json!({"messages":values,"has_more":has_more,"next_cursor":ids.last()}),
        changed,
    ))
}
pub(super) fn ack(
    store: &mut Store,
    id: &str,
    ids: &[String],
    timestamp: u64,
) -> Result<(Value, bool)> {
    Agents::require_live(store, id)?;
    ensure!(ids.len() <= MAX_INBOX, "too many ack IDs");
    for message_id in ids {
        if let Some(m) = store.messages.get(message_id) {
            ensure!(m.to == id, "foreign recipient acknowledgement");
        }
    }
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
        AgentRequest::ReadInbox { after, limit } => {
            let limit = page_limit(*limit)?;
            let mut messages = store
                .messages
                .values()
                .filter(|m| m.to == id && pending(m))
                .collect::<Vec<_>>();
            messages.sort_unstable_by_key(|m| m.sequence);
            let start = cursor_start(&messages, store, id, after.as_deref(), true)?;
            let end = (start + limit).min(messages.len());
            if messages[start..end].iter().all(|m| m.read_at.is_some())
                && store.agents[id].notification.is_none()
            {
                let values=messages[start..end].iter().map(|m|json!({"id":m.id,"from":m.from,"to":m.to,"text":m.text,"created_at":m.created_at,"read_at":m.read_at,"untrusted":true})).collect::<Vec<_>>();
                return Ok(Some(
                    json!({"messages":values,"has_more":end<messages.len(),"next_cursor":messages[start..end].last().map(|m|&m.id)}),
                ));
            }
        }
        _ => {}
    }
    Ok(None)
}
