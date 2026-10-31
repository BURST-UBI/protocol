//! Optional, live group-to-group trust. Registered endpoints are never replaced
//! by a response, and discovering a group is not itself a reason to trust it.

use crate::{GroupEvaluation, GroupClient, GroupError, GroupRegistry, MemberStatus};
use serde::Deserialize;
use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

pub const MAX_TRUST_HOPS: u8 = 8;
const MAX_VISITED_GROUPS: usize = 64;
const MAX_ENDORSEMENTS: usize = 32;

#[derive(Debug)]
pub struct MembershipPath {
    /// The selected root, each endorsing group, then the member's group.
    pub groups: Vec<String>,
    pub member: MemberStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Endorsements {
    version: u32,
    groups: Vec<String>,
}

impl GroupClient {
    pub async fn verify_member_through_groups(
        &self, registry: &GroupRegistry, root: &str, wallet_id: &str, max_hops: u8,
    ) -> Result<Option<MembershipPath>, GroupError> {
        self.evaluation().verify_member_through_groups(registry, root, wallet_id, max_hops).await
    }
}

impl GroupEvaluation<'_> {
    /// A zero-hop policy checks direct membership only. Greater depths explicitly
    /// authorize disclosure to registered groups vouched for by the selected root.
    /// No membership or endorsement response survives this evaluation.
    pub async fn verify_member_through_groups(
        &self,
        registry: &GroupRegistry,
        root: &str,
        wallet_id: &str,
        max_hops: u8,
    ) -> Result<Option<MembershipPath>, GroupError> {
        if max_hops > MAX_TRUST_HOPS {
            return Err(GroupError::Other(format!(
                "group trust depth must not exceed {MAX_TRUST_HOPS}"
            )));
        }
        #[cfg(target_arch = "wasm32")]
        use burst_web_runtime::timeout;
        #[cfg(not(target_arch = "wasm32"))]
        use tokio::time::timeout;
        timeout(
            Duration::from_secs(30),
            self.walk_membership(registry, root, wallet_id, max_hops),
        )
        .await
        .map_err(|_| {
            GroupError::Other("group trust inspection timed out; review required".into())
        })?
    }

    async fn walk_membership(
        &self,
        registry: &GroupRegistry,
        root: &str,
        wallet_id: &str,
        max_hops: u8,
    ) -> Result<Option<MembershipPath>, GroupError> {
        let mut queue = VecDeque::from([vec![root.to_owned()]]);
        let mut visited = BTreeSet::from([root.to_owned()]);
        let mut failure = None;
        while let Some(path) = queue.pop_front() {
            let id = path.last().expect("queued paths contain a root");
            let Some(group) = registry.get(id) else {
                retain_failure(&mut failure, GroupError::GroupNotFound(id.clone()));
                continue;
            };
            match self.verify_member(&group.endpoint_url, wallet_id).await {
                Ok(member) if member.valid => {
                    return Ok(Some(MembershipPath {
                        groups: path,
                        member,
                    }))
                }
                Ok(_) => {}
                Err(error) => {
                    retain_failure(&mut failure, error);
                    continue;
                }
            }
            if path.len() > usize::from(max_hops) {
                continue;
            }
            let edges = match self.endorsed_groups(&group.endpoint_url).await {
                Ok(edges) => edges,
                Err(error) => {
                    retain_failure(&mut failure, error);
                    continue;
                }
            };
            for next in edges {
                if visited.contains(&next) {
                    continue;
                }
                if visited.len() == MAX_VISITED_GROUPS {
                    retain_failure(
                        &mut failure,
                        GroupError::Other(
                            "group trust inspection limit exceeded; review required".into(),
                        ),
                    );
                    continue;
                }
                visited.insert(next.clone());
                let mut next_path = path.clone();
                next_path.push(next);
                queue.push_back(next_path);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

}

impl GroupClient {
    pub(crate) async fn endorsed_groups(&self, endpoint: &str) -> Result<Vec<String>, GroupError> {
        let mut url = crate::client::endpoint_url_checked(endpoint)?;
        url.path_segments_mut()
            .map_err(|()| GroupError::InvalidResponse("invalid group trust endpoint".into()))?
            .pop_if_empty()
            .push("trust");
        let bytes = self.get_bounded(url, 64 * 1024).await?;
        let edges: Endorsements = serde_json::from_slice(&bytes)
            .map_err(|error| GroupError::InvalidResponse(error.to_string()))?;
        let mut unique = BTreeSet::new();
        if edges.version != 1
            || edges.groups.len() > MAX_ENDORSEMENTS
            || edges.groups.iter().any(|id| {
                id.is_empty()
                    || id.trim() != id
                    || id.len() > 128
                    || id.chars().any(char::is_control)
                    || !unique.insert(id)
            })
        {
            return Err(GroupError::InvalidResponse(
                "invalid group trust list".into(),
            ));
        }
        Ok(edges.groups)
    }
}

fn retain_failure(previous: &mut Option<GroupError>, error: GroupError) {
    // A later outage cannot turn malformed or unconfigured evidence into an
    // outage-only result eligible for the wallet's protocol fallback.
    if previous.is_none() || matches!(previous, Some(GroupError::Unreachable(_))) {
        *previous = Some(error);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
