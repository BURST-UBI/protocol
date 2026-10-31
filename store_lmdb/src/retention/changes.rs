//! Buffer committed changes after a caller captures a clean database generation.
//! Exceeding the buffer budget invalidates the watch without rejecting ledger
//! writes. Observations are local before/after images; callers must authenticate
//! and reconcile them before using them to decide what history can be deleted.
use crate::{block_undo::{UndoChange, UndoTable}, LmdbEnvironment, LmdbReadTransaction, write_batch::CleanStorageCut};
use burst_store::StoreError;
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool, Ordering}};

#[derive(Default)]
pub(crate) struct Observers {
    active: AtomicBool,
    owner: Mutex<Weak<Mutex<State>>>,
}
struct State {
    generation: usize,
    valid: bool,
    rows: Vec<ObservedRow>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    visiting_rows: usize,
    visiting_bytes: usize,
}

struct ObservedRow {
    generation: usize,
    change: UndoChange,
}

/// Identity of the native commit containing an observed mutation. Matching a
/// cut establishes identity only, not authentication or deletion authority.
/// It borrows the environment only for the duration of observation visitation.
pub struct BodyChangeCommit<'a> {
    env: &'a LmdbEnvironment,
    generation: usize,
}
impl BodyChangeCommit<'_> {
    pub fn matches_cut(&self, cut: &CleanStorageCut) -> bool {
        cut.matches_generation(self.env, self.generation)
    }
}

/// Single owner; dropping it disables future capture. No reader is retained.
/// Callbacks may consume supplied bytes or write private scratch, but must not
/// access ledger storage, acquire its locks, or reenter this observer.
pub struct BodyChangeWatch {
    origin_generation: Option<usize>,
    observers: Arc<Observers>,
    state: Arc<Mutex<State>>,
}
impl Drop for BodyChangeWatch {
    fn drop(&mut self) {
        if let Ok(mut owner) = self.observers.owner.lock() {
            if owner.ptr_eq(&Arc::downgrade(&self.state)) {
                *owner = Weak::new();
                self.observers.active.store(false, Ordering::Release);
            }
        }
    }
}
fn invalid(message: &str) -> StoreError { StoreError::Corruption(message.into()) }
impl Observers {
    pub(crate) fn watch(self: &Arc<Self>, generation: usize, max_rows: usize, max_bytes: usize)
        -> Result<BodyChangeWatch, StoreError> {
        if max_rows == 0 || max_rows > 65_536 || max_bytes == 0 || max_bytes > 64 << 20 {
            return Err(invalid("invalid body change observation budget"));
        }
        let mut owner = self.owner.lock().map_err(|_| invalid("body change observer poisoned"))?;
        if owner.upgrade().is_some() { return Err(invalid("body change observer already owned")); }
        let state = Arc::new(Mutex::new(State { generation, valid: true, rows: Vec::new(),
            bytes: 0, max_rows, max_bytes, visiting_rows: 0, visiting_bytes: 0 }));
        *owner = Arc::downgrade(&state);
        self.active.store(true, Ordering::Release);
        Ok(BodyChangeWatch { origin_generation: Some(generation), observers: self.clone(), state })
    }
    pub(crate) fn begin(&self) -> Option<Pending> {
        if !self.active.load(Ordering::Acquire) { return None; }
        let state = self.owner.lock().ok()?.upgrade()?;
        let budget = state.lock().ok()?;
        if !budget.valid { return None; }
        Some(Pending { owner: Arc::downgrade(&state), rows: Vec::new(), bytes: 0,
            remaining_rows: budget.max_rows - budget.rows.len() - budget.visiting_rows,
            remaining_bytes: budget.max_bytes - budget.bytes - budget.visiting_bytes, valid: true })
    }
}

pub(crate) struct Pending {
    owner: Weak<Mutex<State>>,
    rows: Vec<ObservedRow>,
    bytes: usize,
    remaining_rows: usize,
    remaining_bytes: usize,
    valid: bool,
}
impl Pending {
    pub(crate) fn invalidate(&mut self) {
        self.valid = false;
        self.rows.clear();
        self.bytes = 0;
    }
    pub(crate) fn record(&mut self, table: UndoTable, key: &[u8], before: Option<&[u8]>, after: Option<&[u8]>) {
        if !self.valid || before == after { return; }
        let size = key.len().checked_add(before.map_or(0, <[u8]>::len))
            .and_then(|n| n.checked_add(after.map_or(0, <[u8]>::len)))
            .and_then(|n| n.checked_add(std::mem::size_of::<ObservedRow>()));
        let next = size.and_then(|n| self.bytes.checked_add(n));
        if self.rows.len() >= self.remaining_rows || next.is_none_or(|n| n > self.remaining_bytes) {
            self.invalidate();
            return;
        }
        self.bytes = next.unwrap();
        self.rows.push(ObservedRow { generation: 0, change: UndoChange { table, key: key.to_vec(), before: before.map(<[u8]>::to_vec), after: after.map(<[u8]>::to_vec) } });
    }
    /// Lock through native commit: a reader can never observe coverage published
    /// before the data, or mistake a later writer's generation for this commit.
    pub(crate) fn commit(self, before: usize, after: usize, commit: impl FnOnce() -> Result<(), StoreError>)
        -> Result<(), StoreError> {
        let state = self.owner.upgrade();
        let mut state = state.as_ref().and_then(|s| s.lock().ok());
        commit()?;
        if let Some(state) = state.as_mut() {
            if !self.valid || !state.valid || state.generation != before
                || (after != before && before.checked_add(1) != Some(after))
                || (after == before && !self.rows.is_empty()) {
                state.valid = false;
                state.rows.clear();
                state.bytes = 0;
            } else {
                state.generation = after;
                state.bytes += self.bytes;
                state.rows.extend(self.rows.into_iter().map(|mut row| { row.generation = after; row }));
            }
        }
        Ok(())
    }
}
// Detached callbacks must not hold the mutex acquired by ordinary writers.
// Their rows stay charged until dropped; concurrent commits use the remaining
// budget and retain their own generation for the next visit.
struct Visiting {
    state: Arc<Mutex<State>>,
    rows: Vec<ObservedRow>,
    finished: bool,
}
impl Visiting {
    fn visit(self, env: &LmdbEnvironment,
        mut visit: impl FnMut(&BodyChangeCommit<'_>, &UndoChange) -> Result<(), StoreError>)
        -> Result<(), StoreError> {
        for row in &self.rows {
            visit(&BodyChangeCommit { env, generation: row.generation }, &row.change)?;
        }
        self.finish()
    }
    fn finish(mut self) -> Result<(), StoreError> {
        drop(std::mem::take(&mut self.rows));
        let mut state = self.state.lock().map_err(|_| invalid("body change observer poisoned"))?;
        state.visiting_rows = 0;
        state.visiting_bytes = 0;
        if !state.valid { return Err(invalid("body change coverage is incomplete")); }
        self.finished = true;
        Ok(())
    }
}
impl Drop for Visiting {
    fn drop(&mut self) {
        if !self.finished {
            if let Ok(mut state) = self.state.lock() {
                state.valid = false;
                state.visiting_rows = 0;
                state.visiting_bytes = 0;
            }
        }
    }
}

impl BodyChangeWatch {
    /// Check that this watch began at `cut` and has never been drained or
    /// invalidated. This does not consume the watch or check the current database
    /// generation; the caller still owns reconciliation and publication checks.
    pub fn check_unconsumed_since(&self, env: &LmdbEnvironment, cut: &CleanStorageCut)
        -> Result<(), StoreError> {
        if !Arc::ptr_eq(&self.observers, &env.body_changes)
            || !self.origin_generation.is_some_and(|generation| cut.matches_generation(env, generation)) {
            return Err(invalid("body change watch does not cover the captured origin"));
        }
        let state = self.state.lock().map_err(|_| invalid("body change observer poisoned"))?;
        if !state.valid { return Err(invalid("body change coverage is incomplete")); }
        Ok(())
    }

    /// Check that the supplied cut matches this watch's last observed generation.
    /// This does not read the current database generation or consume observations.
    /// For a publication check, obtain the cut from the clean writer and keep that
    /// writer held through reconciliation and commit.
    pub fn check_covered_cut(&self, env: &LmdbEnvironment, cut: &CleanStorageCut)
        -> Result<(), StoreError> {
        if !Arc::ptr_eq(&self.observers, &env.body_changes) {
            return Err(invalid("body changes belong to another environment"));
        }
        let state = self.state.lock().map_err(|_| invalid("body change observer poisoned"))?;
        if !state.valid || !cut.matches_generation(env, state.generation) {
            return Err(invalid("body change coverage does not match the current cut"));
        }
        Ok(())
    }

    /// Visit buffered changes, including intermediate writes that cancel each
    /// other. Individual writes with identical before/after bytes are omitted.
    /// On successful visitation, return the exact observed cut and release the
    /// buffered bytes. Any gap or callback error permanently invalidates this watch.
    /// This never authenticates images or permits deletion without reconciliation.
    pub fn visit(&mut self, env: &LmdbEnvironment, mut visit: impl FnMut(&UndoChange) -> Result<(), StoreError>)
        -> Result<CleanStorageCut, StoreError> {
        self.visit_commits(env, |_, row| visit(row))
    }

    /// Visit mutations with their exact successful native commit identity.
    /// Intermediate writes share that identity; later same-key writes do not.
    /// Root-free commits consume no row slots. The same callback restrictions,
    /// coverage checks and poison-on-error behavior as `visit` apply.
    pub fn visit_commits(&mut self, env: &LmdbEnvironment,
        visit: impl FnMut(&BodyChangeCommit<'_>, &UndoChange) -> Result<(), StoreError>)
        -> Result<CleanStorageCut, StoreError> {
        let (view, visiting) = self.detach(env)?;
        let cut = view.clean_storage_cut().ok_or_else(|| invalid("body changes have no storage cut"))?;
        drop(view);
        visiting.visit(env, visit)?;
        Ok(cut)
    }

    /// Return the native snapshot matching exactly the detached observations.
    /// Later commits remain buffered while callbacks run without the observer
    /// lock. The caller owns the reader on this thread and must drop it before
    /// opening another reader. Publication still requires a current-writer check.
    pub fn visit_commits_with_snapshot<'a>(&mut self, env: &'a LmdbEnvironment,
        visit: impl FnMut(&BodyChangeCommit<'_>, &UndoChange) -> Result<(), StoreError>)
        -> Result<LmdbReadTransaction<'a>, StoreError> {
        let (view, visiting) = self.detach(env)?;
        visiting.visit(env, visit)?;
        Ok(view)
    }

    fn detach<'a>(&mut self, env: &'a LmdbEnvironment)
        -> Result<(LmdbReadTransaction<'a>, Visiting), StoreError> {
        self.origin_generation = None;
        if !Arc::ptr_eq(&self.observers, &env.body_changes) { return Err(invalid("body changes belong to another environment")); }
        let mut state = self.state.lock().map_err(|_| invalid("body change observer poisoned"))?;
        let view = env.tx_begin_read()?;
        if !state.valid || view.generation() != Some(state.generation) {
            state.valid = false;
            return Err(invalid("body change coverage is incomplete"));
        }
        let visiting = Visiting {
            state: self.state.clone(), rows: std::mem::take(&mut state.rows), finished: false,
        };
        state.visiting_rows = visiting.rows.len();
        state.visiting_bytes = std::mem::take(&mut state.bytes);
        drop(state);
        Ok((view, visiting))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;
    use burst_types::{BlockHash, WalletAddress};
    fn store(path: &std::path::Path) -> LmdbEnvironment {
        LmdbEnvironment::open(path, crate::REQUIRED_DATABASES, 32 << 20).unwrap()
    }
    fn watch(env: &LmdbEnvironment, rows: usize, bytes: usize) -> BodyChangeWatch {
        env.tx_begin_write().unwrap().watch_body_changes(rows, bytes).unwrap()
    }
    #[test]
    fn captured_watch_transfer_requires_exact_origin_and_undrained_coverage() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        let foreign_dir = tempfile::tempdir().unwrap(); let foreign = store(foreign_dir.path());
        let writer = env.tx_begin_write().unwrap();
        let mut changes = writer.watch_body_changes(8, 4096).unwrap();
        let reader = env.tx_begin_read().unwrap();
        let cut = reader.clean_storage_cut().unwrap();
        assert!(writer.matches_clean_storage_cut(&cut));
        drop(writer);
        changes.check_unconsumed_since(&env, &cut).unwrap();
        changes.check_covered_cut(&env, &cut).unwrap();
        assert!(changes.check_covered_cut(&foreign, &cut).is_err());
        assert!(changes.check_unconsumed_since(&foreign, &cut).is_err());
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("root", &[1]).unwrap();
        let later = writer.commit_with_storage_cut().unwrap();
        changes.check_covered_cut(&env, &later).unwrap();
        assert!(changes.check_covered_cut(&env, &cut).is_err());
        changes.check_unconsumed_since(&env, &cut).unwrap();
        assert!(changes.check_unconsumed_since(&env, &later).is_err());
        drop(reader);
        let mut rows = 0;
        changes.visit(&env, |_| { rows += 1; Ok(()) }).unwrap();
        assert_eq!(rows, 1);
        changes.check_covered_cut(&env, &later).unwrap();
        assert!(changes.check_unconsumed_since(&env, &cut).is_err());
        drop(changes);
        let changes = watch(&env, 1, 4096);
        let cut = env.tx_begin_read().unwrap().clean_storage_cut().unwrap();
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("root", &[2]).unwrap();
        writer.put_meta("root", &[3]).unwrap();
        writer.commit().unwrap();
        assert!(changes.check_unconsumed_since(&env, &cut).is_err());
        let current = env.tx_begin_read().unwrap().clean_storage_cut().unwrap();
        assert!(changes.check_covered_cut(&env, &current).is_err());
        drop(changes);
        watch(&env, 1, 4096);
    }

    #[test]
    fn detached_visit_allows_commits_and_keeps_later_images_for_the_next_cut() {
        use std::{sync::mpsc, time::Duration};
        for pinned in [false, true] {
            let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
            let mut observer = watch(&env, 8, 4096);
            let mut writer = env.tx_begin_write().unwrap();
            writer.put_meta("root", &[1]).unwrap();
            let first = writer.commit_with_storage_cut().unwrap();
            let (start, started) = mpsc::channel();
            let (complete, completed) = mpsc::channel();
            std::thread::scope(|scope| {
                let env = &env;
                scope.spawn(move || {
                    started.recv().unwrap();
                    let mut writer = env.tx_begin_write().unwrap();
                    writer.put_meta("root", &[2]).unwrap();
                    writer.put_meta("root", &[3]).unwrap();
                    let later = writer.commit_with_storage_cut().unwrap();
                    complete.send(later).unwrap();
                });
                let mut later = None;
                let visit = |identity: &BodyChangeCommit<'_>, row: &UndoChange| {
                    assert!(identity.matches_cut(&first));
                    assert_eq!(row.after.as_deref(), Some([1].as_slice()));
                    start.send(()).unwrap();
                    later = Some(completed.recv_timeout(Duration::from_secs(5))
                        .map_err(|_| invalid("writer blocked behind observation callback"))?);
                    Ok(())
                };
                let (cut, snapshot) = if pinned {
                    let snapshot = observer.visit_commits_with_snapshot(env, visit).unwrap();
                    (snapshot.clean_storage_cut().unwrap(), Some(snapshot))
                } else { (observer.visit_commits(env, visit).unwrap(), None) };
                if let Some(snapshot) = &snapshot {
                    assert_eq!(env.meta_store().get_meta_txn(snapshot, "root").unwrap(), Some(vec![1]));
                }
                drop(snapshot);
                assert!(!env.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
                let mut images = Vec::new();
                let current = observer.visit_commits(&env, |identity, row| {
                    assert!(identity.matches_cut(later.as_ref().unwrap()));
                    assert!(!identity.matches_cut(&first));
                    images.push((row.before.clone(), row.after.clone()));
                    Ok(())
                }).unwrap();
                assert_eq!(images, vec![(Some(vec![1]), Some(vec![2])), (Some(vec![2]), Some(vec![3]))]);
                assert!(env.tx_begin_write().unwrap().matches_clean_storage_cut(&current));
                observer.visit(&env, |_| panic!("detached rows repeated")).unwrap();
            });
        }
    }

    #[test]
    fn detached_rows_keep_both_budgets_and_overflow_poisoning_depends_on_commit() {
        use std::{sync::mpsc, time::Duration};
        for pinned in [false, true] {
            let extent = std::mem::size_of::<ObservedRow>() + 2;
            for (rows, bytes) in [(1, 4096), (2, extent * 2 - 1)] {
                for commit in [false, true] {
                    let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
                    let mut observer = watch(&env, rows, bytes);
                    let mut writer = env.tx_begin_write().unwrap();
                    writer.put_meta("a", &[1]).unwrap(); writer.commit().unwrap();
                    let (start, started) = mpsc::channel();
                    let (complete, completed) = mpsc::channel();
                    std::thread::scope(|scope| {
                        let env = &env;
                        scope.spawn(move || {
                            started.recv().unwrap();
                            let mut writer = env.tx_begin_write().unwrap();
                            writer.put_meta("b", &[2]).unwrap();
                            if commit { writer.commit().unwrap(); } else { drop(writer); }
                            complete.send(()).unwrap();
                        });
                        let visit = |_: &BodyChangeCommit<'_>, _: &UndoChange| {
                            start.send(()).unwrap();
                            completed.recv_timeout(Duration::from_secs(5))
                                .map_err(|_| invalid("writer blocked behind detached budget"))?;
                            Ok(())
                        };
                        let result = if pinned {
                            observer.visit_commits_with_snapshot(env, visit).map(drop)
                        } else { observer.visit_commits(env, visit).map(drop) };
                        assert_eq!(result.is_err(), commit);
                    });
                    assert_eq!(env.meta_store().get_meta("b").is_ok(), commit);
                    assert_eq!(observer.visit(&env, |_| panic!("overflow or abort published rows")).is_err(), commit);
                    let state = observer.state.lock().unwrap();
                    assert_eq!((state.visiting_rows, state.visiting_bytes), (0, 0));
                }
            }
        }
    }

    #[test]
    fn detached_callback_panic_permanently_invalidates_coverage() {
        for pinned in [false, true] {
            let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
            let mut observer = watch(&env, 8, 4096);
            let mut writer = env.tx_begin_write().unwrap();
            writer.put_meta("root", &[1]).unwrap(); writer.commit().unwrap();
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if pinned {
                    observer.visit_commits_with_snapshot(&env, |_, _| panic!("consumer panic")).unwrap();
                } else { observer.visit(&env, |_| panic!("consumer panic")).unwrap(); }
            }));
            assert!(panic.is_err());
            assert!(observer.visit(&env, |_| Ok(())).is_err());
            let mut writer = env.tx_begin_write().unwrap();
            writer.put_meta("root", &[2]).unwrap(); writer.commit().unwrap();
            assert_eq!(env.meta_store().get_meta("root").unwrap(), vec![2]);
        }
    }

    #[test]
    fn commit_identity_distinguishes_own_delete_from_later_same_key_writes() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        let other_dir = tempfile::tempdir().unwrap(); let other = store(other_dir.path());
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("body", &[1]).unwrap(); writer.commit().unwrap();
        let mut observer = watch(&env, 8, 4096);
        let original = env.tx_begin_read().unwrap().clean_storage_cut().unwrap();
        let foreign = other.tx_begin_read().unwrap().clean_storage_cut().unwrap();
        let mut writer = env.tx_begin_write().unwrap();
        writer.delete_meta("body").unwrap();
        let own = writer.commit_with_storage_cut().unwrap();
        let mut aborted = env.tx_begin_write().unwrap();
        aborted.put_meta("body", &[9]).unwrap(); drop(aborted);
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("body", &[1]).unwrap();
        writer.put_meta("body", &[2]).unwrap();
        writer.delete_meta("body").unwrap();
        let later = writer.commit_with_storage_cut().unwrap();
        let mut observed = Vec::new();
        let cut = observer.visit_commits(&env, |commit, row| {
            assert!(!commit.matches_cut(&original));
            assert!(!commit.matches_cut(&foreign));
            observed.push((commit.matches_cut(&own), commit.matches_cut(&later), row.clone()));
            Ok(())
        }).unwrap();
        assert_eq!(observed.len(), 4);
        assert!(observed[0].0 && !observed[0].1);
        assert_eq!(observed[0].2.before, Some(vec![1]));
        assert_eq!(observed[0].2.after, None);
        for row in &observed[1..] { assert!(!row.0 && row.1); }
        assert_eq!(observed[1].2.after, Some(vec![1]));
        assert_eq!(observed[2].2.after, Some(vec![2]));
        assert_eq!(observed[3].2.after, None);
        assert!(env.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
        observer.visit_commits(&env, |_, _| panic!("rows were not consumed")).unwrap();
    }

    #[test]
    fn commit_identity_overhead_is_budgeted_before_copying() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        let exact = std::mem::size_of::<ObservedRow>() + 1 + 1;
        for (budget, succeeds) in [(exact - 1, false), (exact, true)] {
            let mut observer = watch(&env, 1, budget);
            let mut writer = env.tx_begin_write().unwrap();
            writer.put_meta("x", &[1]).unwrap();
            let committed = writer.commit_with_storage_cut().unwrap();
            let mut count = 0;
            let result = observer.visit_commits(&env, |identity, _| {
                assert!(identity.matches_cut(&committed)); count += 1; Ok(())
            });
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(count, usize::from(succeeds));
            drop(observer);
            let mut writer = env.tx_begin_write().unwrap();
            writer.delete_meta("x").unwrap(); writer.commit().unwrap();
        }
    }

    #[test]
    fn observations_preserve_intermediate_images_and_abort_without_publication() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        let mut watch = watch(&env, 16, 4096);
        let mut aborted = env.tx_begin_write().unwrap();
        aborted.put_meta("root", &[9]).unwrap(); drop(aborted);
        env.tx_begin_write().unwrap().commit().unwrap();
        watch.visit(&env, |_| panic!("aborted/empty transaction published a row")).unwrap();
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("root", &[1]).unwrap();
        writer.put_meta("root", &[2]).unwrap();
        writer.delete_meta("root").unwrap();
        writer.commit().unwrap();
        let mut observed = Vec::new();
        let cut = watch.visit(&env, |row| { observed.push(row.clone()); Ok(()) }).unwrap();
        assert_eq!(observed.len(), 3);
        assert_eq!(observed[0].before, None);
        assert_eq!(observed[0].after, Some(vec![1]));
        assert_eq!(observed[1].before, Some(vec![1]));
        assert_eq!(observed[1].after, Some(vec![2]));
        assert_eq!(observed[2].before, Some(vec![2]));
        assert_eq!(observed[2].after, None);
        assert!(env.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
        watch.visit(&env, |_| panic!("successful visit did not consume rows")).unwrap();
    }
    #[test]
    fn overflow_invalidates_only_after_commit_and_never_rejects_ledger_writes() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        for commit in [false, true] {
            let mut watch = watch(&env, 1, 128);
            let mut writer = env.tx_begin_write().unwrap();
            writer.put_meta("too_large", &vec![3; 1024]).unwrap();
            if commit { writer.commit().unwrap(); } else { drop(writer); }
            assert_eq!(watch.visit(&env, |_| Ok(())).is_err(), commit);
        }
        assert_eq!(env.meta_store().get_meta("too_large").unwrap(), vec![3; 1024]);
        let mut watch = watch(&env, 1, 4096);
        for key in ["one", "two"] {
            let mut writer = env.tx_begin_write().unwrap(); writer.put_meta(key, &[1]).unwrap(); writer.commit().unwrap();
        }
        assert!(watch.visit(&env, |_| Ok(())).is_err());
    }
    #[test]
    fn gaps_foreign_views_callback_errors_and_owner_drop_fail_closed() {
        for pinned in [false, true] {
            let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
            let other_dir = tempfile::tempdir().unwrap(); let other = store(other_dir.path());
            let mut observer = watch(&env, 16, 4096);
            assert!(env.tx_begin_write().unwrap().watch_body_changes(16, 4096).is_err());
            assert!(if pinned { observer.visit_commits_with_snapshot(&other, |_, _| Ok(())).map(drop) } else { observer.visit(&other, |_| Ok(())).map(drop) }.is_err());
            env.meta_store().put_meta("untracked", &[1]).unwrap();
            assert!(observer.visit(&env, |_| Ok(())).is_err());
            drop(observer);
            let mut observer = watch(&env, 16, 4096);
            let mut writer = env.tx_begin_write().unwrap(); writer.put_meta("tracked", &[2]).unwrap(); writer.commit().unwrap();
            assert!(if pinned { observer.visit_commits_with_snapshot(&env, |_, _| Err(invalid("consumer failed"))).map(drop) } else { observer.visit(&env, |_| Err(invalid("consumer failed"))).map(drop) }.is_err());
            assert!(observer.visit(&env, |_| Ok(())).is_err());
            drop(observer);
            let old = watch(&env, 16, 4096);
            let mut writer = env.tx_begin_write().unwrap(); writer.put_meta("discarded", &[3]).unwrap();
            drop(old); writer.commit().unwrap();
            let mut fresh = watch(&env, 16, 4096);
            fresh.visit(&env, |_| panic!("old owner's rows leaked")).unwrap();
        }
    }

    #[test]
    fn soft_reservations_skip_rows_final_votes_are_observed_and_reopen_rejects() {
        let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
        let epoch = BlockHash::new([9;32]);
        let voter = WalletAddress::new("brst_observer_voter");
        let account = WalletAddress::new("brst_observer_account");
        let hash = BlockHash::new([8;32]);
        env.meta_store().put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let mut observer = watch(&env, 16, 4096);
        let origin = env.tx_begin_write().unwrap().clean_storage_cut().unwrap();
        for _ in 0..4 {
            env.meta_store().reserve_consensus_vote(&epoch, &voter, &account, &BlockHash::ZERO, &hash, false, 0).unwrap();
        }
        let before = observer.visit(&env, |_| panic!("soft reservation introduced a body mutation")).unwrap();
        assert!(origin.body_dependencies_unchanged(&before, &env).unwrap());
        assert!(!origin.same_committed_view(&before));
        env.meta_store().reserve_consensus_vote_with_governance_limit(&epoch, &voter, &account,
            &BlockHash::ZERO, &hash, true, 0, Some(100), Some(100)).unwrap();
        let mut rows = Vec::new();
        let after = observer.visit(&env, |row| { rows.push(row.clone()); Ok(()) }).unwrap();
        assert!(!before.body_dependencies_unchanged(&after, &env).unwrap());
        assert_eq!(rows.len(), 3);
        assert!(rows[0].key.starts_with(b"consensus_final/"));
        assert_eq!(rows[0].after.as_deref(), Some(hash.as_bytes().as_slice()));
        for (row, prefix) in rows[1..].iter().zip([b"consensus_time/".as_slice(), b"consensus_governance/"]) {
            assert!(row.key.starts_with(prefix));
            assert_eq!(row.after.as_deref(), Some(100u64.to_be_bytes().as_slice()));
        }
        drop(env);
        let reopened = store(dir.path());
        assert!(!before.same_committed_view(&reopened.tx_begin_write().unwrap().clean_storage_cut().unwrap()));
        assert!(observer.visit(&reopened, |_| Ok(())).is_err());
    }

    #[test]
    fn journal_sealing_and_elected_handoff_are_observed_atomically() {
        for mode in 0..5 {
            let dir = tempfile::tempdir().unwrap(); let env = store(dir.path());
            let mut observer = watch(&env, if mode == 4 { 1 } else { 16 }, 8192);
            let hash = BlockHash::new([1; 32]);
            let account = WalletAddress::new("brst_watch");
            let mut writer = env.tx_begin_write().unwrap();
            writer.begin_block_undo(hash, account.clone(), 1).unwrap();
            if mode != 1 { writer.put_meta("recorded", &[1]).unwrap(); }
            if mode == 2 { writer.put_block(&hash, b"body").unwrap(); }
            if mode == 2 || mode == 3 {
                assert_eq!(writer.commit_elected_block(&hash, &account, 1).is_ok(), mode == 2);
            } else { assert_eq!(writer.commit().is_ok(), mode == 0 || mode == 4); }
            if mode == 4 {
                assert!(observer.visit(&env, |_| Ok(())).is_err());
                assert!(env.tx_begin_write().unwrap().block_undo(&hash).unwrap().is_some());
                continue;
            }
            let mut observed = Vec::new();
            let cut = observer.visit(&env, |row| { observed.push(row.clone()); Ok(()) }).unwrap();
            assert!(env.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
            if mode == 1 || mode == 3 { assert!(observed.is_empty()); continue; }
            let journals: Vec<_> = observed.iter().filter(|row| row.key.starts_with(crate::block_undo::PREFIX)).collect();
            assert_eq!(journals.len(), 1);
            let journal = journals[0];
            assert_eq!(journal.key, crate::block_undo::key(&hash));
            assert!(journal.before.is_none());
            let decoded = crate::block_undo::BlockUndo::decode(&hash, journal.after.as_deref().unwrap()).unwrap();
            assert_eq!(decoded.changes.len(), if mode == 2 { 2 } else { 1 });
            for change in &decoded.changes { assert!(observed.contains(change)); }
            let queued = observed.iter().filter(|row| row.key.starts_with(b"pending_confirmation/")).count();
            assert_eq!(queued, usize::from(mode == 2));
        }
    }
}
