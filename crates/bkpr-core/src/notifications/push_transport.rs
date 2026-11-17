//! Encrypted Web Push publication with per-attempt, fail-closed egress pinning.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bkpr_protocol::{PushEndpointPolicy, PushSubscription};
use iprfc::IpNet;
use jwt_simple::prelude::{ES256KeyPair, ES256PublicKey};
use reqwest::header::{AUTHORIZATION, RETRY_AFTER};
use reqwest::{Client, Request, Url};
use web_push::{
    ContentEncoding, PartialVapidSignatureBuilder, SubscriptionInfo, Urgency,
    VapidSignatureBuilder, WebPushMessageBuilder, request_builder,
};

const MAX_ENDPOINT_BYTES: usize = 4096;
const MAX_PAYLOAD_BYTES: usize = 3000;
const MAX_PRIVATE_CIDRS: usize = 16;
const MAX_DNS_ADDRESSES: usize = 16;
const MAX_DNS_LOOKUPS: usize = 4;
static ACTIVE_DNS_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
const MAX_TTL_SECONDS: u32 = 86_400;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(86_400);
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const VAPID_LIFETIME: Duration = Duration::from_secs(3600);

/// Result of one attempt; provider acceptance is not device delivery or display.
#[derive(Debug)]
pub(crate) enum PushSendOutcome {
    /// The provider returned HTTP 2xx, accepting the message without proving device delivery.
    Accepted,
    /// The provider reported an expired or nonexistent subscription (404/410).
    Expired,
    /// A definitive HTTP response rejected this attempt; it is not retried automatically.
    Rejected {
        /// HTTP response status, without response contents or endpoint secrets.
        status: u16,
    },
    /// A definitely unaccepted attempt may be retried within the outbox's expiry budget.
    Retry {
        /// Provider-requested minimum delay, capped at one day when supplied.
        after: Option<Duration>,
    },
    /// Local validation or request construction failed before any HTTP dispatch.
    NotSent {
        /// Stable diagnostic code that never includes endpoint or key material.
        reason: String,
    },
    /// Dispatch may have reached the provider; automatically replaying is unsafe.
    Unknown {
        /// Stable diagnostic code that never includes endpoint or key material.
        reason: String,
    },
}

/// Host VAPID signing identity; it holds no session state or shared HTTP destination pool.
pub(crate) struct Publisher {
    signing_key: PartialVapidSignatureBuilder,
}

impl Publisher {
    /// Generates a private P-256 VAPID key in PKCS#8 PEM for private durable storage.
    pub(crate) fn generate_key() -> Result<String> {
        ES256KeyPair::generate()
            .to_pem()
            .map_err(|_| anyhow!("push_key_generation_failed"))
    }

    /// Loads a private VAPID PEM; malformed or oversized key material is rejected.
    pub(crate) fn new(private_pem: &str) -> Result<Self> {
        if private_pem.len() > 4096 {
            bail!("push_signing_key_invalid");
        }
        let signing_key = VapidSignatureBuilder::from_pem_no_sub(private_pem.as_bytes())
            .map_err(|_| anyhow!("push_signing_key_invalid"))?;
        Ok(Self { signing_key })
    }

    /// Exports the uncompressed 65-byte SEC1 application-server key, base64url without padding.
    pub(crate) fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing_key.get_public_key())
    }

    /// Publishes at normal urgency with explicit TTL (1–86400 seconds) and at most 3000 payload bytes.
    ///
    /// DNS is bounded to three seconds; a pinned HTTPS attempt has a five-second connection and
    /// fifteen-second total timeout. Cancellation drops the attempt; callers must retain an unknown
    /// outcome if cancellation occurred after durable dispatch admission. No locks may cover this call.
    pub(crate) async fn send(
        &self,
        subscription: &PushSubscription,
        policy: &PushEndpointPolicy,
        payload: &[u8],
        ttl: u32,
    ) -> PushSendOutcome {
        let endpoint = match validate(subscription, policy) {
            Ok(endpoint) => endpoint,
            Err(error) => return not_sent(error.to_string()),
        };
        if payload.len() > MAX_PAYLOAD_BYTES {
            return not_sent("push_payload_too_large");
        }
        if ttl == 0 || ttl > MAX_TTL_SECONDS {
            return not_sent("push_ttl_invalid");
        }
        let addresses = match resolve_endpoint(&endpoint, policy).await {
            Ok(addresses) => addresses,
            Err(outcome) => return outcome,
        };
        // A fresh client has no cached connections or fallback resolver paths for this destination.
        let client = match pinned_client(&endpoint, &addresses) {
            Ok(client) => client,
            Err(_) => return not_sent("push_http_client_unavailable"),
        };
        let request = match self.build_request(&client, subscription, endpoint, payload, ttl) {
            Ok(request) => request,
            Err(error) => return not_sent(error.to_string()),
        };
        match client.execute(request).await {
            Ok(response) => classify_response(
                response.status().as_u16(),
                response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
                SystemTime::now(),
            ),
            Err(error) if error.is_connect() => PushSendOutcome::Retry { after: None },
            Err(_) => PushSendOutcome::Unknown {
                reason: "push_http_outcome_unknown".into(),
            },
        }
    }

    fn build_request(
        &self,
        client: &Client,
        subscription: &PushSubscription,
        endpoint: Url,
        payload: &[u8],
        ttl: u32,
    ) -> Result<Request> {
        let subscription = SubscriptionInfo::new(
            endpoint.as_str(),
            subscription.p256dh.as_str(),
            subscription.auth.as_str(),
        );
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|now| now.checked_add(VAPID_LIFETIME))
            .ok_or_else(|| anyhow!("push_clock_invalid"))?;
        let mut signature = self.signing_key.clone().add_sub_info(&subscription);
        signature.add_claim("aud", endpoint.origin().ascii_serialization());
        signature.add_claim("exp", expires.as_secs());
        signature.add_claim("sub", "https://github.com/calops/bkpr");
        let signature = signature
            .build()
            .map_err(|_| anyhow!("push_signing_failed"))?;
        let mut message = WebPushMessageBuilder::new(&subscription);
        message.set_payload(ContentEncoding::Aes128Gcm, payload);
        message.set_vapid_signature(signature);
        message.set_ttl(ttl);
        message.set_urgency(Urgency::Normal);
        let message = message
            .build()
            .map_err(|_| anyhow!("push_encryption_failed"))?;
        let (parts, body) = request_builder::build_request::<Vec<u8>>(message).into_parts();
        let mut request = client.post(endpoint).body(body);
        for (name, value) in &parts.headers {
            request = request.header(name.as_str(), value.as_bytes());
        }
        let mut request = request
            .build()
            .map_err(|_| anyhow!("push_request_invalid"))?;
        if let Some(value) = request.headers_mut().get_mut(AUTHORIZATION) {
            value.set_sensitive(true);
        }
        Ok(request)
    }
}

/// Checks subscription syntax, key validity, literal-IP egress and bounded private exceptions.
/// Domain resolution is deferred until each send; this function performs no network I/O.
pub(crate) fn validate_subscription(
    subscription: &PushSubscription,
    policy: &PushEndpointPolicy,
) -> Result<()> {
    validate(subscription, policy).map(|_| ())
}

fn validate(subscription: &PushSubscription, policy: &PushEndpointPolicy) -> Result<Url> {
    let networks = private_networks(policy)?;
    let endpoint = parse_endpoint(&subscription.endpoint)?;
    if literal_address(&endpoint).is_some_and(|ip| !address_allowed(ip, &networks)) {
        bail!("push_endpoint_address_disallowed");
    }
    if subscription.p256dh.len() != 87 || subscription.auth.len() != 22 {
        bail!("push_subscription_keys_invalid");
    }
    let public = URL_SAFE_NO_PAD
        .decode(&subscription.p256dh)
        .map_err(|_| anyhow!("push_subscription_keys_invalid"))?;
    let auth = URL_SAFE_NO_PAD
        .decode(&subscription.auth)
        .map_err(|_| anyhow!("push_subscription_keys_invalid"))?;
    if public.len() != 65 || public[0] != 4 || auth.len() != 16 {
        bail!("push_subscription_keys_invalid");
    }
    ES256PublicKey::from_bytes(&public).map_err(|_| anyhow!("push_subscription_keys_invalid"))?;
    Ok(endpoint)
}

fn parse_endpoint(endpoint: &str) -> Result<Url> {
    if endpoint.len() > MAX_ENDPOINT_BYTES
        || endpoint
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
        || endpoint.trim() != endpoint
    {
        bail!("push_endpoint_invalid");
    }
    let url = Url::parse(endpoint).map_err(|_| anyhow!("push_endpoint_invalid"))?;
    let authority_has_userinfo = endpoint
        .split_once("://")
        .map(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
                .contains('@')
        })
        .unwrap_or(true);
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.port() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || authority_has_userinfo
        || url.fragment().is_some()
    {
        bail!("push_endpoint_invalid");
    }
    Ok(url)
}

fn private_networks(policy: &PushEndpointPolicy) -> Result<Vec<IpNet>> {
    if policy.allow_private_cidrs.len() > MAX_PRIVATE_CIDRS {
        bail!("push_endpoint_policy_invalid");
    }
    policy
        .allow_private_cidrs
        .iter()
        .map(|cidr| {
            if cidr.len() > 64 {
                bail!("push_endpoint_policy_invalid");
            }
            let network: IpNet = cidr
                .parse()
                .map_err(|_| anyhow!("push_endpoint_policy_invalid"))?;
            if network.addr() != network.network()
                || !(iprfc::RFC1918.contains(&network)
                    || iprfc::RFC4193.contains(&network)
                    || iprfc::RFC6598.contains(&network)
                    || (network.network().is_loopback() && network.broadcast().is_loopback()))
            {
                bail!("push_endpoint_policy_invalid");
            }
            Ok(network)
        })
        .collect()
}

fn literal_address(endpoint: &Url) -> Option<IpAddr> {
    let host = endpoint.host_str()?;
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

fn address_allowed(address: IpAddr, private_networks: &[IpNet]) -> bool {
    // Mapped addresses must not turn a disallowed IPv4 endpoint into apparently public IPv6.
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        IpAddr::V4(_) => address,
    };
    if address.is_unspecified()
        || address.is_multicast()
        || match address {
            IpAddr::V4(ip) => {
                ip.is_link_local()
                    || ip.octets() == [100, 100, 100, 200]
                    || ip.octets() == [168, 63, 129, 16]
            }
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local() || ip.segments() == [0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254]
            }
        }
    {
        return false;
    }
    if private_networks
        .iter()
        .any(|network| network.contains(&address))
    {
        return true;
    }
    if iprfc::RFC6890.contains(&address) {
        return false;
    }
    match address {
        IpAddr::V4(ip) => !ip.is_broadcast(),
        IpAddr::V6(ip) => ip.segments()[0] & 0xe000 == 0x2000,
    }
}

struct DnsPermit<'a> {
    active: &'a AtomicUsize,
}

impl<'a> DnsPermit<'a> {
    fn acquire(active: &'a AtomicUsize) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if count < MAX_DNS_LOOKUPS {
                    Some(count + 1)
                } else {
                    None
                }
            })
            .ok()
            .map(|_| Self { active })
    }
}

impl Drop for DnsPermit<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn resolve_endpoint(
    endpoint: &Url,
    policy: &PushEndpointPolicy,
) -> std::result::Result<Vec<SocketAddr>, PushSendOutcome> {
    let host = endpoint.host_str().expect("validated endpoint has a host");
    let port = endpoint
        .port_or_known_default()
        .expect("validated HTTPS endpoint has a port");
    let networks = private_networks(policy).map_err(|error| not_sent(error.to_string()))?;
    if let Some(address) = literal_address(endpoint) {
        return validate_answers([SocketAddr::new(address, port)], &networks);
    }
    let Some(permit) = DnsPermit::acquire(&ACTIVE_DNS_LOOKUPS) else {
        return Err(PushSendOutcome::Retry { after: None });
    };
    let host = host.to_owned();
    let lookup = tokio::task::spawn_blocking(move || {
        // libc resolution cannot be canceled; admission belongs to the real task, not its waiter.
        let _permit = permit;
        match (host.as_str(), port).to_socket_addrs() {
            Ok(answers) => validate_answers(answers, &networks),
            Err(_) => Err(PushSendOutcome::Retry { after: None }),
        }
    });
    match tokio::time::timeout(DNS_TIMEOUT, lookup).await {
        Ok(Ok(outcome)) => outcome,
        _ => Err(PushSendOutcome::Retry { after: None }),
    }
}

fn validate_answers(
    answers: impl IntoIterator<Item = SocketAddr>,
    networks: &[IpNet],
) -> std::result::Result<Vec<SocketAddr>, PushSendOutcome> {
    let mut addresses = Vec::new();
    for address in answers {
        if addresses.len() >= MAX_DNS_ADDRESSES {
            return Err(not_sent("push_dns_answer_limit"));
        }
        if !address_allowed(address.ip(), networks) {
            return Err(not_sent("push_endpoint_address_disallowed"));
        }
        addresses.push(address);
    }
    if addresses.is_empty() {
        return Err(PushSendOutcome::Retry { after: None });
    }
    Ok(addresses)
}

fn pinned_client(endpoint: &Url, addresses: &[SocketAddr]) -> Result<Client, reqwest::Error> {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(0)
        .http1_only()
        .resolve_to_addrs(
            endpoint.host_str().expect("validated endpoint has a host"),
            addresses,
        )
        .build()
}

fn classify_response(status: u16, retry_after: Option<&str>, now: SystemTime) -> PushSendOutcome {
    match status {
        200..=299 => PushSendOutcome::Accepted,
        404 | 410 => PushSendOutcome::Expired,
        429 | 500 | 502 | 503 | 504 => PushSendOutcome::Retry {
            after: retry_after.and_then(|value| parse_retry_after(value, now)),
        },
        status => PushSendOutcome::Rejected { status },
    }
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if value.len() > 128 {
        return None;
    }
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .ok()
            .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    httpdate::parse_http_date(value).ok().map(|date| {
        date.duration_since(now)
            .unwrap_or_default()
            .min(MAX_RETRY_AFTER)
    })
}

fn not_sent(reason: impl Into<String>) -> PushSendOutcome {
    PushSendOutcome::NotSent {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jwt_simple::prelude::ECDSAP256PublicKeyLike;

    fn test_subscription(endpoint: &str) -> PushSubscription {
        let publisher = Publisher::new(&Publisher::generate_key().unwrap()).unwrap();
        PushSubscription {
            endpoint: endpoint.into(),
            p256dh: publisher.public_key(),
            auth: URL_SAFE_NO_PAD.encode([7_u8; 16]),
        }
    }

    fn policy(cidrs: &[&str]) -> PushEndpointPolicy {
        PushEndpointPolicy {
            allow_private_cidrs: cidrs.iter().map(|cidr| (*cidr).into()).collect(),
        }
    }

    #[test]
    fn generated_key_exports_valid_uncompressed_sec1_and_reloads_stably() {
        let pem = Publisher::generate_key().unwrap();
        let publisher = Publisher::new(&pem).unwrap();
        let key = URL_SAFE_NO_PAD.decode(publisher.public_key()).unwrap();
        assert_eq!(key.len(), 65);
        assert_eq!(key[0], 4);
        assert!(ES256PublicKey::from_bytes(&key).is_ok());
        assert_eq!(
            publisher.public_key(),
            Publisher::new(&pem).unwrap().public_key()
        );
        assert!(Publisher::new("invalid private material").is_err());
        assert!(Publisher::new(&"x".repeat(4097)).is_err());
    }

    #[test]
    fn endpoint_validation_rejects_unsafe_normalization_and_authority() {
        for endpoint in [
            "http://example.com/token",
            "https://user:secret@example.com/token",
            "https://@example.com/token",
            "https://example.com/token#secret",
            "https://example.com:0/token",
            " https://example.com/token",
            "https://example.com/\ntoken",
            "https:\\example.com/token",
        ] {
            assert!(parse_endpoint(endpoint).is_err(), "{endpoint:?}");
        }
        assert!(parse_endpoint(&format!("https://example.com/{}", "x".repeat(4096))).is_err());
        assert!(parse_endpoint("https://example.com:8443/token?credential=secret").is_ok());
        assert!(parse_endpoint("https://[2606:4700:4700::1111]/token").is_ok());
    }

    #[test]
    fn public_addresses_and_explicit_private_scopes_are_distinct() {
        let networks = private_networks(&policy(&[
            "127.0.0.1/32",
            "10.1.0.0/16",
            "fd00::/8",
            "100.64.0.0/10",
        ]))
        .unwrap();
        for address in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(address_allowed(address.parse().unwrap(), &[]));
        }
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "fd00::1234",
            "100.100.1.1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!address_allowed(address.parse().unwrap(), &[]));
            assert!(address_allowed(address.parse().unwrap(), &networks));
        }
        for address in [
            "0.0.0.0",
            "169.254.169.254",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "3fff::1",
            "64:ff9b::a00:1",
            "::ffff:169.254.169.254",
            "fd00:ec2::254",
            "100.100.100.200",
            "168.63.129.16",
        ] {
            assert!(
                !address_allowed(address.parse().unwrap(), &networks),
                "{address}"
            );
        }
    }

    #[test]
    fn exceptions_cannot_enable_metadata_reserved_or_overbroad_networks() {
        for cidr in [
            "0.0.0.0/0",
            "::/0",
            "169.254.0.0/16",
            "fe80::/10",
            "192.0.2.0/24",
            "224.0.0.0/4",
            "10.0.0.1/8",
            "not-a-cidr",
            "::ffff:127.0.0.0/104",
        ] {
            assert!(private_networks(&policy(&[cidr])).is_err(), "{cidr}");
        }
        assert!(private_networks(&policy(&["::1/128", "192.168.0.0/16", "172.16.0.0/12"])).is_ok());
        assert!(private_networks(&policy(&["127.0.0.1/32"; 17])).is_err());
    }

    #[test]
    fn abandoned_dns_waiter_retains_admission_until_blocking_task_exits() {
        static ACTIVE: AtomicUsize = AtomicUsize::new(0);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (finished, finished_rx) = std::sync::mpsc::channel();
        let mut permits = (0..MAX_DNS_LOOKUPS)
            .map(|_| DnsPermit::acquire(&ACTIVE).unwrap())
            .collect::<Vec<_>>();
        let permit = permits.pop().unwrap();
        let lookup = runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            let _ = release_rx.recv();
            drop(permit);
            let _ = finished.send(());
        });
        started_rx.recv().unwrap();
        drop(lookup);
        assert!(DnsPermit::acquire(&ACTIVE).is_none());
        release.send(()).unwrap();
        finished_rx.recv().unwrap();
        let permit = DnsPermit::acquire(&ACTIVE).unwrap();
        drop(permit);
        drop(permits);
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    }

    #[test]
    fn ipv6_literals_are_resolved_without_bracketed_hostname_lookup() {
        let endpoint = parse_endpoint("https://[2606:4700:4700::1111]:8443/token").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let addresses = runtime
            .block_on(resolve_endpoint(&endpoint, &policy(&[])))
            .unwrap();
        assert_eq!(
            addresses,
            ["[2606:4700:4700::1111]:8443".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn mixed_dns_answers_and_answer_floods_fail_closed() {
        let public = "8.8.8.8:443".parse::<SocketAddr>().unwrap();
        let private = "127.0.0.1:443".parse::<SocketAddr>().unwrap();
        assert!(matches!(
            validate_answers([public, private], &[]),
            Err(PushSendOutcome::NotSent { .. })
        ));
        assert!(matches!(
            validate_answers([private, public], &[]),
            Err(PushSendOutcome::NotSent { .. })
        ));
        assert!(validate_answers([public; MAX_DNS_ADDRESSES], &[]).is_ok());
        assert!(matches!(
            validate_answers([public; MAX_DNS_ADDRESSES + 1], &[]),
            Err(PushSendOutcome::NotSent { .. })
        ));
        assert!(matches!(
            validate_answers([], &[]),
            Err(PushSendOutcome::Retry { .. })
        ));
    }

    #[test]
    fn malformed_subscription_keys_are_rejected_without_endpoint_disclosure() {
        let mut subscription = test_subscription("https://example.com/secret-endpoint");
        subscription.auth = "invalid-secret".into();
        let error = validate_subscription(&subscription, &policy(&[])).unwrap_err();
        assert_eq!(error.to_string(), "push_subscription_keys_invalid");
        subscription.auth = URL_SAFE_NO_PAD.encode([7_u8; 16]);
        subscription.p256dh = URL_SAFE_NO_PAD.encode([4_u8; 65]);
        assert!(validate_subscription(&subscription, &policy(&[])).is_err());
        let subscription = test_subscription("https://127.0.0.1/secret");
        assert!(validate_subscription(&subscription, &policy(&[])).is_err());
        assert!(validate_subscription(&subscription, &policy(&["127.0.0.1/32"])).is_ok());
    }

    #[test]
    fn vapid_audience_uses_exact_origin_and_request_has_explicit_delivery_limits() {
        let publisher = Publisher::new(&Publisher::generate_key().unwrap()).unwrap();
        for endpoint in [
            "https://example.com:8443/token",
            "https://[2606:4700:4700::1111]:8443/token",
            "https://example.com:443/token",
        ] {
            let subscription = test_subscription(endpoint);
            let endpoint = validate(&subscription, &policy(&[])).unwrap();
            let request = publisher
                .build_request(
                    &Client::new(),
                    &subscription,
                    endpoint.clone(),
                    b"notification",
                    300,
                )
                .unwrap();
            assert_eq!(request.headers()["ttl"], "300");
            assert_eq!(request.headers()["urgency"], "normal");
            assert_eq!(request.headers()["content-encoding"], "aes128gcm");
            let authorization = request.headers().get(AUTHORIZATION).unwrap();
            assert!(authorization.is_sensitive());
            let authorization = authorization.to_str().unwrap();
            let token = authorization
                .strip_prefix("vapid t=")
                .unwrap()
                .split(',')
                .next()
                .unwrap();
            let claims: serde_json::Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(token.split('.').nth(1).unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(claims["aud"], endpoint.origin().ascii_serialization());
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let expiry = claims["exp"].as_u64().unwrap();
            assert!(expiry >= now + 3500 && expiry <= now + 3600);
            let public = ES256PublicKey::from_bytes(
                &URL_SAFE_NO_PAD.decode(publisher.public_key()).unwrap(),
            )
            .unwrap();
            assert!(
                public
                    .verify_token::<serde_json::Value>(token, None)
                    .is_ok()
            );
        }
    }

    #[test]
    fn payload_and_ttl_validation_happen_before_network_dispatch() {
        let publisher = Publisher::new(&Publisher::generate_key().unwrap()).unwrap();
        let subscription = test_subscription("https://unresolved.invalid/secret");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            matches!(runtime.block_on(publisher.send(&subscription, &policy(&[]), &[0; MAX_PAYLOAD_BYTES + 1], 300)), PushSendOutcome::NotSent { reason } if reason == "push_payload_too_large")
        );
        for ttl in [0, MAX_TTL_SECONDS + 1] {
            assert!(
                matches!(runtime.block_on(publisher.send(&subscription, &policy(&[]), b"payload", ttl)), PushSendOutcome::NotSent { reason } if reason == "push_ttl_invalid")
            );
        }
        let endpoint = parse_endpoint(&subscription.endpoint).unwrap();
        assert!(
            publisher
                .build_request(
                    &Client::new(),
                    &subscription,
                    endpoint,
                    &[0; MAX_PAYLOAD_BYTES],
                    300
                )
                .is_ok()
        );
    }

    #[test]
    fn response_outcomes_never_claim_display_or_retry_unknown_rejections() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for status in [200, 201, 202, 204, 299] {
            assert!(matches!(
                classify_response(status, None, now),
                PushSendOutcome::Accepted
            ));
        }
        for status in [404, 410] {
            assert!(matches!(
                classify_response(status, None, now),
                PushSendOutcome::Expired
            ));
        }
        for status in [429, 500, 502, 503, 504] {
            assert!(
                matches!(classify_response(status, Some("60"), now), PushSendOutcome::Retry { after: Some(delay) } if delay == Duration::from_secs(60))
            );
        }
        for status in [301, 307, 400, 401, 403, 413, 501] {
            assert!(
                matches!(classify_response(status, None, now), PushSendOutcome::Rejected { status: actual } if actual == status)
            );
        }
        assert_eq!(parse_retry_after("99999999", now), Some(MAX_RETRY_AFTER));
        assert_eq!(
            parse_retry_after("Wed, 15 Nov 2023 22:13:20 GMT", now),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            parse_retry_after("Tue, 14 Nov 2023 22:12:20 GMT", now),
            Some(Duration::ZERO)
        );
        for invalid in ["-1", "1.5", "18446744073709551616", "not-a-date"] {
            assert_eq!(parse_retry_after(invalid, now), None);
        }
        assert_eq!(parse_retry_after(&"9".repeat(129), now), None);
    }
}
