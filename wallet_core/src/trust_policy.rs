//! Optional wallet acceptance rules over originator facts and group responses.
//! These decisions do not validate protocol state or sign receive blocks.

use burst_groups::evaluation::MAX_CONCURRENT_REQUESTS;
use burst_groups::{GroupClient, GroupEvaluation, GroupRegistry};
use burst_types::WalletAddress;
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};

/// A wallet's trust policy for accepting TRST.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TrustPolicy {
    /// Apply no additional group restriction; protocol validation still applies.
    AcceptAll,
    /// Require the originator to be a member of at least one trusted group.
    RequireGroup { trusted_groups: Vec<String> },
    /// Require membership in every selected group; an empty list requires review.
    RequireAllGroups { trusted_groups: Vec<String> },
    /// All rules must pass. Any rejection wins over warnings; no rules accepts.
    Custom { rules: Vec<TrustRule> },
    /// Explicitly allow selected groups to vouch through other registered groups.
    RequireGroupGraph {
        trusted_groups: Vec<String>,
        require_all: bool,
        max_hops: u8,
    },
}

/// A single rule in a custom trust policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TrustRule {
    /// Originator must be a member of this group.
    GroupMembership { group_id: String },
    /// Originator must have been verified for at least this many seconds.
    MinVerificationAge { seconds: u64 },
    /// Maximum unknown-origin share in basis points (10,000 = 100%).
    MaxUnknownOriginProportion { max_bps: u32 },
}

/// Facts for the actual TRST originator and payment, not the immediate sender.
/// Missing facts require review. Group membership never supplies protocol state.
#[derive(Clone, Debug, Default)]
pub struct TrustContext {
    pub verification_age_seconds: Option<u64>,
    pub unknown_origins: Option<UnknownOrigins>,
}

#[derive(Clone, Debug)]
pub struct UnknownOrigins {
    pub amount: u128,
    pub total: u128,
}

/// Only Accept permits automatic acceptance. Warn requires the user's fallback
/// decision; it is neither a rejection nor permission to sign a receive block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustDecision {
    Accept,
    Reject { reason: String },
    Warn { reason: String },
}

pub struct GroupCheck {
    pub decision: TrustDecision,
    /// Warning caused only by service unavailability (`GroupError::Unreachable`),
    /// including HTTP server errors. Malformed responses and unknown IDs do not qualify.
    pub offline_only: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineFallback {
    /// Waive group checks only for service-unavailability warnings. Does not run
    /// protocol validation; the caller must already have established it.
    Protocol,
    /// Keep service outages as warnings for explicit user review.
    Review,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PaymentTrustResult {
    pub decision: TrustDecision,
    pub used_protocol_fallback: bool,
    /// Originator checks completed before return, including a decisive rejection.
    /// A whole-payment timeout returns zero rather than partial progress.
    pub checked_originators: usize,
}

pub struct GroupRequirements<'a> {
    pub group_ids: &'a [String],
    pub require_all: bool,
    /// Zero preserves direct membership checks. Endpoints remain registry-pinned.
    pub max_hops: u8,
}

pub async fn evaluate_payment_groups(
    payment: &crate::provenance::PaymentOrigins,
    group_ids: &[String],
    require_all: bool,
    fallback: OfflineFallback,
    client: &GroupClient,
    registry: &GroupRegistry,
) -> PaymentTrustResult {
    evaluate_payment_group_graph(
        payment,
        GroupRequirements {
            group_ids,
            require_all,
            max_hops: 0,
        },
        fallback,
        client,
        registry,
    )
    .await
}

fn payment_creators(
    payment: &crate::provenance::PaymentOrigins,
) -> Result<std::collections::BTreeSet<&str>, &'static str> {
    if !payment.confirmed
        || !payment.provenance.complete
        || !payment.provenance.confirmed
        || payment.provenance.origins.is_empty()
        || payment
            .provenance
            .origins
            .iter()
            .any(|burn| !burn.confirmed)
    {
        return Err("complete, confirmed payment origins are required for creator checking");
    }
    // Revoked contributions have already been removed by protocol accounting.
    // Their historical presence must not taint the surviving honest value.
    let creators: std::collections::BTreeSet<_> = payment
        .provenance
        .origins
        .iter()
        .filter(|burn| !burn.revoked)
        .map(|burn| burn.originator.as_str())
        .collect();
    if creators.is_empty() {
        return Err("no surviving originators to check");
    }
    if creators.len() > 256 {
        return Err("creator inspection limit exceeded; review the payment manually");
    }
    if creators
        .iter()
        .any(|address| crate::signed_client::parse_address(address).is_err())
    {
        return Err("invalid original creator address");
    }
    Ok(creators)
}

/// Require every surviving creator to be personally trusted. No membership service is queried.
pub fn evaluate_payment_creators(
    payment: &crate::provenance::PaymentOrigins,
    trusted: &[String],
) -> PaymentTrustResult {
    let decision = (|| {
        if trusted.is_empty()
            || trusted.len() > 256
            || trusted
                .iter()
                .any(|address| crate::signed_client::parse_address(address).is_err())
        {
            return Err("choose between 1 and 256 valid creator addresses".into());
        }
        let creators = payment_creators(payment).map_err(str::to_owned)?;
        let allowed: std::collections::BTreeSet<_> = trusted.iter().map(String::as_str).collect();
        let count = creators.len();
        if let Some(creator) = creators.iter().find(|creator| !allowed.contains(**creator)) {
            return Ok((
                TrustDecision::Reject {
                    reason: format!("Original creator {creator} is not on your trusted list"),
                },
                count,
            ));
        }
        Ok((TrustDecision::Accept, count))
    })();
    let (decision, checked_originators) =
        decision.unwrap_or_else(|reason: String| (TrustDecision::Warn { reason }, 0));
    PaymentTrustResult {
        decision,
        checked_originators,
        used_protocol_fallback: false,
    }
}

/// Check every distinct surviving originator in the supplied payment report.
/// Completeness and confirmation flags are prerequisites, not proofs verified here.
/// The caller authenticates provenance and applies protocol accounting first.
/// The network evaluation has a 30-second deadline; initial report scanning is
/// synchronous. Any originator rejection rejects the payment.
pub async fn evaluate_payment_group_graph(
    payment: &crate::provenance::PaymentOrigins,
    rules: GroupRequirements<'_>,
    fallback: OfflineFallback,
    client: &GroupClient,
    registry: &GroupRegistry,
) -> PaymentTrustResult {
    let hold = |reason: &str| PaymentTrustResult {
        decision: TrustDecision::Warn {
            reason: reason.into(),
        },
        used_protocol_fallback: false,
        checked_originators: 0,
    };
    let creators = match payment_creators(payment) {
        Ok(creators) => creators,
        Err(reason) => return hold(reason),
    };
    if rules.group_ids.len() > 32 || rules.max_hops > burst_groups::trust_graph::MAX_TRUST_HOPS {
        return hold("group inspection limit exceeded; review the payment manually");
    }
    let queries = client.evaluation();
    let evaluation =
        async {
            let mut warning = None;
            let mut used_protocol_fallback = false;
            let mut checked_originators = 0;
            let mut results = stream::iter(creators.into_iter().map(|creator| {
                let queries = &queries;
                let rules = &rules;
                async move {
                    check_membership(
                        rules.group_ids,
                        rules.require_all,
                        &WalletAddress::new(creator),
                        queries,
                        registry,
                        rules.max_hops,
                    )
                    .await
                }
            }))
            .buffer_unordered(MAX_CONCURRENT_REQUESTS);
            while let Some(result) = results.next().await {
                checked_originators += 1;
                match result.decision {
                    TrustDecision::Accept => {}
                    TrustDecision::Reject { reason } => {
                        return PaymentTrustResult {
                            decision: TrustDecision::Reject { reason },
                            used_protocol_fallback: false,
                            checked_originators,
                        }
                    }
                    TrustDecision::Warn { reason } => {
                        if result.offline_only && matches!(fallback, OfflineFallback::Protocol) {
                            used_protocol_fallback = true;
                        } else if warning.is_none() {
                            warning = Some(reason);
                        }
                    }
                }
            }
            PaymentTrustResult {
                decision: warning.clone().map_or(TrustDecision::Accept, |reason| {
                    TrustDecision::Warn { reason }
                }),
                used_protocol_fallback: used_protocol_fallback && warning.is_none(),
                checked_originators,
            }
        };
    crate::runtime::timeout(std::time::Duration::from_secs(30), evaluation)
        .await
        .unwrap_or_else(|_| hold("group inspection timed out; review the payment manually"))
}

/// Evaluate one originator against wallet policy, assuming protocol checks occur separately.
/// Group results are shared only within this evaluation, never across payments.
/// Other rules use the supplied facts; this function does not authenticate them.
pub async fn evaluate_policy(
    policy: &TrustPolicy,
    originator: &WalletAddress,
    group_client: &GroupClient,
    registry: &GroupRegistry,
    context: &TrustContext,
) -> TrustDecision {
    match policy {
        TrustPolicy::AcceptAll => TrustDecision::Accept,
        TrustPolicy::RequireGroup { trusted_groups } => {
            evaluate_groups(trusted_groups, false, originator, group_client, registry).await
        }
        TrustPolicy::RequireAllGroups { trusted_groups } => {
            evaluate_groups(trusted_groups, true, originator, group_client, registry).await
        }
        TrustPolicy::RequireGroupGraph {
            trusted_groups,
            require_all,
            max_hops,
        } => {
            check_group_membership_with_depth(
                trusted_groups,
                *require_all,
                originator,
                group_client,
                registry,
                *max_hops,
            )
            .await
            .decision
        }
        TrustPolicy::Custom { rules } => {
            let mut warning = None;
            for rule in rules {
                let decision = match rule {
                    TrustRule::GroupMembership { group_id } => {
                        evaluate_groups(
                            std::slice::from_ref(group_id),
                            true,
                            originator,
                            group_client,
                            registry,
                        )
                        .await
                    }
                    TrustRule::MinVerificationAge { seconds } => {
                        match context.verification_age_seconds {
                            Some(age) if age >= *seconds => TrustDecision::Accept,
                            Some(_) => TrustDecision::Reject {
                                reason: "originator was verified too recently".into(),
                            },
                            None => TrustDecision::Warn {
                                reason: "originator verification age is unavailable".into(),
                            },
                        }
                    }
                    TrustRule::MaxUnknownOriginProportion { max_bps } => {
                        evaluate_unknown_origins(*max_bps, context.unknown_origins.as_ref())
                    }
                };
                match decision {
                    TrustDecision::Reject { .. } => return decision,
                    TrustDecision::Warn { .. } => {
                        if warning.is_none() {
                            warning = Some(decision);
                        }
                    }
                    TrustDecision::Accept => {}
                }
            }
            warning.unwrap_or(TrustDecision::Accept)
        }
    }
}

async fn evaluate_groups(
    group_ids: &[String],
    require_all: bool,
    originator: &WalletAddress,
    client: &GroupClient,
    registry: &GroupRegistry,
) -> TrustDecision {
    check_group_membership(group_ids, require_all, originator, client, registry)
        .await
        .decision
}

pub async fn check_group_membership(
    group_ids: &[String],
    require_all: bool,
    originator: &WalletAddress,
    client: &GroupClient,
    registry: &GroupRegistry,
) -> GroupCheck {
    check_group_membership_with_depth(group_ids, require_all, originator, client, registry, 0).await
}

/// Query the registered groups, directly or through bounded trust paths.
/// For any-group policy, one positive result suffices despite unavailable peers.
/// For all-group policy, one negative result rejects despite unavailable peers.
/// Otherwise missing evidence warns; the overall timeout is not an offline-only
/// warning and cannot trigger automatic protocol fallback.
pub async fn check_group_membership_with_depth(
    group_ids: &[String],
    require_all: bool,
    originator: &WalletAddress,
    client: &GroupClient,
    registry: &GroupRegistry,
    max_hops: u8,
) -> GroupCheck {
    check_membership(
        group_ids,
        require_all,
        originator,
        &client.evaluation(),
        registry,
        max_hops,
    )
    .await
}

async fn check_membership(
    group_ids: &[String],
    require_all: bool,
    originator: &WalletAddress,
    queries: &GroupEvaluation<'_>,
    registry: &GroupRegistry,
    max_hops: u8,
) -> GroupCheck {
    if group_ids.is_empty()
        || group_ids.len() > 32
        || max_hops > burst_groups::trust_graph::MAX_TRUST_HOPS
    {
        return GroupCheck {
            offline_only: false,
            decision: TrustDecision::Warn {
                reason: "choose 1–32 trusted groups and a trust depth from 0 to 8, or use protocol checks only".into(),
            },
        };
    }
    let check = async {
        let mut unavailable = Vec::new();
        let mut offline_only = true;
        let mut endpoints = Vec::new();
        for id in group_ids {
            if let Some(group) = registry.get(id) {
                endpoints.push((id.as_str(), group.endpoint_url.as_str()));
            } else {
                unavailable.push(id.clone());
                offline_only = false;
            }
        }
        let mut responses = stream::iter(endpoints.into_iter().map(|(id, endpoint)| async move {
            let response = if max_hops == 0 {
                queries
                    .verify_member(endpoint, originator.as_str())
                    .await
                    .map(|member| member.valid)
            } else {
                queries
                    .verify_member_through_groups(registry, id, originator.as_str(), max_hops)
                    .await
                    .map(|path| path.is_some())
            };
            (id.to_owned(), response)
        }))
        .buffer_unordered(MAX_CONCURRENT_REQUESTS);
        let mut has_member = false;
        let mut has_nonmember = false;
        while let Some((id, response)) = responses.next().await {
            match response {
                Ok(true) => has_member = true,
                Ok(false) => has_nonmember = true,
                Err(error) => {
                    offline_only &= matches!(error, burst_groups::GroupError::Unreachable(_));
                    unavailable.push(id);
                }
            }
            if (!require_all && has_member) || (require_all && has_nonmember) {
                break;
            }
        }
        let decision = if (!require_all && has_member)
            || (require_all && !has_nonmember && unavailable.is_empty())
        {
            TrustDecision::Accept
        } else if (require_all && has_nonmember) || unavailable.is_empty() {
            TrustDecision::Reject {
                reason: "originator does not meet the selected group membership rules".into(),
            }
        } else {
            TrustDecision::Warn {
                reason: format!(
                    "could not check trusted groups: {}; review before accepting",
                    unavailable.join(", ")
                ),
            }
        };
        GroupCheck {
            offline_only: offline_only && matches!(decision, TrustDecision::Warn { .. }),
            decision,
        }
    };
    crate::runtime::timeout(std::time::Duration::from_secs(30), check)
        .await
        .unwrap_or_else(|_| GroupCheck {
            offline_only: false,
            decision: TrustDecision::Warn {
                reason: "group inspection timed out; review before accepting".into(),
            },
        })
}

fn evaluate_unknown_origins(max_bps: u32, origins: Option<&UnknownOrigins>) -> TrustDecision {
    if max_bps > 10_000 {
        return TrustDecision::Warn {
            reason: "unknown-origin limit must be between 0 and 10000 basis points".into(),
        };
    }
    let Some(origins) = origins else {
        return TrustDecision::Warn {
            reason: "payment origin proportions are unavailable".into(),
        };
    };
    if origins.total == 0 || origins.amount > origins.total {
        return TrustDecision::Warn {
            reason: "payment origin proportions are inconsistent".into(),
        };
    }
    // Equivalent to total * max_bps / 10000 without overflowing u128.
    let allowed = (origins.total / 10_000) * u128::from(max_bps)
        + (origins.total % 10_000) * u128::from(max_bps) / 10_000;
    if origins.amount <= allowed {
        TrustDecision::Accept
    } else {
        TrustDecision::Reject {
            reason: "too much TRST comes from unknown origins".into(),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "trust_policy_graph_tests.rs"]
mod graph_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use burst_groups::GroupInfo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn payment() -> crate::provenance::PaymentOrigins {
        use crate::provenance::{BurnOrigin, OriginReport, PaymentOrigins};
        PaymentOrigins {
            payment: "04".repeat(32),
            confirmed: true,
            provenance: OriginReport {
                origin: "03".repeat(32),
                complete: true,
                confirmed: true,
                origins: (1..=2)
                    .map(|id| BurnOrigin {
                        hash: format!("{id:02x}").repeat(32),
                        originator: burst_crypto::derive_address(
                            &burst_crypto::keypair_from_seed(&[id; 32]).public,
                        )
                        .to_string(),
                        timestamp: 100,
                        confirmed: true,
                        revoked: false,
                    })
                    .collect(),
            },
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn personal_trust_checks_every_surviving_creator() {
        let mut report = payment();
        let trusted: Vec<_> = report
            .provenance
            .origins
            .iter()
            .map(|o| o.originator.clone())
            .collect();
        assert_eq!(
            evaluate_payment_creators(&report, &trusted).decision,
            TrustDecision::Accept
        );
        assert!(matches!(
            evaluate_payment_creators(&report, &trusted[..1]).decision,
            TrustDecision::Reject { .. }
        ));
        report.provenance.origins[1].revoked = true;
        let result = evaluate_payment_creators(&report, &trusted[..1]);
        assert_eq!(result.decision, TrustDecision::Accept);
        assert_eq!(result.checked_originators, 1);
        assert!(!result.used_protocol_fallback);
        report
            .provenance
            .origins
            .push(report.provenance.origins[0].clone());
        assert_eq!(
            evaluate_payment_creators(&report, &trusted).checked_originators,
            1
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn personal_trust_holds_incomplete_reports_and_invalid_lists() {
        let baseline = payment();
        let trusted: Vec<_> = baseline
            .provenance
            .origins
            .iter()
            .map(|o| o.originator.clone())
            .collect();
        for case in 0..6 {
            let mut report = baseline.clone();
            match case {
                0 => report.confirmed = false,
                1 => report.provenance.complete = false,
                2 => report.provenance.confirmed = false,
                3 => report.provenance.origins[0].confirmed = false,
                4 => report.provenance.origins.clear(),
                _ => report
                    .provenance
                    .origins
                    .iter_mut()
                    .for_each(|o| o.revoked = true),
            }
            assert!(matches!(
                evaluate_payment_creators(&report, &trusted).decision,
                TrustDecision::Warn { .. }
            ));
        }
        for list in [
            vec![],
            vec!["invalid".into()],
            vec![trusted[0].clone(); 257],
        ] {
            assert!(matches!(
                evaluate_payment_creators(&baseline, &list).decision,
                TrustDecision::Warn { .. }
            ));
        }
    }

    async fn membership_replies(
        replies: Vec<(u16, &str)>,
    ) -> (GroupRegistry, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut registry = GroupRegistry::new();
        registry
            .register(GroupInfo {
                id: "local".into(),
                name: "Local".into(),
                description: String::new(),
                member_count: 0,
                endpoint_url: format!("http://{}", listener.local_addr().unwrap()),
            })
            .unwrap();
        let replies: Vec<_> = replies
            .into_iter()
            .map(|(status, body)| (status, body.to_string()))
            .collect();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 8192);
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    header.push(byte[0]);
                }
                requests.push(
                    String::from_utf8(header)
                        .unwrap()
                        .lines()
                        .next()
                        .unwrap()
                        .to_string(),
                );
                socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        (registry, server)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn payment_checks_distinct_original_creators_live_not_sender_or_merge_account() {
        let mut payment = payment();
        let mut duplicate = payment.provenance.origins[0].clone();
        duplicate.hash = "05".repeat(32);
        payment.provenance.origins.push(duplicate);
        let (registry, server) =
            membership_replies(vec![(200, r#"{"valid":true,"score":1}"#); 4]).await;
        let client = GroupClient::new().unwrap();
        for _ in 0..2 {
            let result = evaluate_payment_groups(
                &payment,
                &["local".into()],
                false,
                OfflineFallback::Protocol,
                &client,
                &registry,
            )
            .await;
            assert_eq!(result.decision, TrustDecision::Accept);
            assert_eq!(result.checked_originators, 2);
            assert!(!result.used_protocol_fallback);
        }
        let requests = server.await.unwrap();
        let mut expected: Vec<_> = payment.provenance.origins[..2]
            .iter()
            .map(|burn| format!("GET /verify/{} HTTP/1.1", burn.originator))
            .collect();
        expected.sort();
        for batch in requests.chunks_exact(2) {
            let mut batch = batch.to_vec();
            batch.sort();
            assert_eq!(batch, expected);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn revoked_history_does_not_taint_surviving_honest_origins() {
        let mut payment = payment();
        payment.provenance.origins[0].revoked = true;
        let (registry, server) =
            membership_replies(vec![(200, r#"{"valid":true,"score":1}"#)]).await;
        let result = evaluate_payment_groups(
            &payment,
            &["local".into()],
            true,
            OfflineFallback::Review,
            &GroupClient::new().unwrap(),
            &registry,
        )
        .await;
        assert_eq!(result.decision, TrustDecision::Accept);
        assert_eq!(result.checked_originators, 1);
        assert_eq!(
            server.await.unwrap(),
            vec![format!(
                "GET /verify/{} HTTP/1.1",
                payment.provenance.origins[1].originator
            )]
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn only_actual_group_outages_can_use_the_explicit_protocol_fallback() {
        for (status, body, fallback, accept) in [
            (503, "", OfflineFallback::Protocol, true),
            (503, "", OfflineFallback::Review, false),
            (404, "", OfflineFallback::Protocol, false),
            (200, "{}", OfflineFallback::Protocol, false),
            (
                200,
                r#"{"valid":false,"score":1}"#,
                OfflineFallback::Protocol,
                false,
            ),
        ] {
            let mut payment = payment();
            payment.provenance.origins.truncate(1);
            let (registry, server) = membership_replies(vec![(status, body)]).await;
            let result = evaluate_payment_groups(
                &payment,
                &["local".into()],
                false,
                fallback,
                &GroupClient::new().unwrap(),
                &registry,
            )
            .await;
            assert_eq!(
                result.decision == TrustDecision::Accept,
                accept,
                "{status}: {body}"
            );
            assert_eq!(result.used_protocol_fallback, accept);
            server.await.unwrap();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn an_early_warning_or_fallback_cannot_hide_a_later_negative_origin() {
        for first in [(200, "{}"), (503, "")] {
            let (registry, server) =
                membership_replies(vec![first, (200, r#"{"valid":false,"score":1}"#)]).await;
            let result = evaluate_payment_groups(
                &payment(),
                &["local".into()],
                false,
                OfflineFallback::Protocol,
                &GroupClient::new().unwrap(),
                &registry,
            )
            .await;
            assert!(matches!(result.decision, TrustDecision::Reject { .. }));
            assert!(!result.used_protocol_fallback);
            assert!((1..=2).contains(&result.checked_originators));
            server.abort();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn missing_or_unconfirmed_evidence_and_configuration_never_trigger_fallback() {
        let client = GroupClient::new().unwrap();
        let registry = GroupRegistry::new();
        for groups in [vec![], vec!["missing".into()]] {
            let result = evaluate_payment_groups(
                &payment(),
                &groups,
                false,
                OfflineFallback::Protocol,
                &client,
                &registry,
            )
            .await;
            assert!(matches!(result.decision, TrustDecision::Warn { .. }));
            assert!(!result.used_protocol_fallback);
        }
        let mut unconfirmed = payment();
        unconfirmed.confirmed = false;
        let mut revoked = payment();
        revoked
            .provenance
            .origins
            .iter_mut()
            .for_each(|burn| burn.revoked = true);
        let mut empty = payment();
        empty.provenance.origins.clear();
        for payment in [unconfirmed, revoked, empty] {
            let result = evaluate_payment_groups(
                &payment,
                &["local".into()],
                false,
                OfflineFallback::Protocol,
                &client,
                &registry,
            )
            .await;
            assert!(matches!(result.decision, TrustDecision::Warn { .. }));
            assert_eq!(result.checked_originators, 0);
            assert!(!result.used_protocol_fallback);
        }
    }

    async fn group(
        registry: &mut GroupRegistry,
        id: &str,
        body: &str,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        registry
            .register(GroupInfo {
                id: id.into(),
                name: id.into(),
                description: String::new(),
                member_count: 0,
                endpoint_url: format!("http://{}/community", listener.local_addr().unwrap()),
            })
            .unwrap();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let length = socket.read(&mut request).await.unwrap();
            assert!(std::str::from_utf8(&request[..length])
                .unwrap()
                .starts_with("GET /community/verify/brst_originator HTTP/1.1\r\n"));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        })
    }

    async fn evaluate(
        policy: &TrustPolicy,
        registry: &GroupRegistry,
        context: &TrustContext,
    ) -> TrustDecision {
        evaluate_policy(
            policy,
            &WalletAddress::new("brst_originator"),
            &GroupClient::new().unwrap(),
            registry,
            context,
        )
        .await
    }

    #[tokio::test]
    async fn empty_required_group_list_is_not_accept_all() {
        for policy in [
            TrustPolicy::RequireGroup {
                trusted_groups: vec![],
            },
            TrustPolicy::RequireAllGroups {
                trusted_groups: vec![],
            },
        ] {
            assert!(matches!(
                evaluate(&policy, &GroupRegistry::new(), &TrustContext::default()).await,
                TrustDecision::Warn { .. }
            ));
        }
        assert_eq!(
            evaluate(
                &TrustPolicy::AcceptAll,
                &GroupRegistry::new(),
                &TrustContext::default()
            )
            .await,
            TrustDecision::Accept
        );
    }

    #[tokio::test]
    async fn any_trusted_group_can_vouch_when_another_is_unavailable() {
        let mut registry = GroupRegistry::new();
        let task = group(&mut registry, "community", r#"{"valid":true,"score":0}"#).await;
        let policy = TrustPolicy::RequireGroup {
            trusted_groups: vec!["offline".into(), "community".into()],
        };
        assert_eq!(
            evaluate(&policy, &registry, &TrustContext::default()).await,
            TrustDecision::Accept
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn negative_membership_and_unavailable_evidence_are_distinct() {
        for (require_all, expected_rejection) in [(false, false), (true, true)] {
            let mut registry = GroupRegistry::new();
            let task = group(&mut registry, "community", r#"{"valid":false,"score":1}"#).await;
            let trusted_groups = vec!["offline".into(), "community".into()];
            let policy = if require_all {
                TrustPolicy::RequireAllGroups { trusted_groups }
            } else {
                TrustPolicy::RequireGroup { trusted_groups }
            };
            let result = evaluate(&policy, &registry, &TrustContext::default()).await;
            assert_eq!(
                matches!(result, TrustDecision::Reject { .. }),
                expected_rejection
            );
            assert!(!matches!(result, TrustDecision::Accept));
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn all_groups_requires_every_membership() {
        for valid in [true, false] {
            let mut registry = GroupRegistry::new();
            let first = group(&mut registry, "one", r#"{"valid":true,"score":0.5}"#).await;
            let second = group(
                &mut registry,
                "two",
                &format!(r#"{{"valid":{valid},"score":0.5}}"#),
            )
            .await;
            let policy = TrustPolicy::RequireAllGroups {
                trusted_groups: vec!["one".into(), "two".into()],
            };
            assert_eq!(
                matches!(
                    evaluate(&policy, &registry, &TrustContext::default()).await,
                    TrustDecision::Accept
                ),
                valid
            );
            first.await.unwrap();
            second.await.unwrap();
        }
    }

    #[tokio::test]
    async fn malformed_group_data_requires_review() {
        let mut registry = GroupRegistry::new();
        let task = group(&mut registry, "community", r#"{"valid":true,"score":999}"#).await;
        let policy = TrustPolicy::RequireGroup {
            trusted_groups: vec!["community".into()],
        };
        assert!(matches!(
            evaluate(&policy, &registry, &TrustContext::default()).await,
            TrustDecision::Warn { .. }
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn later_custom_failure_is_not_hidden_by_an_earlier_warning() {
        let policy = TrustPolicy::Custom {
            rules: vec![
                TrustRule::GroupMembership {
                    group_id: "offline".into(),
                },
                TrustRule::MinVerificationAge { seconds: 100 },
            ],
        };
        let context = TrustContext {
            verification_age_seconds: Some(99),
            ..Default::default()
        };
        assert!(matches!(
            evaluate(&policy, &GroupRegistry::new(), &context).await,
            TrustDecision::Reject { .. }
        ));
    }

    #[tokio::test]
    async fn every_custom_rule_must_pass_and_age_boundary_is_inclusive() {
        let mut registry = GroupRegistry::new();
        let task = group(&mut registry, "community", r#"{"valid":true,"score":1}"#).await;
        let policy = TrustPolicy::Custom {
            rules: vec![
                TrustRule::GroupMembership {
                    group_id: "community".into(),
                },
                TrustRule::MinVerificationAge { seconds: 100 },
                TrustRule::MaxUnknownOriginProportion { max_bps: 2500 },
            ],
        };
        let context = TrustContext {
            verification_age_seconds: Some(100),
            unknown_origins: Some(UnknownOrigins {
                amount: 1,
                total: 4,
            }),
        };
        assert_eq!(
            evaluate(&policy, &registry, &context).await,
            TrustDecision::Accept
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn missing_age_requires_review_not_rejection_or_acceptance() {
        let policy = TrustPolicy::Custom {
            rules: vec![TrustRule::MinVerificationAge { seconds: 1 }],
        };
        assert!(matches!(
            evaluate(&policy, &GroupRegistry::new(), &TrustContext::default()).await,
            TrustDecision::Warn { .. }
        ));
    }

    #[test]
    fn origin_proportion_uses_exact_units_and_cannot_overflow() {
        for (amount, total, max_bps, accepted) in [
            (1, 3, 3333, false),
            (1, 3, 3334, true),
            (0, 3, 0, true),
            (1, 3, 0, false),
            (u128::MAX, u128::MAX, 10000, true),
            (u128::MAX, u128::MAX, 9999, false),
        ] {
            assert_eq!(
                matches!(
                    evaluate_unknown_origins(max_bps, Some(&UnknownOrigins { amount, total })),
                    TrustDecision::Accept
                ),
                accepted
            );
        }
        for (max_bps, facts) in [
            (
                10001,
                Some(UnknownOrigins {
                    amount: 0,
                    total: 1,
                }),
            ),
            (1, None),
            (
                1,
                Some(UnknownOrigins {
                    amount: 0,
                    total: 0,
                }),
            ),
            (
                1,
                Some(UnknownOrigins {
                    amount: 2,
                    total: 1,
                }),
            ),
        ] {
            assert!(matches!(
                evaluate_unknown_origins(max_bps, facts.as_ref()),
                TrustDecision::Warn { .. }
            ));
        }
    }
}
