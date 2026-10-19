//! GitHub transport and response normalization; no provider protocol escapes this module.
use super::{BatchResponse, CiSource, FetchError, RetryPolicy, empty, same_target};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use bkpr::ci_contract::{
    ChangeRequest, ChangeRequestBase, ChangeRequestState, CiCheck, CiFetchState, CiSnapshot,
    CiStatus, ForgeProvider, ForgeRepository, IssueTracker, LinkedIssue,
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
        if sources[..index]
            .iter()
            .any(|prior| same_target(prior, source))
        {
            continue;
        }
        let (owner, name) = source
            .repository
            .path
            .split_once('/')
            .expect("validated GitHub repository");
        query.push_str(&format!(
            "r{index}:repository(owner:{owner},name:{name}) {{ ",
            owner = json!(owner),
            name = json!(name),
        ));
        if let Some(branch) = &source.remote_branch {
            query.push_str(&format!(
                "object:ref(qualifiedName:{qualified}) {{ target {{ ... on Commit {{ ",
                qualified = json!(format!("refs/heads/{}", branch.name)),
            ));
        } else {
            query.push_str(&format!(
                "object(expression:{sha}) {{ ... on Commit {{ ",
                sha = json!(source.sha),
            ));
        }
        query.push_str("oid statusCheckRollup { state contexts(first:100) { nodes { __typename ... on CheckRun { name status conclusion detailsUrl startedAt completedAt } ... on StatusContext { context state targetUrl } } pageInfo { hasNextPage } } }");
        query.push_str(" } } ");
        if source.remote_branch.is_some() {
            query.push_str(" } ");
        }
        const PR_FIELDS: &str = "number title url body comments(first:10) { nodes { bodyHTML } } isDraft state updatedAt headRefOid headRefName headRepository { nameWithOwner } baseRefName baseRefOid repository { nameWithOwner url }";
        for (branch_index, branch_source) in sources.iter().enumerate() {
            if !same_target(source, branch_source) {
                continue;
            }
            let Some(branch) = branch_source
                .remote_branch
                .as_ref()
                .map(|branch| branch.name.as_str())
                .or(branch_source.branch.as_deref())
            else {
                continue;
            };
            query.push_str(&format!(
                "b{branch_index}:ref(qualifiedName:{qualified}) {{ associatedPullRequests(states:OPEN,first:2) {{ nodes {{ {PR_FIELDS} }} pageInfo {{ hasNextPage }} }} }} c{branch_index}:pullRequests(headRefName:{branch},states:[MERGED,CLOSED],first:1,orderBy:{{field:UPDATED_AT,direction:DESC}}) {{ nodes {{ {PR_FIELDS} }} }} ",
                qualified = json!(format!("refs/heads/{branch}")),
                branch = json!(branch),
            ));
        }
        query.push_str(" } ");
    }
    query.push('}');
    query
}

#[cfg(target_os = "linux")]
pub(super) fn request(sources: &[CiSource]) -> std::io::Result<(std::process::Command, Vec<u8>)> {
    let mut command = std::process::Command::new("gh");
    // The scheduler batches only sources with the same canonical item CWD.
    command.current_dir(&sources[0].cwd);
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

/// Only explicit canonical Linear issue URLs are evidence of an issue association.
/// In particular, a bare ticket identifier never supplies a workspace or URL.
fn linear_issue(url: &str) -> Option<LinkedIssue> {
    if url.len() > 4096
        || url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let url = url.split(['?', '#']).next()?;
    let path = url.strip_prefix("https://linear.app/")?;
    if !path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_/".contains(&byte))
    {
        return None;
    }
    let mut parts = path.trim_end_matches('/').split('/');
    let workspace = parts.next()?;
    if workspace.is_empty() || parts.next()? != "issue" {
        return None;
    }
    let identifier = parts.next()?;
    let (team, number) = identifier.rsplit_once('-')?;
    if team.is_empty()
        || team.len() > 32
        || !team.as_bytes()[0].is_ascii_uppercase()
        || !team
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    if let Some(slug) = parts.next()
        && (slug.is_empty() || parts.next().is_some())
    {
        return None;
    }
    Some(LinkedIssue {
        provider: IssueTracker::Linear,
        identifier: identifier.to_owned(),
        title: None,
        url: url.trim_end_matches('/').to_owned(),
    })
}

fn issue_title(label: &str, issue: &LinkedIssue) -> Option<String> {
    let mut normalized = String::new();
    let mut remaining = 512;
    for word in label.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
            remaining -= 1;
        }
        for c in word.chars().filter(|c| !c.is_control()).take(remaining) {
            normalized.push(c);
            remaining -= 1;
        }
        if remaining == 0 {
            break;
        }
    }
    let label = normalized.trim();
    if label.is_empty() || label.starts_with("https://") || label == issue.identifier {
        return None;
    }
    let label = label
        .trim_start_matches('[')
        .strip_prefix(&issue.identifier)
        .map(|title| title.trim_start_matches([' ', ']', ':', '-', '–', '—']))
        .unwrap_or(label);
    (!label.is_empty()).then(|| label.to_owned())
}

fn insert_issue(issues: &mut Vec<LinkedIssue>, url: &str, label: &str, title: &str) {
    let Some(mut issue) = linear_issue(url) else {
        return;
    };
    issue.title = issue_title(title, &issue).or_else(|| issue_title(label, &issue));
    // A pretty URL and a short URL can identify the same issue.
    fn workspace(url: &str) -> Option<&str> {
        url.strip_prefix("https://linear.app/")
            .and_then(|path| path.split_once("/issue/"))
            .map(|(workspace, _)| workspace)
    }
    if let Some(prior) = issues.iter_mut().find(|prior| {
        prior.identifier == issue.identifier && workspace(&prior.url) == workspace(&issue.url)
    }) {
        if prior.title.is_none() {
            prior.title = issue.title;
        }
    } else if issues.len() < 32 {
        issues.push(issue);
    }
}

#[cfg(test)]
fn linked_issues(html: &str) -> Vec<LinkedIssue> {
    let mut issues = Vec::new();
    collect_linked_issues(&mut issues, html);
    issues
}

fn collect_linked_issues(issues: &mut Vec<LinkedIssue>, html: &str) {
    // Provider-rendered comment anchors supply canonical URLs, not guessed ticket IDs.
    fn bounded(text: &str) -> &str {
        let mut end = text.len().min(1024 * 1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
    let document = Html::parse_fragment(bounded(html));
    let selector = Selector::parse("a[href]").expect("static anchor selector");
    for anchor in document.select(&selector) {
        let url = anchor.value().attr("href").unwrap_or_default();
        if !url.starts_with("https://linear.app/") {
            continue;
        }
        let label = anchor.text().collect::<String>();
        insert_issue(
            issues,
            url,
            &label,
            anchor.value().attr("title").unwrap_or_default(),
        );
    }
}

fn change_request_base(node: &Value) -> Option<ChangeRequestBase> {
    let branch = node["baseRefName"].as_str()?.to_owned();
    let commit = node["baseRefOid"].as_str()?.to_owned();
    if branch.is_empty()
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let repository = repository(node["repository"]["url"].as_str()?)?;
    if !repository
        .path
        .eq_ignore_ascii_case(node["repository"]["nameWithOwner"].as_str()?)
    {
        return None;
    }
    Some(ChangeRequestBase {
        branch,
        commit,
        repository,
    })
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
    let mut issues = Vec::new();
    for comment in node["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
    {
        collect_linked_issues(
            &mut issues,
            comment["bodyHTML"].as_str().unwrap_or_default(),
        );
    }
    Some(ChangeRequest {
        number: node["number"].as_u64()?,
        title: text(node, "title")?,
        url: text(node, "url")?,
        state,
        description: node["body"].as_str().unwrap_or_default().to_owned(),
        issues,
        base: change_request_base(node),
    })
}

fn matching_request(source: &CiSource, nodes: &[Value], checked_sha: &str) -> Option<ChangeRequest> {
    // Every PR association is verified against the actual provider-checked head.
    let branch = source.remote_branch.as_ref()
        .map(|branch| branch.name.as_str())
        .or(source.branch.as_deref())?;
    let matching = || {
        nodes.iter().filter(|node| {
            node["headRefOid"]
                .as_str()
                .is_some_and(|sha| sha == checked_sha)
                && node["headRefName"].as_str() == Some(branch)
                && node["headRepository"]["nameWithOwner"]
                    .as_str()
                    .is_some_and(|path| path.eq_ignore_ascii_case(&source.repository.path))
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

fn parse(
    source: &CiSource,
    repository: &Value,
    now: u64,
    index: usize,
) -> Result<CiSnapshot, FetchError> {
    if !repository.is_object() {
        return Err(FetchError::normal("GitHub repository is inaccessible"));
    }
    let commit = if source.remote_branch.is_some() {
        &repository["object"]["target"]
    } else {
        &repository["object"]
    };
    let mut snapshot = empty(source, now);
    let checked_sha = commit["oid"].as_str().filter(|sha| !sha.is_empty());
    if checked_sha.is_none()
        || (source.remote_branch.is_none() && checked_sha != Some(source.sha.as_str()))
    {
        snapshot.fetch_state = CiFetchState::Unavailable;
        snapshot.commit_missing = true;
        snapshot.error = Some(if let Some(branch) = source.checked_ref() {
            format!("Remote branch {branch} is not available on GitHub")
        } else {
            "Current commit is not available on GitHub (not pushed or repository inaccessible)".into()
        });
        return Ok(snapshot);
    }
    snapshot.head_sha = checked_sha.unwrap().to_owned();
    if source.branch.is_some() {
        let branch_ref = &repository[format!("b{index}")];
        let active = &branch_ref["associatedPullRequests"];
        if let Some(nodes) = active["nodes"].as_array() {
            // A capped active connection is ambiguous, not evidence of a closed PR.
            if active["pageInfo"]["hasNextPage"] != true {
                snapshot.change_request = matching_request(source, nodes, &snapshot.head_sha);
            }
        }
        // Only fall back when the branch has no active PR, including a deleted ref.
        if (branch_ref.is_null() || active["nodes"].as_array().is_some_and(Vec::is_empty))
            && let Some(nodes) = repository[format!("c{index}")]["nodes"].as_array()
        {
            snapshot.change_request = matching_request(source, nodes, &snapshot.head_sha);
        }
    }
    snapshot.fetch_state = CiFetchState::Ready;
    let contexts = &commit["statusCheckRollup"]["contexts"];
    if !commit["statusCheckRollup"].is_null() && !contexts["nodes"].is_array() {
        return Err(FetchError::normal(
            "GitHub check results are incomplete or inaccessible",
        ));
    }
    snapshot.truncated = contexts["pageInfo"]["hasNextPage"] == true;
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
        snapshot.error = Some("Provider results truncated at 100 checks".into());
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
            let branch_index = sources.iter().position(|queried| queried == source)?;
            let index = sources
                .iter()
                .position(|queried| same_target(queried, source))?;
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
                parse(source, &value["data"][alias.as_str()], now, branch_index)
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
            .filter(|source| sources.contains(source))
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

    fn matching_request(source: &CiSource, nodes: &[Value]) -> Option<ChangeRequest> {
        super::matching_request(source, nodes, &source.sha)
    }

    #[test]
    fn request_description_and_comment_linear_links_survive_provider_normalization() {
        let body = "Searchable full description\n\n\
            [ENG-42 Fix **unicode** rendering](https://linear.app/work/issue/ENG-42/fix-rendering)\n\
            [ENG-43][ticket]\n\n\
            [ticket]: https://linear.app/work/issue/ENG-43 \"Preserve link titles\"\n\n\
            https://linear.app/work/issue/ENG-44/plain-url.";
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        node["body"] = json!(body);
        node["comments"] = json!({"nodes":[{"bodyHTML":
            "<p><a href=\"https://linear.app/work/issue/ENG-42/fix-rendering\">ENG-42 Fix <b>unicode</b> rendering</a>\
             <a href=\"https://linear.app/work/issue/ENG-43\" title=\"Preserve link titles\">ENG-43</a>\
             <a href=\"https://linear.app/work/issue/ENG-44/plain-url\">ENG-44</a></p>"
        }]});
        let snapshot = parse(&source(), &repository_result(vec![node], vec![]), 42, 0).unwrap();
        let request = snapshot.change_request.unwrap();
        assert_eq!(request.description, body);
        assert_eq!(
            request
                .issues
                .iter()
                .map(|issue| (
                    issue.identifier.as_str(),
                    issue.title.as_deref(),
                    issue.url.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "ENG-42",
                    Some("Fix unicode rendering"),
                    "https://linear.app/work/issue/ENG-42/fix-rendering"
                ),
                (
                    "ENG-43",
                    Some("Preserve link titles"),
                    "https://linear.app/work/issue/ENG-43"
                ),
                (
                    "ENG-44",
                    None,
                    "https://linear.app/work/issue/ENG-44/plain-url"
                ),
            ]
        );
    }

    #[test]
    fn rendered_autolinks_supply_verified_urls_titles_and_html_entities() {
        let issues = linked_issues(
            r#"<p><a href="https://linear.app/real-workspace/issue/ENG-42"
                title="Fix &amp; simplify &quot;rendering&quot;"><span>ENG-42</span></a>
                <a href='https://linear.app/other/issue/OPS-9'><b>OPS-9</b> Review &amp; ship</a></p>"#,
        );
        assert_eq!(issues.len(), 2);
        assert_eq!(
            issues[0].url,
            "https://linear.app/real-workspace/issue/ENG-42"
        );
        assert_eq!(
            issues[0].title.as_deref(),
            Some("Fix & simplify \"rendering\"")
        );
        assert_eq!(issues[1].title.as_deref(), Some("Review & ship"));
        let raw_html =
            r#"<a href="https://linear.app/work/issue/ENG-7"><b>ENG-7</b> HTML-only body</a>"#;
        let issues = linked_issues(raw_html);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].url, "https://linear.app/work/issue/ENG-7");
        assert_eq!(issues[0].title.as_deref(), Some("HTML-only body"));
    }

    #[test]
    fn unsupported_or_deceptive_issue_links_never_produce_metadata() {
        for url in [
            "http://linear.app/work/issue/ENG-42",
            "https://linear.app.evil.test/work/issue/ENG-42",
            "https://evil.test/https://linear.app/work/issue/ENG-42",
            "https://linear.app@evil.test/work/issue/ENG-42",
            "https://evil.test@linear.app/work/issue/ENG-42",
            "https://linear.app:443/work/issue/ENG-42",
            "https://linear.app/work/project/ENG-42",
            "https://linear.app/work/issue/not-a-ticket",
            "https://linear.app/work/issue/ENG-42/slug/extra",
            "https://linear.app/work/issue/ENG-42%0a",
            "https://linear.app/work/issue/ENG-42\\evil",
            "javascript:https://linear.app/work/issue/ENG-42",
        ] {
            assert!(linear_issue(url).is_none(), "{url}");
            let html = format!("<a href='{url}'>ENG-42</a>");
            assert!(linked_issues(&html).is_empty(), "{url}");
        }
        assert!(
            linked_issues(
                r#"<a href="https://linear.app/work/issue/ENG-42&#x0a;">Unsafe entity</a>"#
            )
            .is_empty()
        );
        assert!(linked_issues("<p>ENG-42 #42 Linear ENG-42</p>").is_empty());
        let request = change_request(&pr(7, "OPEN", false, "2026-10-13T12:00:00Z")).unwrap();
        assert!(request.description.is_empty());
        assert!(request.issues.is_empty());
    }

    #[test]
    fn duplicate_issue_urls_merge_titles_but_not_different_workspaces() {
        let issues = linked_issues(
            r#"<a href="https://linear.app/work/issue/ENG-42">ENG-42</a>
                <a href="https://linear.app/work/issue/ENG-42/pretty-slug/">ENG-42</a>
                <a href="https://linear.app/work/issue/ENG-42?source=github"
                title="Useful title">ENG-42</a>
                <a href="https://linear.app/other/issue/ENG-42">Other workspace issue</a>"#,
        );
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].url, "https://linear.app/work/issue/ENG-42");
        assert_eq!(issues[0].title.as_deref(), Some("Useful title"));
        assert_eq!(issues[1].url, "https://linear.app/other/issue/ENG-42");
    }

    #[test]
    fn issue_parsing_is_bounded_without_truncating_searchable_description() {
        let body = (1..=40)
            .map(|number| format!("https://linear.app/work/issue/ENG-{number}\n\n"))
            .collect::<String>();
        let html = (1..=40)
            .map(|number| {
                format!("<a href=\"https://linear.app/work/issue/ENG-{number}\">ENG-{number}</a>")
            })
            .collect::<String>();
        assert_eq!(linked_issues(&html).len(), 32);
        let body = format!("{body}{} searchable-tail", "界".repeat(400_000));
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        node["body"] = json!(body);
        node["comments"] = json!({"nodes":[{"bodyHTML": html}]});
        let request = change_request(&node).unwrap();
        assert_eq!(request.description, body);
        assert_eq!(request.issues.len(), 32);
    }

    fn source() -> CiSource {
        CiSource {
            cwd: "/project".into(),
            repository: repository("https://github.com/owner/repo").unwrap(),
            sha: "abc".into(),
            branch: Some("main".into()),
            remote_branch: None,
        }
    }
    fn pr(number: u64, state: &str, draft: bool, updated: &str) -> Value {
        json!({"number":number,"title":format!("Change {number}"),"url":format!("https://github.com/owner/repo/pull/{number}"),"state":state,"isDraft":draft,"updatedAt":updated,"headRefOid":"abc","headRefName":"main","headRepository":{"nameWithOwner":"owner/repo"}})
    }
    fn repository_result(prs: Vec<Value>, checks: Vec<Value>) -> Value {
        let (active, closed): (Vec<_>, Vec<_>) =
            prs.into_iter().partition(|node| node["state"] == "OPEN");
        json!({"object":{"oid":"abc","statusCheckRollup":{"contexts":{"nodes":checks,"pageInfo":{"hasNextPage":false}}}},"b0":{"associatedPullRequests":{"nodes":active,"pageInfo":{"hasNextPage":false}}},"c0":{"nodes":closed}})
    }
    fn check(start: Value, end: Value) -> Value {
        json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS","startedAt":start,"completedAt":end})
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gh_uses_each_item_cwd_without_overriding_auth_configuration() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let wrapper = bin.join("gh");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
case "$PWD" in */one) conclusion=SUCCESS;; *) conclusion=FAILURE;; esac
printf '{"data":{"r0":{"object":{"oid":"abc","statusCheckRollup":{"contexts":{"nodes":[{"__typename":"CheckRun","name":"%s","status":"COMPLETED","conclusion":"%s"}],"pageInfo":{"hasNextPage":false}}}},"b0":{"associatedPullRequests":{"nodes":[],"pageInfo":{"hasNextPage":false}}},"c0":{"nodes":[]}},"rateLimit":{"remaining":5000}}}' "$PWD" "$conclusion"
"#,
        ).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (directory, expected) in [("one", CiStatus::Passed), ("two", CiStatus::Failed)] {
            let mut source = source();
            source.cwd = root.path().join(directory);
            std::fs::create_dir(&source.cwd).unwrap();
            source.cwd = std::fs::canonicalize(&source.cwd).unwrap();
            let sources = [source.clone()];
            let (mut command, _) = request(&sources).unwrap();
            assert_eq!(command.get_current_dir(), Some(source.cwd.as_path()));
            assert_eq!(command.get_envs().count(), 0);
            // Override PATH only on this child; never mutate the test process's auth/env.
            let output = command.env("PATH", &bin).output().unwrap();
            assert!(output.status.success());
            let mut batch = response(true, &output.stdout, "", &sources, sources.iter(), 42).unwrap();
            let snapshot = batch.results.remove(&source).unwrap().unwrap();
            assert_eq!(snapshot.status, expected);
            assert_eq!(snapshot.checks[0].name, source.cwd.to_str().unwrap());
        }
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
                0,
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
    fn branch_identity_completed_heads_and_ambiguity_are_verified() {
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
        let mut mismatch = open.clone();
        mismatch["headRefName"] = json!("different");
        assert!(matching_request(&source(), &[mismatch]).is_none());
        let mut mismatched_completed = merged.clone();
        mismatched_completed["headRefOid"] = json!("different");
        assert!(matching_request(&source(), &[mismatched_completed]).is_none());
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
    fn remote_branch_reports_published_checks_for_unpushed_local_head() {
        let mut source = source();
        source.sha = "unpushed-local-head".into();
        source.branch = Some("local-work".into());
        source.remote_branch = Some(super::super::RemoteBranch {
            remote: "upstream".into(),
            name: "main".into(),
            tracking_sha: Some("stale-tracking-head".into()),
        });
        let mut value = repository_result(
            vec![pr(7, "OPEN", false, "2026-10-13T12:00:00Z")],
            vec![check(Value::Null, Value::Null)],
        );
        let commit = value["object"].take();
        value["object"] = json!({"target": commit});
        let snapshot = parse(&source, &value, 42, 0).unwrap();
        assert_eq!(snapshot.change_request.as_ref().unwrap().number, 7);
        assert_eq!(snapshot.head_sha, "abc");
        assert_eq!(snapshot.local_head_sha, "unpushed-local-head");
        assert_eq!(snapshot.branch.as_deref(), Some("local-work"));
        assert_eq!(snapshot.checked_ref.as_deref(), Some("upstream/main"));
        assert_eq!(snapshot.status, CiStatus::Passed);
        assert_eq!(snapshot.fetch_state, CiFetchState::Ready);
        assert_eq!(snapshot.checks.len(), 1);
        assert!(snapshot.error.is_none());
        for state in ["MERGED", "CLOSED"] {
            let completed = pr(8, state, false, "2026-10-14T12:00:00Z");
            value["b0"]["associatedPullRequests"]["nodes"] = json!([]);
            value["c0"]["nodes"] = json!([completed]);
            assert_eq!(parse(&source, &value, 42, 0).unwrap().change_request.unwrap().number, 8);
            value["c0"]["nodes"][0]["headRefOid"] = json!("not-the-remote-tip");
            assert!(parse(&source, &value, 42, 0).unwrap().change_request.is_none());
        }
        value["b0"]["associatedPullRequests"]["nodes"] =
            json!([pr(7, "OPEN", false, "2026-10-13T12:00:00Z")]);

        value["b0"]["associatedPullRequests"]["nodes"][0]["headRefOid"] = json!("different");
        assert!(parse(&source, &value, 42, 0).unwrap().change_request.is_none());
        value["object"] = Value::Null;
        let missing = parse(&source, &value, 42, 0).unwrap();
        assert!(missing.commit_missing);
        assert!(missing.head_sha.is_empty());
        assert!(missing.change_request.is_none());
        assert_eq!(missing.fetch_state, CiFetchState::Unavailable);
        assert!(missing.error.as_deref().unwrap().contains("upstream/main"));
    }

    #[test]
    fn shared_remote_ref_publishes_provider_tip_with_each_local_identity() {
        let mut first = source();
        first.sha = "local-one".into();
        first.branch = Some("local-one".into());
        first.remote_branch = Some(super::super::RemoteBranch {
            remote: "origin".into(),
            name: "main".into(),
            tracking_sha: None,
        });
        let mut second = first.clone();
        second.sha = "local-two".into();
        second.branch = Some("local-two".into());
        let mut value = repository_result(
            vec![pr(7, "OPEN", false, "2026-10-13T12:00:00Z")],
            vec![check(Value::Null, Value::Null)],
        );
        let commit = value["object"].take();
        value["object"] = json!({"target": commit});
        value["b1"] = value["b0"].clone();
        value["c1"] = value["c0"].clone();
        let sources = [first, second];
        let batch = normalize(json!({"data":{"r0":value}}), &sources, &sources, 42);
        assert_eq!(batch.results.len(), 2);
        for source in &sources {
            let snapshot = batch.results[source].as_ref().unwrap();
            assert_eq!(snapshot.head_sha, "abc");
            assert_eq!(snapshot.local_head_sha, source.sha);
            assert_eq!(snapshot.branch, source.branch);
            assert_eq!(snapshot.checks.len(), 1);
            assert_eq!(snapshot.change_request.as_ref().unwrap().number, 7);
        }
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
        let snapshot = parse(&source(), &repository_result(vec![], checks), 42, 0).unwrap();
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
    fn tracker_comment_recovers_ticket_not_linked_in_description() {
        let source = source();
        let url = "https://linear.app/work/issue/ENG-42/fix-rendering";
        let title = "Fix rendering";
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        node["body"] = json!("Fixes ENG-42.");
        node["comments"] = json!({"nodes":[{"bodyHTML":format!(
            "<details><summary><a href=\"{url}\" rel=\"nofollow\">ENG-42 {title}</a></summary></details><a href=\"https://linear.app/work/review/fix-rendering\">Review in Linear</a>"
        )}]});
        let value = repository_result(vec![node], vec![check(Value::Null, Value::Null)]);
        let batch = normalize(
            json!({"data":{"r0":value}}),
            std::slice::from_ref(&source),
            [&source],
            42,
        );
        let snapshot = batch.results[&source].as_ref().unwrap();
        let request = snapshot.change_request.as_ref().unwrap();
        assert_eq!(request.number, 7);
        assert_eq!(request.issues.len(), 1);
        assert_eq!(request.issues[0].url, url);
        assert_eq!(request.issues[0].title.as_deref(), Some(title));
        assert_eq!(snapshot.status, CiStatus::Passed);
        assert_eq!(snapshot.fetch_state, CiFetchState::Ready);
        assert!(!snapshot.truncated);
    }

    #[test]
    fn comments_enforce_safe_hosts_workspace_identity_and_read_bounds() {
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        let mut comments = vec![json!({"bodyHTML":
            "<a href='https://linear.app/work/issue/ENG-42/pretty'>ENG-42 Useful title</a>\
             <a href='https://linear.app/other/issue/ENG-42'>Other workspace</a>\
             <a href='https://linear.app.evil.test/work/issue/ENG-9'>Unsafe</a>"
        })];
        comments.extend((1..10).map(|_| json!({"bodyHTML":"<p>No ticket here</p>"})));
        comments.push(json!({"bodyHTML":"<a href='https://linear.app/work/issue/ENG-99'>Ignored eleventh comment</a>"}));
        node["comments"] = json!({"nodes":comments});
        let request = change_request(&node).unwrap();
        assert_eq!(request.issues.len(), 2);
        assert_eq!(request.issues[0].title.as_deref(), Some("Useful title"));
        assert_eq!(
            request.issues[1].url,
            "https://linear.app/other/issue/ENG-42"
        );
    }

    #[test]
    fn shared_commit_keeps_branch_metadata_isolated_and_preserves_distinct_same_named_jobs() {
        let first = source();
        let mut second = first.clone();
        second.branch = Some("other".into());
        let mut detached = first.clone();
        detached.branch = None;
        let mut unqueried = first.clone();
        unqueried.branch = Some("arrived-later".into());
        let sources = [first.clone(), second.clone(), detached.clone()];
        let mut main_pr = pr(7, "OPEN", true, "2026-10-13T12:00:00Z");
        main_pr["comments"] = json!({"nodes":[{"bodyHTML":"<a href='https://linear.app/work/issue/ENG-7'>Ticket</a>"}]});
        let mut other_pr = pr(8, "OPEN", false, "2026-10-13T12:00:00Z");
        other_pr["headRefName"] = json!("other");
        other_pr["comments"] = json!({"nodes":[{"bodyHTML":"<a href='https://linear.app/work/issue/ENG-8'>Ticket</a>"}]});
        let mut push = check(Value::Null, Value::Null);
        push["detailsUrl"] = json!("https://github.com/owner/repo/actions/runs/1");
        let mut pull_request = push.clone();
        pull_request["detailsUrl"] = json!("https://github.com/owner/repo/actions/runs/2");
        let mut value = repository_result(vec![main_pr], vec![push, pull_request]);
        value["b1"] =
            json!({"associatedPullRequests":{"nodes":[other_pr],"pageInfo":{"hasNextPage":false}}});
        value["c1"] = json!({"nodes":[]});
        let batch = normalize(
            json!({"data":{"r0":value}}),
            &sources,
            [&first, &second, &detached, &unqueried],
            42,
        );
        assert_eq!(batch.results.len(), 3);
        for (source, number, ticket) in [(&first, 7, "ENG-7"), (&second, 8, "ENG-8")] {
            let snapshot = batch.results[source].as_ref().unwrap();
            assert_eq!(snapshot.checks.len(), 2);
            assert_ne!(snapshot.checks[0].url, snapshot.checks[1].url);
            let request = snapshot.change_request.as_ref().unwrap();
            assert_eq!(request.number, number);
            assert_eq!(request.issues[0].identifier, ticket);
        }
        let snapshot = batch.results[&detached].as_ref().unwrap();
        assert_eq!(snapshot.checks.len(), 2);
        assert!(snapshot.change_request.is_none());
    }

    #[test]
    fn pr_history_and_ambiguity_never_make_complete_checks_unavailable() {
        let mut value = repository_result(
            vec![
                pr(7, "OPEN", false, "2026-10-13T12:00:00Z"),
                pr(8, "MERGED", false, "2026-10-14T12:00:00Z"),
            ],
            vec![check(Value::Null, Value::Null)],
        );
        value["c0"]["pageInfo"] = json!({"hasNextPage":true});
        let snapshot = parse(&source(), &value, 42, 0).unwrap();
        assert_eq!(snapshot.change_request.unwrap().number, 7);
        assert_eq!(snapshot.fetch_state, CiFetchState::Ready);
        assert_eq!(snapshot.status, CiStatus::Passed);
        assert!(!snapshot.truncated);
        value["b0"]["associatedPullRequests"]["pageInfo"]["hasNextPage"] = json!(true);
        let snapshot = parse(&source(), &value, 42, 0).unwrap();
        assert!(snapshot.change_request.is_none());
        assert_eq!(snapshot.fetch_state, CiFetchState::Ready);
        assert_eq!(snapshot.status, CiStatus::Passed);
        assert!(snapshot.error.is_none());
        value["b0"] = Value::Null;
        let snapshot = parse(&source(), &value, 42, 0).unwrap();
        assert_eq!(
            snapshot.change_request.unwrap().state,
            ChangeRequestState::Merged
        );
        let mut fork = pr(9, "OPEN", false, "2026-10-13T12:00:00Z");
        fork["headRepository"]["nameWithOwner"] = json!("fork/repo");
        value["b0"] =
            json!({"associatedPullRequests":{"nodes":[fork],"pageInfo":{"hasNextPage":false}}});
        assert!(
            parse(&source(), &value, 42, 0)
                .unwrap()
                .change_request
                .is_none()
        );
    }

    #[test]
    fn check_truncation_warns_without_erasing_verified_request() {
        let mut value = repository_result(
            vec![pr(7, "OPEN", false, "2026-10-13T12:00:00Z")],
            vec![check(Value::Null, Value::Null)],
        );
        value["object"]["statusCheckRollup"]["contexts"]["pageInfo"]["hasNextPage"] = json!(true);
        let snapshot = parse(&source(), &value, 42, 0).unwrap();
        assert!(snapshot.truncated);
        assert_eq!(snapshot.status, CiStatus::Unknown);
        assert_eq!(snapshot.fetch_state, CiFetchState::Unavailable);
        assert!(snapshot.error.is_some());
        assert_eq!(snapshot.change_request.unwrap().number, 7);
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
    fn shared_budget_and_http_headers_preserve_retry_deadlines() {
        let sources: Vec<_> = (0..16)
            .map(|index| {
                let mut source = source();
                source.sha = format!("sha{index}");
                source
            })
            .collect();
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

    #[test]
    fn authoritative_pr_base_metadata_requires_a_valid_provider_commit() {
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        node["baseRefName"] = json!("stack/parent");
        node["baseRefOid"] = json!("1234567890abcdef1234567890abcdef12345678");
        node["repository"] = json!({
            "nameWithOwner":"upstream/project", "url":"https://github.com/upstream/project"
        });
        let request = change_request(&node).unwrap();
        let base = request.base.unwrap();
        assert_eq!(base.branch, "stack/parent");
        assert_eq!(base.commit, "1234567890abcdef1234567890abcdef12345678");
        assert_eq!(base.repository.path, "upstream/project");
        node["baseRefOid"] = json!("--not-a-commit");
        assert!(change_request(&node).unwrap().base.is_none());
    }

    #[test]
    fn issue_association_is_comment_only_and_not_workspace_or_bot_specific() {
        let body_url = "https://linear.app/body-team/issue/ENG-42";
        let comment_url = "https://linear.app/another-workspace/issue/ENG-42/real-ticket";
        let mut node = pr(7, "OPEN", false, "2026-10-13T12:00:00Z");
        node["body"] = json!(format!("Body references {body_url}"));
        node["bodyHTML"] = json!(format!("<a href=\"{body_url}\">ENG-42 Body reference</a>"));
        let request = change_request(&node).unwrap();
        assert!(request.issues.is_empty());
        assert!(request.description.contains(body_url));
        node["comments"] = json!({"nodes":[{
            "author":{"login":"generic-tracker-bot"},
            "bodyHTML":format!("<details><summary><a href=\"{comment_url}\">ENG-42 Real ticket</a></summary></details>")
        }]});
        let request = change_request(&node).unwrap();
        assert_eq!(request.issues.len(), 1);
        assert_eq!(request.issues[0].url, comment_url);
        assert_eq!(request.issues[0].title.as_deref(), Some("Real ticket"));
    }

    #[test]
    fn commit_availability_is_explicit_not_inferred_from_error_messages() {
        let value = repository_result(Vec::new(), Vec::new());
        assert!(!parse(&source(), &value, 42, 0).unwrap().commit_missing);
        for object in [Value::Null, json!({"oid":"other"})] {
            let mut missing = value.clone();
            missing["object"] = object;
            let snapshot = parse(&source(), &missing, 42, 0).unwrap();
            assert!(snapshot.commit_missing);
            assert_eq!(snapshot.fetch_state, CiFetchState::Unavailable);
        }
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
