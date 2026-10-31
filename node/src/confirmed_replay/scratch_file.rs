//! Process-local accounting for private replay files, grouped by canonical directory.
//! A lease includes existing ScratchFile reservations and reserves space for the
//! persistent recovery-input store. It does not inventory arbitrary directory files.
use std::{
    collections::HashMap,
    fs::{File, Metadata},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

type Registry = Mutex<HashMap<PathBuf, Weak<Account>>>;
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn lock<T>(mutex: &Mutex<T>) -> io::Result<std::sync::MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| io::Error::other("replay scratch accounting poisoned"))
}
#[derive(Default)]
struct Usage {
    bytes: u64, limit: Option<u64>, policy: Weak<Policy>,
    #[cfg(test)]
    files: HashMap<u64, (u64, String)>,
}
struct Account { directory: PathBuf, usage: Mutex<Usage> }
impl Drop for Account {
    fn drop(&mut self) {
        // A new owner may have replaced an expired weak entry before this drop.
        if let Ok(mut entries) = registry().lock() {
            if entries.get(&self.directory).is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self)) {
                entries.remove(&self.directory);
            }
        }
    }
}
fn account(directory: &Path) -> io::Result<Arc<Account>> {
    let directory = directory.canonicalize()?;
    let mut entries = lock(registry())?;
    if let Some(account) = entries.get(&directory).and_then(Weak::upgrade) { return Ok(account); }
    let account = Arc::new(Account { directory: directory.clone(), usage: Mutex::default() });
    entries.insert(directory, Arc::downgrade(&account));
    Ok(account)
}

pub(crate) const PERSISTENT_INPUTS_DIRECTORY: &str = "recovery-inputs-v2";

// Count retained input files even when their owner has closed or the process
// restarted. Never follow a cache-directory/file symlink or ignore an unknown
// file that would otherwise escape the aggregate budget.
pub(super) fn persistent_bytes(directory: &Path) -> io::Result<u64> {
    let path = directory.join(PERSISTENT_INPUTS_DIRECTORY);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("invalid persistent recovery input directory"));
    }
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() != "data.mdb" && entry.file_name() != "lock.mdb" {
            return Err(io::Error::other("unknown persistent recovery input file"));
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::other("invalid persistent recovery input file"));
        }
        bytes = bytes.checked_add(metadata.len()).ok_or_else(|| io::Error::other("persistent input extent overflow"))?;
    }
    Ok(bytes)
}

/// One active policy per canonical directory in this process. Files created or grown under this policy
/// retain it, including files held by recovery caches and successor owners.
#[derive(Clone)]
pub(crate) struct ScratchLimit(Arc<Policy>);
struct Policy { account: Arc<Account>, persistent: u64, total: u64 }
impl ScratchLimit {
    #[cfg(test)]
    pub(crate) fn acquire(directory: &Path, bytes: u64) -> io::Result<Self> {
        Self::acquire_reserved(directory, bytes, 0)
    }

    /// Reserve growth of the dedicated persistent input store while its owner
    /// is active. Ordinary leases still charge its actual retained file extents.
    #[cfg(test)]
    pub(crate) fn acquire_reserved(directory: &Path, bytes: u64, persistent: u64) -> io::Result<Self> {
        Self::acquire_policy(directory, bytes, persistent, false)
    }

    /// Reuse the same aggregate policy when a previous recovery left owned
    /// scratch behind. Sharing never adds capacity or changes its reservation.
    pub(crate) fn acquire_shared_reserved(directory: &Path, bytes: u64, persistent: u64) -> io::Result<Self> {
        Self::acquire_policy(directory, bytes, persistent, true)
    }

    fn acquire_policy(directory: &Path, bytes: u64, persistent: u64, share: bool) -> io::Result<Self> {
        let account = account(directory)?;
        let mut usage = lock(&account.usage)?;
        let retained = persistent_bytes(&account.directory)?.max(persistent);
        if usage.limit.is_some() {
            if share {
                if let Some(policy) = usage.policy.upgrade() {
                    if policy.total == bytes && retained <= policy.persistent {
                        return Ok(Self(policy));
                    }
                    // The upgrade may be the last owner if another worker just
                    // dropped its lease. Policy::drop locks this same mutex.
                    drop(usage);
                    return Err(io::Error::other("replay scratch limit already owned or incompatible"));
                }
            }
            return Err(io::Error::other("replay scratch limit already owned or incompatible"));
        }
        let available = bytes.checked_sub(retained).filter(|n| *n != 0)
            .ok_or_else(|| io::Error::other("persistent inputs exhaust replay scratch budget"))?;
        if usage.bytes > available {
            return Err(io::Error::other("replay scratch limit unavailable or already exceeded"));
        }
        let policy = Arc::new(Policy { account: account.clone(), persistent: retained, total: bytes });
        usage.limit = Some(available);
        usage.policy = Arc::downgrade(&policy);
        Ok(Self(policy))
    }

    /// Retain an already active directory policy; this never creates a budget.
    pub(crate) fn current(directory: &Path) -> io::Result<Option<Self>> {
        let account = account(directory)?;
        let usage = lock(&account.usage)?;
        let policy = usage.policy.upgrade();
        if usage.limit.is_some() && policy.is_none() {
            return Err(io::Error::other("replay scratch policy is retiring"));
        }
        Ok(policy.map(Self))
    }

    pub(crate) fn temporary_capacity(&self) -> u64 { self.0.total - self.0.persistent }

    #[cfg(test)]
    pub(crate) fn used_temporary_bytes(&self) -> io::Result<u64> {
        Ok(lock(&self.0.account.usage)?.bytes)
    }


    #[cfg(test)]
    pub(crate) fn diagnostic_files(&self) -> io::Result<Vec<(u64, String)>> {
        let mut files: Vec<_> = lock(&self.0.account.usage)?.files.values().cloned().collect();
        files.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
        Ok(files)
    }

    pub(crate) fn check_persistent(&self, directory: &Path, required: u64) -> io::Result<()> {
        if directory.canonicalize()? != self.0.account.directory || required > self.0.persistent {
            return Err(io::Error::other("persistent input reservation unavailable for directory"));
        }
        Ok(())
    }
}
impl Drop for Policy {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.account.usage.lock() { usage.limit = None; }
    }
}

/// Production callers cannot obtain a raw writable handle or clone. Charges are
/// reserved file extents (including seek gaps), not filesystem blocks or memory buffers.
/// A failed or partial write conservatively retains its reservation until drop.
pub(crate) struct ScratchFile {
    file: File,
    account: Arc<Account>,
    reserved: u64,
    position: u64,
    policy: Option<Arc<Policy>>,
    #[cfg(test)]
    diagnostic_id: Option<u64>,
}
impl ScratchFile {
    pub(crate) fn new_in(directory: &Path) -> io::Result<Self> {
        let account = account(directory)?;
        let file = tempfile::tempfile_in(&account.directory)?;
        let policy = lock(&account.usage)?.policy.upgrade();
        #[cfg(test)]
        let diagnostic_id = if std::env::var_os("BURST_SCRATCH_ALLOCATION_DIAGNOSTIC").is_some() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            lock(&account.usage)?.files.insert(id, (0, trace));
            Some(id)
        } else { None };
        Ok(Self { file, account, reserved: 0, position: 0, policy,
            #[cfg(test)]
            diagnostic_id,
        })
    }
    pub(crate) fn new() -> io::Result<Self> { Self::new_in(&std::env::temp_dir()) }
    pub(crate) fn metadata(&self) -> io::Result<Metadata> { self.file.metadata() }

    /// Absolute I/O avoids a separate seek syscall on Unix. Callers must not
    /// depend on the sequential cursor afterward; other targets use seek I/O.
    pub(crate) fn read_exact_at(&mut self, bytes: &mut [u8], offset: u64) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_exact_at(bytes, offset)
        }
        #[cfg(not(unix))]
        {
            self.seek(SeekFrom::Start(offset))?;
            self.read_exact(bytes)
        }
    }

    pub(crate) fn write_all_at(&mut self, bytes: &[u8], offset: u64) -> io::Result<()> {
        if bytes.is_empty() { return Ok(()); }
        let end = offset.checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("replay scratch extent overflow"))?;
        // Charge the entire requested extent before any write, including holes.
        // A failed/partial write retains this reservation until owner cleanup.
        self.reserve(end)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(bytes, offset)
        }
        #[cfg(not(unix))]
        {
            self.seek(SeekFrom::Start(offset))?;
            self.write_all(bytes)
        }
    }
    fn reserve(&mut self, end: u64) -> io::Result<()> {
        if end <= self.reserved { return Ok(()); }
        let mut usage = lock(&self.account.usage)?;
        if self.policy.is_none() { self.policy = usage.policy.upgrade(); }
        let bytes = usage.bytes.checked_add(end - self.reserved)
            .filter(|bytes| usage.limit.is_none_or(|limit| *bytes <= limit))
            .ok_or_else(|| {
                #[cfg(test)]
                if std::env::var_os("BURST_SCRATCH_FAILURE_DIAGNOSTIC").is_some() {
                    eprintln!("SCRATCH_REJECT used={} limit={:?} file_reserved={} requested_end={}\n{}",
                        usage.bytes, usage.limit, self.reserved, end,
                        std::backtrace::Backtrace::force_capture());
                    let mut owners: Vec<_> = usage.files.values().collect();
                    owners.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
                    for (bytes, allocation) in owners.into_iter().take(12) {
                        eprintln!("SCRATCH_REJECT_OWNER bytes={bytes} allocation=\n{allocation}");
                    }
                    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
                        for line in status.lines().filter(|line| ["VmRSS:", "VmHWM:", "RssAnon:", "RssFile:"]
                            .iter().any(|prefix| line.starts_with(prefix))) { eprintln!("SCRATCH_REJECT_MEMORY {line}"); }
                    }
                }
                io::Error::other("aggregate replay scratch byte budget exceeded")
            })?;
        usage.bytes = bytes;
        self.reserved = end;
        #[cfg(test)]
        if let Some(id) = self.diagnostic_id {
            if let Some(entry) = usage.files.get_mut(&id) { entry.0 = end; }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_len(&mut self, length: u64) -> io::Result<()> {
        self.reserve(length)?;
        self.file.set_len(length)
    }
}
impl Drop for ScratchFile {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.account.usage.lock() {
            usage.bytes -= self.reserved;
            #[cfg(test)]
            if let Some(id) = self.diagnostic_id { usage.files.remove(&id); }
        }
    }
}
impl Read for ScratchFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(bytes)?;
        self.position += read as u64;
        Ok(read)
    }
}
impl Seek for ScratchFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = self.file.seek(position)?;
        Ok(self.position)
    }
}
impl Write for ScratchFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() { return Ok(0); }
        let end = self.position.checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("replay scratch extent overflow"))?;
        self.reserve(end)?;
        let written = self.file.write(bytes)?;
        self.position += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> { self.file.flush() }
}
#[cfg(all(test, target_os = "linux"))]
impl std::os::fd::AsRawFd for ScratchFile {
    fn as_raw_fd(&self) -> std::os::fd::RawFd { std::os::fd::AsRawFd::as_raw_fd(&self.file) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_policy_survives_parent_drop_and_checks_reservation_identity() {
        let directory = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let parent = ScratchLimit::acquire_reserved(directory.path(), 10, 6).unwrap();
        let child = parent.clone();
        child.check_persistent(&directory.path().join("."), 6).unwrap();
        assert!(child.check_persistent(directory.path(), 7).is_err());
        assert!(child.check_persistent(foreign.path(), 1).is_err());
        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        file.write_all(&[0;4]).unwrap();
        drop(parent);
        assert!(file.write_all(&[0]).is_err());
        assert!(ScratchLimit::acquire(directory.path(), 10).is_err());
        drop(child);
        // The file itself retains enforcement after both explicit owners drop.
        assert!(file.write_all(&[0]).is_err());
        assert!(ScratchLimit::acquire(directory.path(), 10).is_err());
        let shared = ScratchLimit::acquire_shared_reserved(directory.path(), 10, 6).unwrap();
        assert!(ScratchLimit::acquire_shared_reserved(directory.path(), 11, 6).is_err());
        assert!(ScratchLimit::acquire_shared_reserved(directory.path(), 10, 7).is_err());
        assert!(file.write_all(&[0]).is_err());
        drop(shared);
        drop(file);
        let next = ScratchLimit::acquire(directory.path(), 5).unwrap();
        drop(next);
    }

    #[test]
    fn retained_record_forks_keep_the_budget_and_shared_recovery_cannot_expand_it() {
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire_reserved(directory.path(), 4096, 1024).unwrap();
        let mut records = super::super::scratch_records::ScratchRecords::<Vec<u8>>::new(directory.path()).unwrap();
        records.put([1; 32], &vec![1; 16]).unwrap();
        let mut successor = records.fork().unwrap();
        drop(records);
        drop(limit);
        let next = ScratchLimit::acquire_shared_reserved(directory.path(), 4096, 1024).unwrap();
        assert!(ScratchLimit::acquire_shared_reserved(directory.path(), 8192, 1024).is_err());
        assert!(ScratchLimit::acquire_shared_reserved(directory.path(), 4096, 2048).is_err());
        assert!(successor.put([2; 32], &vec![2; 4096]).is_err());
        drop(next);
        assert!(ScratchLimit::acquire(directory.path(), 4096).is_err());
        drop(successor);
        drop(ScratchLimit::acquire(directory.path(), 1).unwrap());
    }

    #[test]
    fn retained_input_extents_reduce_budget_after_owner_and_registry_drop() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join(PERSISTENT_INPUTS_DIRECTORY);
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("data.mdb"), [0;6]).unwrap();
        std::fs::write(cache.join("lock.mdb"), [0;2]).unwrap();
        for _ in 0..2 {
            let lease = ScratchLimit::acquire(directory.path(), 10).unwrap();
            let mut file = ScratchFile::new_in(directory.path()).unwrap();
            file.write_all(&[0;2]).unwrap();
            assert!(file.write_all(&[0]).is_err());
            drop(file); drop(lease);
        }
        let lease = ScratchLimit::acquire_reserved(directory.path(), 20, 18).unwrap();
        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        assert!(file.write_all(&[0;3]).is_err());
        drop(file); drop(lease);
        std::fs::write(cache.join("unexpected"), [0]).unwrap();
        assert!(ScratchLimit::acquire(directory.path(), 100).is_err());
    }

    #[test]
    fn budget_includes_existing_files_and_releases_only_dropped_owners() {
        let directory = tempfile::tempdir().unwrap();
        let mut prior = ScratchFile::new_in(directory.path()).unwrap();
        prior.write_all(&[1; 6]).unwrap();
        assert!(ScratchLimit::acquire(directory.path(), 5).is_err());
        let lease = ScratchLimit::acquire(directory.path(), 10).unwrap();
        assert!(ScratchLimit::acquire(directory.path(), 10).is_err());
        let mut next = ScratchFile::new_in(&directory.path().join(".")).unwrap();
        next.write_all(&[2; 4]).unwrap();
        assert!(prior.write_all(&[3]).is_err());
        assert_eq!(prior.metadata().unwrap().len(), 6);
        drop(next);
        prior.write_all(&[3; 4]).unwrap();
        drop(lease);
        assert!(prior.write_all(&[4]).is_err(), "growth adopted the active policy");
        assert!(ScratchLimit::acquire(directory.path(), 10).is_err());
        drop(prior);
        assert!(ScratchLimit::acquire(directory.path(), 1).is_ok());
    }
    #[test]
    fn overwrite_seek_and_truncation_do_not_evade_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let _lease = ScratchLimit::acquire(directory.path(), 10).unwrap();
        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        file.seek(SeekFrom::Start(9)).unwrap();
        file.write_all(&[1]).unwrap();
        file.rewind().unwrap();
        file.write_all(&[2; 10]).unwrap();
        file.set_len(0).unwrap();
        let mut other = ScratchFile::new_in(directory.path()).unwrap();
        assert!(other.write_all(&[1]).is_err());
        file.seek(SeekFrom::Start(10)).unwrap();
        assert!(file.write_all(&[1]).is_err());
        drop(file);
        other.write_all(&[1; 10]).unwrap();
    }
    #[test]
    fn concurrent_files_cannot_overcommit_one_directory() {
        let directory = tempfile::tempdir().unwrap();
        let lease = ScratchLimit::acquire(directory.path(), 10).unwrap();
        let files: Vec<_> = (0..8).map(|_| ScratchFile::new_in(directory.path()).unwrap()).collect();
        let barrier = Arc::new(std::sync::Barrier::new(files.len()));
        let handles: Vec<_> = files.into_iter().map(|mut file| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let result = file.write_all(&[1; 6]);
                (file, result)
            })
        }).collect();
        let retained: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        assert_eq!(retained.iter().filter(|(_, result)| result.is_ok()).count(), 1);
        assert_eq!(lock(&lease.0.account.usage).unwrap().bytes, 6);
        drop(retained);
        assert_eq!(lock(&lease.0.account.usage).unwrap().bytes, 0);
    }

    #[test]
    fn failed_write_retains_reservation_and_record_failure_poisoning() {
        let directory = tempfile::tempdir().unwrap();
        let lease = ScratchLimit::acquire(directory.path(), 100).unwrap();
        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        // A read-only descriptor forces a write failure after reservation.
        let readonly = directory.path().join("readonly");
        std::fs::write(&readonly, []).unwrap();
        file.file = File::open(readonly).unwrap();
        assert!(file.write_all(&[0; 100]).is_err());
        assert_eq!(lock(&lease.0.account.usage).unwrap().bytes, 100);
        drop(file);
        drop(lease);
        let lease = ScratchLimit::acquire(directory.path(), 90).unwrap();
        let mut records = super::super::scratch_records::ScratchRecords::<u64>::new(directory.path()).unwrap();
        let fork = records.fork().unwrap();
        // The 8-byte payload and 84-byte descriptor leaf exceed the 90-byte
        // aggregate limit even though either file fits.
        // Partial publication must still poison every fork.
        assert!(records.put([1; 32], &7).is_err());
        assert!(records.get(&[1; 32]).is_err());
        assert!(fork.get(&[1; 32]).is_err());
        drop(records);
        assert!(lock(&lease.0.account.usage).unwrap().bytes > 0);
        drop(fork);
        assert_eq!(lock(&lease.0.account.usage).unwrap().bytes, 0);
    }

    #[test]
    fn absolute_io_preserves_extent_budget_short_reads_and_failed_write_reservations() {
        let directory = tempfile::tempdir().unwrap();
        let lease = ScratchLimit::acquire(directory.path(), 16).unwrap();
        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        file.write_all_at(&[1, 2, 3, 4, 5, 6, 7, 8], 4).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 12);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 12);
        file.write_all_at(&[9, 10], 6).unwrap();
        let mut bytes = [0; 12];
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, [0, 0, 0, 0, 1, 2, 9, 10, 5, 6, 7, 8]);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 12);
        assert!(file.write_all_at(&[0; 5], 12).is_err());
        assert!(file.write_all_at(&[0; 2], u64::MAX).is_err());
        file.write_all_at(&[], u64::MAX).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 12);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 12);
        assert_eq!(file.read_exact_at(&mut [0; 3], 10).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        // Sequential callers still explicitly position their own reads/writes.
        file.seek(SeekFrom::Start(1)).unwrap();
        file.write_all(&[11]).unwrap();
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes[1], 11);
        file.set_len(2).unwrap();
        // Truncation does not release the owner's conservative reservation.
        assert_eq!(file.metadata().unwrap().len(), 2);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 12);
        assert!(file.read_exact_at(&mut [0; 1], 2).is_err());
        drop(file);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 0);

        let mut file = ScratchFile::new_in(directory.path()).unwrap();
        let readonly = directory.path().join("absolute-readonly");
        std::fs::write(&readonly, []).unwrap();
        file.file = File::open(readonly).unwrap();
        assert!(file.write_all_at(&[0; 8], 8).is_err());
        assert_eq!(lease.used_temporary_bytes().unwrap(), 16);
        drop(file);
        assert_eq!(lease.used_temporary_bytes().unwrap(), 0);
    }

}
