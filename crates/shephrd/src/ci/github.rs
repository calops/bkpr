//! GitHub transport and response normalization; no provider protocol escapes this module.
use super::{BatchResponse, CiSource, FetchError, RetryPolicy, empty, same_commit};
use serde_json::{Value, json};
use shephrd::ci_contract::{
    ChangeRequest, ChangeRequestState, CiCheck, CiFetchState, CiSnapshot, CiStatus, ForgeProvider,
    ForgeRepository,
};

pub(super) fn repository(url: &str) -> Option<ForgeRepository> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if [owner, name].iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    }) {
        return None;
    }
    Some(ForgeRepository {
        provider: ForgeProvider::Github,
        host: "github.com".into(),
        path: path.into(),
        web_url: format!("https://github.com/{path}"),
    })
}

fn query(sources: &[CiSource]) -> String {
    let mut query = String::from("query { rateLimit { cost remaining resetAt } ");
    for (index, source) in sources.iter().enumerate() {
        let (owner, name) = source
            .repository
            .path
            .split_once('/')
            .expect("validated GitHub repository");
        query.push_str(&format!("r{index}:repository(owner:{owner},name:{name}) {{ object(expression:{sha}) {{ ... on Commit {{ ", owner=json!(owner), name=json!(name), sha=json!(source.sha)));
        query.push_str("oid associatedPullRequests(first:10) { nodes { number title url isDraft state updatedAt headRefOid headRefName } pageInfo { hasNextPage } } statusCheckRollup { state contexts(first:100) { nodes { __typename ... on CheckRun { name status conclusion detailsUrl startedAt completedAt } ... on StatusContext { context state targetUrl } } pageInfo { hasNextPage } } }");
        query.push_str(" } } } ");
    }
    query.push('}');
    query
}

#[cfg(target_os = "linux")]
pub(super) fn request(sources: &[CiSource]) -> std::io::Result<(std::process::Command, Vec<u8>)> {
    let mut command = std::process::Command::new("gh");
    command.args([
        "api",
        "graphql",
        "--include",
        "--hostname",
        &sources[0].repository.host,
        "--input",
        "-",
    ]);
    Ok((
        command,
        serde_json::to_vec(&json!({"query": query(sources)}))?,
    ))
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .map(|value| value.chars().take(512).collect())
}

fn status(node: &Value) -> CiStatus {
    if node["__typename"] == "StatusContext" {
        return match node["state"].as_str() {
            Some("SUCCESS") => CiStatus::Passed,
            Some("FAILURE" | "ERROR") => CiStatus::Failed,
            Some("PENDING" | "EXPECTED") => CiStatus::Pending,
            _ => CiStatus::Unknown,
        };
    }
    match node["status"].as_str() {
        Some("QUEUED" | "REQUESTED" | "WAITING" | "PENDING") => CiStatus::Pending,
        Some("IN_PROGRESS") => CiStatus::Running,
        Some("COMPLETED") => match node["conclusion"].as_str() {
            Some("SUCCESS") => CiStatus::Passed,
            Some("FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE") => {
                CiStatus::Failed
            }
            Some("CANCELLED") => CiStatus::Cancelled,
            Some("SKIPPED" | "NEUTRAL") => CiStatus::Skipped,
            _ => CiStatus::Unknown,
        },
        _ => CiStatus::Unknown,
    }
}

fn change_request(node: &Value) -> Option<ChangeRequest> {
    let state = match node["state"].as_str()? {
        "OPEN" => match node["isDraft"].as_bool()? {
            true => ChangeRequestState::Draft,
            false => ChangeRequestState::Open,
        },
        "MERGED" => ChangeRequestState::Merged,
        "CLOSED" => ChangeRequestState::Closed,
        _ => return None,
    };
    Some(ChangeRequest {
        number: node["number"].as_u64()?,
        title: text(node, "title")?,
        url: text(node, "url")?,
        state,
    })
}

fn matching_request(source: &CiSource, nodes: &[Value]) -> Option<ChangeRequest> {
    // Without branch identity an associated PR is not a verified current-branch match.
    let branch = source.branch.as_deref()?;
    let matching = || {
        nodes.iter().filter(|node| {
            node["headRefOid"].as_str() == Some(source.sha.as_str())
                && node["headRefName"].as_str() == Some(branch)
        })
    };
    let mut active = matching().filter(|node| node["state"] == "OPEN");
    if let Some(first) = active.next() {
        return active
            .next()
            .is_none()
            .then(|| change_request(first))
            .flatten();
    }
    let mut inactive =
        matching().filter(|node| matches!(node["state"].as_str(), Some("MERGED" | "CLOSED")));
    let first = inactive.next()?;
    let Some(second) = inactive.next() else {
        return change_request(first);
    };
    // Every candidate needs a provider timestamp; equal newest timestamps remain ambiguous.
    let mut latest = (first, epoch(first["updatedAt"].as_str()?)?);
    let mut tied = false;
    for node in std::iter::once(second).chain(inactive) {
        let updated = epoch(node["updatedAt"].as_str()?)?;
        if updated > latest.1 {
            latest = (node, updated);
            tied = false;
        } else if updated == latest.1 {
            tied = true;
        }
    }
    (!tied).then(|| change_request(latest.0)).flatten()
}

fn parse(source: &CiSource, repository: &Value, now: u64) -> Result<CiSnapshot, FetchError> {
    let commit = &repository["object"];
    if commit["oid"].as_str() != Some(source.sha.as_str()) {
        return Err(FetchError::normal(
            "Current commit is not available on GitHub (not pushed or repository inaccessible)",
        ));
    }
    let mut snapshot = empty(source, now);
    snapshot.fetch_state = CiFetchState::Ready;
    let prs = &commit["associatedPullRequests"];
    let contexts = &commit["statusCheckRollup"]["contexts"];
    if !prs["nodes"].is_array()
        || (!commit["statusCheckRollup"].is_null() && !contexts["nodes"].is_array())
    {
        return Err(FetchError::normal(
            "GitHub check or pull request results are incomplete or inaccessible",
        ));
    }
    snapshot.truncated =
        contexts["pageInfo"]["hasNextPage"] == true || prs["pageInfo"]["hasNextPage"] == true;
    if prs["pageInfo"]["hasNextPage"] != true {
        snapshot.change_request = matching_request(source, prs["nodes"].as_array().unwrap());
    }
    snapshot.checks = contexts["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| CiCheck {
            name: text(node, "name")
                .or_else(|| text(node, "context"))
                .unwrap_or_else(|| "Unnamed check".into()),
            status: status(node),
            url: text(node, "detailsUrl").or_else(|| text(node, "targetUrl")),
            duration_seconds: if node["__typename"] == "CheckRun" && node["status"] == "COMPLETED" {
                node["startedAt"]
                    .as_str()
                    .and_then(epoch)
                    .zip(node["completedAt"].as_str().and_then(epoch))
                    .and_then(|(start, end)| end.checked_sub(start))
            } else {
                None
            },
        })
        .collect();
    snapshot.status = CiStatus::aggregate(snapshot.checks.iter().map(|check| check.status));
    if snapshot.truncated {
        snapshot.fetch_state = CiFetchState::Unavailable;
        snapshot.error =
            Some("Provider results truncated at 100 checks / 10 associated PRs".into());
        if !matches!(
            snapshot.status,
            CiStatus::Failed | CiStatus::Running | CiStatus::Pending
        ) {
            snapshot.status = CiStatus::Unknown;
        }
    }
    Ok(snapshot)
}

fn alias_index(alias: &str, count: usize) -> Option<usize> {
    let digits = alias.strip_prefix('r')?;
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|index| *index < count)
}

fn graphql_retry(error: &Value) -> RetryPolicy {
    match error["type"].as_str() {
        Some("RATE_LIMITED") => RetryPolicy::RateLimited,
        Some("FORBIDDEN" | "UNAUTHORIZED" | "NOT_FOUND") => RetryPolicy::Authentication,
        _ => RetryPolicy::Normal,
    }
}

fn graphql_error(error: &Value) -> FetchError {
    let retry = graphql_retry(error);
    let message = match retry {
        RetryPolicy::RateLimited => "GitHub rate limit exceeded",
        RetryPolicy::Authentication => {
            "GitHub repository access denied or authentication unavailable"
        }
        RetryPolicy::Normal => "GitHub GraphQL query failed or results are inaccessible",
    };
    FetchError {
        message: message.into(),
        retry,
    }
}

pub(super) fn normalize<'a>(
    value: Value,
    sources: &[CiSource],
    subscribers: impl IntoIterator<Item = &'a CiSource>,
    now: u64,
) -> BatchResponse {
    let errors = value["errors"].as_array();
    let global_error = errors.and_then(|errors| {
        errors
            .iter()
            .filter(|error| {
                error["type"] == "RATE_LIMITED"
                    || error["path"][0]
                        .as_str()
                        .and_then(|alias| alias_index(alias, sources.len()))
                        .is_none()
            })
            .max_by_key(|error| match graphql_retry(error) {
                RetryPolicy::Normal => 0,
                RetryPolicy::Authentication => 1,
                RetryPolicy::RateLimited => 2,
            })
            .map(graphql_error)
    });
    let rate = &value["data"]["rateLimit"];
    let cost = rate["cost"].as_u64().unwrap_or(1);
    let budget_delay = rate["remaining"]
        .as_u64()
        .filter(|remaining| *remaining < 100 || *remaining <= cost.saturating_mul(2))
        .map(|_| {
            rate["resetAt"]
                .as_str()
                .and_then(epoch)
                .map_or(3600, |reset| reset.saturating_sub(now).max(60))
        });
    let retry_after = budget_delay
        .into_iter()
        .chain(value["_retry_after"].as_u64())
        .max();
    let results = subscribers
        .into_iter()
        .filter_map(|source| {
            let index = sources
                .iter()
                .position(|queried| same_commit(queried, source))?;
            let alias = format!("r{index}");
            let result = if let Some(error) = &global_error {
                Err(error.clone())
            } else if let Some(error) = errors.and_then(|errors| {
                errors
                    .iter()
                    .find(|error| error["path"][0].as_str() == Some(alias.as_str()))
            }) {
                Err(graphql_error(error))
            } else {
                parse(source, &value["data"][alias.as_str()], now)
            };
            Some((source.clone(), result))
        })
        .collect();
    BatchResponse {
        results,
        global_error,
        retry_after,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn response<'a>(
    success: bool,
    bytes: &[u8],
    stderr: &str,
    sources: &[CiSource],
    subscribers: impl IntoIterator<Item = &'a CiSource>,
    now: u64,
) -> Result<BatchResponse, FetchError> {
    let (headers, body) = split_http(bytes);
    let hard_retry = retry_after(headers);
    let mut parsed: Option<Value> = serde_json::from_slice(body).ok();
    if let Some(value) = &mut parsed
        && let Some(delay) = hard_retry
    {
        value["_retry_after"] = json!(delay);
    }
    if let Some(value) = parsed
        && (success || value["errors"].is_array())
    {
        return Ok(normalize(value, sources, subscribers, now));
    }
    // Raw CLI stderr may contain private URLs or token-like content and is never published.
    let error = if success {
        FetchError::normal("Invalid GitHub GraphQL response")
    } else if stderr.to_ascii_lowercase().contains("rate limit") {
        FetchError {
            message: "GitHub rate limit exceeded".into(),
            retry: RetryPolicy::RateLimited,
        }
    } else if stderr.contains("auth login")
        || stderr.contains("GH_TOKEN")
        || stderr.contains("401")
        || stderr.contains("403")
    {
        FetchError { message: "GitHub authentication unavailable or repository access denied; use existing gh auth context".into(), retry: RetryPolicy::Authentication }
    } else {
        FetchError::normal("GitHub request failed (network, provider, or repository access)")
    };
    if let Some(delay) = hard_retry {
        // HTTP-level failures can still carry a hard retry deadline, even when the
        // body is not GraphQL or a 403 otherwise looks like an authentication error.
        let results = subscribers
            .into_iter()
            .filter(|source| sources.iter().any(|queried| same_commit(queried, source)))
            .map(|source| (source.clone(), Err(error.clone())))
            .collect();
        Ok(BatchResponse {
            results,
            global_error: Some(error),
            retry_after: Some(delay),
        })
    } else {
        Err(error)
    }
}

fn split_http(bytes: &[u8]) -> (&[u8], &[u8]) {
    if bytes.starts_with(b"HTTP/") {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            return (&bytes[..index], &bytes[index + 4..]);
        }
        if let Some(index) = bytes.windows(2).position(|window| window == b"\n\n") {
            return (&bytes[..index], &bytes[index + 2..]);
        }
    }
    (&[], bytes)
}
fn retry_after(headers: &[u8]) -> Option<u64> {
    std::str::from_utf8(headers).ok()?.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("retry-after")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn epoch(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    fn number(bytes: &[u8]) -> Option<u64> {
        bytes.iter().try_fold(0, |number, byte| {
            byte.is_ascii_digit()
                .then(|| number * 10 + u64::from(byte - b'0'))
        })
    }
    let year = number(&bytes[..4])?;
    let month = number(&bytes[5..7])?;
    let day = number(&bytes[8..10])?;
    let hour = number(&bytes[11..13])?;
    let minute = number(&bytes[14..16])?;
    let second = number(&bytes[17..19])?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days[month as usize - 1]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap_years = |year: u64| year / 4 - year / 100 + year / 400;
    let elapsed = 365 * (year - 1970) + leap_years(year - 1) - leap_years(1969)
        + days[..month as usize - 1].iter().sum::<u64>()
        + day
        - 1;
    Some(elapsed * 86400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> CiSource {
        CiSource {
            repository: repository("https://github.com/owner/repo").unwrap(),
            sha: "abc".into(),
            branch: Some("main".into()),
        }
    }
    fn pr(number: u64, state: &str, draft: bool, updated: &str) -> Value {
        json!({"number":number,"title":format!("Change {number}"),"url":format!("https://github.com/owner/repo/pull/{number}"),"state":state,"isDraft":draft,"updatedAt":updated,"headRefOid":"abc","headRefName":"main"})
    }
    fn repository_result(prs: Vec<Value>, checks: Vec<Value>) -> Value {
        json!({"object":{"oid":"abc","associatedPullRequests":{"nodes":prs,"pageInfo":{"hasNextPage":false}},"statusCheckRollup":{"contexts":{"nodes":checks,"pageInfo":{"hasNextPage":false}}}}})
    }
    fn check(start: Value, end: Value) -> Value {
        json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS","startedAt":start,"completedAt":end})
    }

    #[test]
    fn remote_detection_is_provider_owned_and_does_not_guess_other_hosts() {
        for remote in [
            "git@github.com:owner/repo.git",
            "https://github.com/owner/repo",
            "ssh://git@github.com/owner/repo.git",
        ] {
            assert_eq!(repository(remote), Some(source().repository));
        }
        assert!(repository("git@example.org:owner/repo.git").is_none());
        assert!(repository("https://github.com/owner/repo/extra").is_none());
    }

    #[test]
    fn verified_change_requests_preserve_all_four_provider_states_and_titles() {
        for (state, draft, expected) in [
            ("OPEN", true, ChangeRequestState::Draft),
            ("OPEN", false, ChangeRequestState::Open),
            ("MERGED", false, ChangeRequestState::Merged),
            ("CLOSED", false, ChangeRequestState::Closed),
        ] {
            let snapshot = parse(
                &source(),
                &repository_result(vec![pr(7, state, draft, "2026-10-13T12:00:00Z")], vec![]),
                42,
            )
            .unwrap();
            let request = snapshot.change_request.unwrap();
            assert_eq!(request.state, expected);
            assert_eq!(request.title, "Change 7");
            assert_eq!(request.number, 7);
            assert_eq!(request.url, "https://github.com/owner/repo/pull/7");
            assert_eq!(snapshot.fetch_state, CiFetchState::Ready);
        }
    }

    #[test]
    fn matching_is_exact_active_first_and_ambiguity_never_invents_a_request() {
        let open = pr(7, "OPEN", false, "2026-10-12T12:00:00Z");
        let merged = pr(8, "MERGED", false, "2026-10-13T12:00:00Z");
        assert_eq!(
            matching_request(&source(), &[merged.clone(), open.clone()])
                .unwrap()
                .number,
            7
        );
        assert!(
            matching_request(
                &source(),
                &[open.clone(), pr(9, "OPEN", true, "2026-10-13T12:00:00Z")]
            )
            .is_none()
        );
        let closed = pr(9, "CLOSED", false, "2026-10-11T12:00:00Z");
        assert_eq!(
            matching_request(&source(), &[closed, merged.clone()])
                .unwrap()
                .number,
            8
        );
        assert!(
            matching_request(
                &source(),
                &[
                    merged.clone(),
                    pr(9, "CLOSED", false, "2026-10-13T12:00:00Z")
                ]
            )
            .is_none()
        );
        for field in ["headRefOid", "headRefName"] {
            let mut mismatch = open.clone();
            mismatch[field] = json!("different");
            assert!(matching_request(&source(), &[mismatch]).is_none());
        }
        let mut detached = source();
        detached.branch = None;
        assert!(matching_request(&detached, std::slice::from_ref(&open)).is_none());
        let mut unknown = open.clone();
        unknown["state"] = json!("NEW_STATE");
        assert!(matching_request(&source(), &[unknown]).is_none());
        let mut missing_draft = open;
        missing_draft.as_object_mut().unwrap().remove("isDraft");
        assert!(matching_request(&source(), &[missing_draft]).is_none());
        let mut missing_updated = merged.clone();
        missing_updated["updatedAt"] = Value::Null;
        assert!(matching_request(&source(), &[merged, missing_updated]).is_none());
    }

    #[test]
    fn check_duration_is_completed_checked_time_difference_not_run_id_or_elapsed_time() {
        let start = json!("2026-10-13T12:00:00Z");
        let end = json!("2026-10-13T12:01:42Z");
        let mut running = check(start.clone(), end.clone());
        running["status"] = json!("IN_PROGRESS");
        let mut status_context = check(start.clone(), end.clone());
        status_context["__typename"] = json!("StatusContext");
        status_context["state"] = json!("SUCCESS");
        let checks = vec![
            check(start.clone(), end.clone()),
            check(start.clone(), start.clone()),
            check(Value::Null, end.clone()),
            check(start.clone(), Value::Null),
            check(end.clone(), start.clone()),
            check(json!("invalid"), end),
            check(json!("2026-02-30T12:00:00Z"), start),
            running,
            status_context,
        ];
        let snapshot = parse(&source(), &repository_result(vec![], checks), 42).unwrap();
        assert_eq!(
            snapshot
                .checks
                .iter()
                .map(|check| check.duration_seconds)
                .collect::<Vec<_>>(),
            vec![Some(102), Some(0), None, None, None, None, None, None, None]
        );
    }

    #[test]
    fn truncation_preserves_warning_without_false_green_or_incomplete_pr_guess() {
        let mut value = repository_result(
            vec![pr(7, "OPEN", false, "2026-10-13T12:00:00Z")],
            vec![check(Value::Null, Value::Null)],
        );
        value["object"]["associatedPullRequests"]["pageInfo"]["hasNextPage"] = json!(true);
        let snapshot = parse(&source(), &value, 42).unwrap();
        assert!(snapshot.truncated);
        assert_eq!(snapshot.status, CiStatus::Unknown);
        assert_eq!(snapshot.fetch_state, CiFetchState::Unavailable);
        assert!(snapshot.error.is_some());
        assert!(snapshot.change_request.is_none());
        assert!(parse(&source(), &json!({"object":{"oid":"old"}}), 42).is_err());
        assert_eq!(
            status(&json!({"status":"COMPLETED","conclusion":"CANCELLED"})),
            CiStatus::Cancelled
        );
        assert_eq!(status(&json!({"status":"IN_PROGRESS"})), CiStatus::Running);
        assert_eq!(
            status(&json!({"__typename":"StatusContext","state":"ERROR"})),
            CiStatus::Failed
        );
        assert_eq!(
            status(&json!({"status":"COMPLETED","conclusion":"NEW_STATE"})),
            CiStatus::Unknown
        );
    }

    #[test]
    fn one_batch_carries_all_metadata_and_honors_shared_budget_headers() {
        let sources: Vec<_> = (0..16)
            .map(|index| {
                let mut source = source();
                source.sha = format!("sha{index}");
                source
            })
            .collect();
        let query = query(&sources);
        assert_eq!(query.matches(":repository(").count(), 16);
        assert_eq!(query.matches("rateLimit {").count(), 1);
        for field in ["title", "isDraft", "updatedAt", "startedAt", "completedAt"] {
            assert!(query.contains(field));
        }
        assert_eq!(epoch("1970-01-01T01:00:00Z"), Some(3600));
        assert_eq!(
            epoch("2024-02-29T00:00:00Z").unwrap() + 86400,
            epoch("2024-03-01T00:00:00Z").unwrap()
        );
        assert!(epoch("2023-02-29T00:00:00Z").is_none());
        let (headers, body) = split_http(
            b"HTTP/2.0 429 Too Many Requests\r\nRetry-After: 7200\r\n\r\n{\"errors\":[]}",
        );
        assert_eq!(retry_after(headers), Some(7200));
        assert_eq!(
            serde_json::from_slice::<Value>(body).unwrap(),
            json!({"errors":[]})
        );
        let batch = normalize(
            json!({"data":{"rateLimit":{"remaining":0,"cost":10,"resetAt":"1970-01-01T03:00:00Z"}},"_retry_after":7200}),
            &sources,
            &sources,
            3600,
        );
        assert_eq!(batch.retry_after, Some(7200));
        assert_eq!(batch.results.len(), 16);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn http_failures_retain_hard_retry_after_even_for_auth_or_invalid_bodies() {
        let source = source();
        for (success, stderr, body) in [
            (false, "HTTP 403 denied", "not graphql"),
            (false, "network failure", r#"{"message":"try later"}"#),
            (true, "", "invalid json"),
        ] {
            let wire = format!("HTTP/2.0 403 Forbidden\r\nRetry-After: 7200\r\n\r\n{body}");
            let batch = response(
                success,
                wire.as_bytes(),
                stderr,
                std::slice::from_ref(&source),
                [&source],
                123,
            )
            .unwrap();
            assert_eq!(batch.retry_after, Some(7200));
            assert!(batch.global_error.is_some());
            assert!(batch.results[&source].is_err());
        }
        let rate = response(
            false,
            b"",
            "HTTP 403 rate limit exceeded",
            std::slice::from_ref(&source),
            [&source],
            123,
        )
        .err()
        .unwrap();
        assert!(matches!(rate.retry, RetryPolicy::RateLimited));
    }
}
