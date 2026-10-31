use crate::block_processor::BlockAuthentication;
use crate::economic_candidate::prepared::{CapturedPayment, PreparedStageError, StagedPayment};
use burst_ledger::{BlockType, StateBlock};
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::{ProtocolParams, WalletAddress};
use std::collections::HashSet;

const MAX_BLOCKS: usize = 32;
const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024;

pub(super) fn eligible(candidate: &BlockAuthentication) -> bool {
    candidate.preparation_fault.is_none()
        && candidate.result().is_ok()
        && matches!(candidate.block().block_type, BlockType::Burn | BlockType::Send
            | BlockType::Receive | BlockType::RejectReceive | BlockType::Merge
            | BlockType::ChangeRepresentative | BlockType::ChangeSigner)
}

pub(super) struct Budget {
    accounts: HashSet<WalletAddress>,
    bytes: u64,
}

impl Budget {
    pub(super) fn new(first: &StateBlock) -> Self {
        Self { accounts: HashSet::from([first.account.clone()]),
            bytes: bincode::serialized_size(first).unwrap_or(MAX_INPUT_BYTES) }
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.accounts.len() < MAX_BLOCKS && self.bytes < MAX_INPUT_BYTES
    }

    pub(super) fn include(&mut self, block: &StateBlock) -> bool {
        let Ok(size) = bincode::serialized_size(block) else { return false; };
        let Some(bytes) = self.bytes.checked_add(size).filter(|&n| n <= MAX_INPUT_BYTES)
            else { return false; };
        if !self.has_capacity() || !self.accounts.insert(block.account.clone()) { return false; }
        self.bytes = bytes;
        true
    }
}

/// The caller has authenticated and admitted distinct accounts against the live
/// frontier. Compute or recheck economic reads against the writer, including
/// effects of earlier group members. The caller must abort the entire transaction
/// on failure.
pub(super) fn stage(
    batch: &mut LmdbWriteTransaction<'_>, candidate: &mut BlockAuthentication,
    params: &ProtocolParams,
) -> Result<StagedPayment, String> {
    let prepared = match candidate.prepared.take() {
        Some(prepared) => prepared,
        None => CapturedPayment::capture(batch, candidate.block(), params)
            .map_err(|error| match error {
                PreparedStageError::BeforeWrite(error) | PreparedStageError::Abort(error) => error,
            })?
            .ok_or("grouped payment needs serial storage fallback")?
            .prepare()?,
    };
    let staged = match prepared.stage_revalidated(batch, params) {
        Ok(Some(staged)) => staged,
        Ok(None) => return Err("grouped preparation needs serial fallback".into()),
        Err(PreparedStageError::BeforeWrite(error) | PreparedStageError::Abort(error)) => return Err(error),
    };
    crate::bootstrap_phase::observe(batch, params)?;
    Ok(staged)
}

pub(super) fn retry_in_order<T>(first: T, grouped: Vec<T>, gap_positions: Vec<usize>,
    serial: &mut std::collections::VecDeque<T>) {
    let mut retry = Vec::new();
    let mut gaps = gap_positions.into_iter().peekable();
    for (index, candidate) in grouped.into_iter().enumerate() {
        while gaps.peek() == Some(&index) {
            retry.push(serial.pop_front().expect("deferred group gap"));
            gaps.next();
        }
        retry.push(candidate);
    }
    for candidate in retry.into_iter().rev() { serial.push_front(candidate); }
    serial.push_front(first);
}

/// Caller has already aborted the writer and dropped live publication guards.
pub(super) async fn wait_growth_release(watch: burst_store_lmdb::GrowthWatch,
    shutdown: &crate::shutdown::ShutdownController) -> bool {
    let mut stop = shutdown.subscribe();
    loop {
        if shutdown.is_requested() { return false; }
        if watch.wait_released(std::time::Duration::ZERO) { return true; }
        tokio::select! {
            biased;
            _ = stop.recv() => return false,
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn growth_release_wait_preserves_owner_and_honors_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        let limit = writer.limit_file_extent(4 << 20).unwrap();
        let watch = limit.watch();
        drop(writer);
        let shutdown = crate::shutdown::ShutdownController::new();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20),
            wait_growth_release(watch.clone(), &shutdown)).await.is_err());
        assert!(!watch.wait_released(std::time::Duration::ZERO));
        shutdown.shutdown();
        assert!(!wait_growth_release(watch.clone(), &shutdown).await);
        assert!(!watch.wait_released(std::time::Duration::ZERO));
        drop(limit);
        assert!(wait_growth_release(watch, &crate::shutdown::ShutdownController::new()).await);
    }

    #[test]
    fn aborted_group_restores_encounter_order_with_interleaved_and_trailing_gaps() {
        for (grouped, positions, pending) in [
            (vec![2, 4], vec![0, 1, 2], vec![1, 3, 5, 6]),
            (vec![1, 2, 3], vec![], vec![4, 5, 6]),
            (vec![3, 4], vec![0, 0, 2], vec![1, 2, 5, 6]),
        ] {
            let mut serial = pending.into_iter().collect();
            retry_in_order(0, grouped, positions, &mut serial);
            assert_eq!(serial.into_iter().collect::<Vec<_>>(), (0..=6).collect::<Vec<_>>());
        }
    }

    #[test]
    fn unprepared_payment_is_eligible_but_faults_and_invalid_signatures_are_not() {
        let keys = burst_crypto::keypair_from_seed(&[17; 32]);
        let mut block = crate::genesis_state::block(
            burst_types::NetworkId::Dev, &ProtocolParams::default());
        block.account = burst_crypto::derive_address(&keys.public);
        block.block_type = BlockType::ChangeRepresentative;
        block.previous = burst_types::BlockHash::new([9; 32]);
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
        let processor = crate::block_processor::BlockProcessor::new(0, block.payment_epoch);
        let mut candidate = processor.authentication_rules().authenticate(block.clone());
        assert!(candidate.result().is_ok());
        assert!(candidate.prepared.is_none());
        assert!(eligible(&candidate));
        candidate.preparation_fault = Some("authoritative snapshot fault".into());
        assert!(!eligible(&candidate));
        block.signature = burst_types::Signature([0; 64]);
        assert!(!eligible(&processor.authentication_rules().authenticate(block.clone())));
        block.block_type = BlockType::GovernanceEndorse;
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
        let lifecycle = processor.authentication_rules().authenticate(block);
        assert!(lifecycle.result().is_ok());
        assert!(!eligible(&lifecycle));
    }

    #[test]
    fn group_budget_keeps_account_conflicts_and_oversized_input_on_serial_path() {
        let mut block = crate::genesis_state::block(burst_types::NetworkId::Dev, &ProtocolParams::default());
        let mut budget = Budget::new(&block);
        assert!(!budget.include(&block));
        for id in 1..MAX_BLOCKS {
            block.account = WalletAddress::new(format!("brst_group_{id}"));
            assert!(budget.include(&block));
        }
        assert!(!budget.has_capacity());
        block.account = WalletAddress::new("brst_overflow");
        assert!(!budget.include(&block));
        let mut budget = Budget::new(&block);
        budget.bytes = MAX_INPUT_BYTES - 1;
        block.account = WalletAddress::new("brst_bytes");
        assert!(!budget.include(&block));
        assert_eq!(budget.accounts.len(), 1);
    }
}


#[cfg(test)]
struct ProbeState {
    first: Option<burst_types::BlockHash>,
    pressure: bool,
    commit_pressure: bool,
    ready: Option<Vec<tokio::task::JoinHandle<BlockAuthentication>>>,
    staged: Option<tokio::sync::oneshot::Sender<()>>,
    resume_staged: std::sync::mpsc::Receiver<bool>,
    aborted: Option<tokio::sync::oneshot::Sender<()>>,
    resume_aborted: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
pub(super) struct AbortProbe(std::sync::Mutex<ProbeState>);

#[cfg(test)]
pub(super) struct ProbeController {
    pub staged: tokio::sync::oneshot::Receiver<()>,
    pub resume_staged: std::sync::mpsc::Sender<bool>,
    pub aborted: tokio::sync::oneshot::Receiver<()>,
    pub resume_aborted: std::sync::mpsc::Sender<()>,
}

#[cfg(test)]
impl AbortProbe {
    pub(super) fn new() -> (std::sync::Arc<Self>, ProbeController) {
        let (staged, staged_rx) = tokio::sync::oneshot::channel();
        let (resume_staged, resume_staged_rx) = std::sync::mpsc::channel();
        let (aborted, aborted_rx) = tokio::sync::oneshot::channel();
        let (resume_aborted, resume_aborted_rx) = std::sync::mpsc::channel();
        (std::sync::Arc::new(Self(std::sync::Mutex::new(ProbeState {
            first: None, pressure: false, commit_pressure: false, ready: None, staged: Some(staged), resume_staged: resume_staged_rx,
            aborted: Some(aborted), resume_aborted: resume_aborted_rx,
        }))), ProbeController { staged: staged_rx, resume_staged, aborted: aborted_rx, resume_aborted })
    }

    pub(super) fn pressure(&self, at_commit: bool) {
        let mut state = self.0.lock().unwrap();
        state.pressure = true;
        state.commit_pressure = at_commit;
    }

    pub(super) fn arm(&self, first: burst_types::BlockHash, ready: Vec<tokio::task::JoinHandle<BlockAuthentication>>) {
        let mut state = self.0.lock().unwrap();
        assert!(state.first.is_none());
        assert!(ready.iter().all(tokio::task::JoinHandle::is_finished));
        state.first = Some(first);
        state.ready = Some(ready);
    }

    pub(super) fn install_ready(&self, first: burst_types::BlockHash, queue: &mut crate::block_validation::BlockValidationQueue) {
        let mut state = self.0.lock().unwrap();
        if state.first == Some(first) {
            if let Some(ready) = state.ready.take() { queue.install_completed_heads(ready); }
        }
    }

    pub(super) fn staged(&self, first: burst_types::BlockHash, batch: &mut LmdbWriteTransaction<'_>, others: &[BlockAuthentication]) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        if state.first != Some(first) { return Ok(()); }
        assert_eq!(others.len(), 1);
        assert!(batch.get_block(&first).is_ok());
        assert!(batch.get_block(&others[0].block().hash).is_ok());
        state.staged.take().expect("one grouped staging attempt").send(()).unwrap();
        let abort = state.resume_staged.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        if state.commit_pressure {
            // The value fits now; its undo journal crosses the ceiling in commit.
            batch.put_meta("test/admission-pressure", &vec![0; 3 << 20]).unwrap();
        }
        if abort {
            if state.pressure {
                batch.put_meta("test/admission-pressure", &vec![0; 8 << 20])
                    .map_err(|error| error.to_string())?;
                panic!("pressure injection did not reach the native ceiling");
            }
            Err("injected grouped staging failure".into())
        } else {
            Ok(())
        }
    }

    pub(super) fn aborted(&self, first: burst_types::BlockHash, commit_attempted: bool) {
        let mut state = self.0.lock().unwrap();
        if state.first != Some(first) { return; }
        if state.pressure { assert_eq!(commit_attempted, state.commit_pressure); }
        state.aborted.take().expect("one group abort").send(()).unwrap();
        state.resume_aborted.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    }
}
