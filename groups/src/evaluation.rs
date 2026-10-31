//! Request sharing and one concurrency budget for a single payment evaluation.

use crate::{GroupClient, GroupError, MemberStatus};
use std::{collections::HashMap, sync::{Arc, Mutex}};
use tokio::sync::{OnceCell, Semaphore};

type Cached<T> = Arc<OnceCell<Result<T, GroupError>>>;

pub const MAX_CONCURRENT_REQUESTS: usize = 8;

pub struct GroupEvaluation<'a> {
    client: &'a GroupClient,
    requests: Semaphore,
    members: Mutex<HashMap<(String, String), Cached<MemberStatus>>>,
    endorsements: Mutex<HashMap<String, Cached<Vec<String>>>>,
}

impl GroupClient {
    /// Drop this evaluation after the payment; results must not cross payments.
    pub fn evaluation(&self) -> GroupEvaluation<'_> {
        GroupEvaluation {
            client: self,
            requests: Semaphore::new(MAX_CONCURRENT_REQUESTS),
            members: Mutex::new(HashMap::new()),
            endorsements: Mutex::new(HashMap::new()),
        }
    }
}

impl GroupEvaluation<'_> {
    pub async fn verify_member(&self, endpoint: &str, wallet: &str) -> Result<MemberStatus, GroupError> {
        let cell = self.members.lock().map_err(|_| GroupError::Other("membership cache unavailable".into()))?
            .entry((endpoint.to_owned(), wallet.to_owned())).or_default().clone();
        cell.get_or_init(|| async {
            let _permit = self.requests.acquire().await
                .map_err(|_| GroupError::Other("group evaluation closed".into()))?;
            self.client.verify_member(endpoint, wallet).await
        }).await.clone()
    }

    pub(crate) async fn endorsed_groups(&self, endpoint: &str) -> Result<Vec<String>, GroupError> {
        let cell = self.endorsements.lock().map_err(|_| GroupError::Other("endorsement cache unavailable".into()))?
            .entry(endpoint.to_owned()).or_default().clone();
        cell.get_or_init(|| async {
            let _permit = self.requests.acquire().await
                .map_err(|_| GroupError::Other("group evaluation closed".into()))?;
            self.client.endorsed_groups(endpoint).await
        }).await.clone()
    }
}
