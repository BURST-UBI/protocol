use std::borrow::Cow;
use std::ops::Deref;
use std::ptr;

use crate::mdb::error::mdb_result;
use crate::mdb::ffi;
use crate::{Env, Result};

/// A read-only transaction.
///
/// ## LMDB Limitations
///
/// It's a must to keep read transactions short-lived.
///
/// Active Read transactions prevent the reuse of pages freed
/// by newer write transactions, thus the database can grow quickly.
///
/// ## OSX/Darwin Limitation
///
/// At least 10 transactions can be active at the same time in the same process, since only 10 POSIX semaphores can
/// be active at the same time for a process. Threads are in the same process space.
///
/// If the process crashes in the POSIX semaphore locking section of the transaction, the semaphore will be kept locked.
///
/// Note: if your program already use POSIX semaphores, you will have less available for heed/LMDB!
///
/// You may increase the limit by editing it **at your own risk**: `/Library/LaunchDaemons/sysctl.plist`
pub struct RoTxn<'e> {
    pub(crate) txn: *mut ffi::MDB_txn,
    env: Cow<'e, Env>,
}

impl<'e> RoTxn<'e> {
    /// Return the identifier of this transaction's pinned snapshot.
    pub fn id(&self) -> usize {
        // The transaction pointer remains valid for the lifetime of this borrow.
        unsafe { lmdb_master_sys::mdb_txn_id(self.txn) }
    }

    pub(crate) fn new(env: &'e Env) -> Result<RoTxn<'e>> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();

        unsafe {
            mdb_result(ffi::mdb_txn_begin(
                env.env_mut_ptr(),
                ptr::null_mut(),
                ffi::MDB_RDONLY,
                &mut txn,
            ))?
        };

        Ok(RoTxn { txn, env: Cow::Borrowed(env) })
    }

    pub(crate) fn static_read_txn(env: Env) -> Result<RoTxn<'static>> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();

        unsafe {
            mdb_result(ffi::mdb_txn_begin(
                env.env_mut_ptr(),
                ptr::null_mut(),
                ffi::MDB_RDONLY,
                &mut txn,
            ))?
        };

        Ok(RoTxn { txn, env: Cow::Owned(env) })
    }

    pub(crate) fn env_mut_ptr(&self) -> *mut ffi::MDB_env {
        self.env.env_mut_ptr()
    }

    /// Commit a read transaction.
    ///
    /// Synchronizing some [`Env`] metadata with the global handle.
    ///
    /// ## LMDB
    ///
    /// It's mandatory in a multi-process setup to call [`RoTxn::commit`] upon read-only database opening.
    /// After the transaction opening, the database is dropped. The next transaction might return
    /// `Io(Os { code: 22, kind: InvalidInput, message: "Invalid argument" })` known as `EINVAL`.
    pub fn commit(mut self) -> Result<()> {
        let result = unsafe { mdb_result(ffi::mdb_txn_commit(self.txn)) };
        self.txn = ptr::null_mut();
        result.map_err(Into::into)
    }
}

impl Drop for RoTxn<'_> {
    fn drop(&mut self) {
        if !self.txn.is_null() {
            abort_txn(self.txn);
        }
    }
}

#[cfg(feature = "read-txn-no-tls")]
unsafe impl Send for RoTxn<'_> {}

fn abort_txn(txn: *mut ffi::MDB_txn) {
    // Asserts that the transaction hasn't been already committed.
    assert!(!txn.is_null());
    unsafe { ffi::mdb_txn_abort(txn) }
}

/// A read-write transaction.
///
/// ## LMDB Limitations
///
/// Only one [`RwTxn`] may exist in the same environment at the same time.
/// If two exist, the new one may wait on a mutex for [`RwTxn::commit`] or [`RwTxn::abort`] to
/// be called for the first one.
///
/// ## OSX/Darwin Limitation
///
/// At least 10 transactions can be active at the same time in the same process, since only 10 POSIX semaphores can
/// be active at the same time for a process. Threads are in the same process space.
///
/// If the process crashes in the POSIX semaphore locking section of the transaction, the semaphore will be kept locked.
///
/// Note: if your program already use POSIX semaphores, you will have less available for heed/LMDB!
///
/// You may increase the limit by editing it **at your own risk**: `/Library/LaunchDaemons/sysctl.plist`
pub struct RwTxn<'p> {
    pub(crate) txn: RoTxn<'p>,
    growth: Option<crate::GrowthOutcome>,
}

impl<'p> RwTxn<'p> {
    pub(crate) fn new(env: &'p Env) -> Result<RwTxn<'p>> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();

        unsafe { mdb_result(ffi::mdb_txn_begin(env.env_mut_ptr(), ptr::null_mut(), 0, &mut txn))? };

        let mut writer = RwTxn { txn: RoTxn { txn, env: Cow::Borrowed(env) }, growth: None };
        writer.growth = env.growth_outcome();
        if let Some(outcome) = &writer.growth {
            unsafe { mdb_result(ffi::mdb_txn_limit_file_extent(txn, outcome.bytes()))? };
        }
        Ok(writer)
    }

    /// Install an absolute file-extent ceiling for this top-level writer and
    /// subsequent writers in this process. New native pages (including commit
    /// metadata) are checked before allocation; reusable pages remain available.
    /// A nested writer cannot acquire a lease. A second lease is rejected.
    /// The caller must coordinate cancellation/retry on MdbError::GrowthLimit.
    pub fn limit_file_extent(&mut self, bytes: usize) -> Result<crate::FileGrowthLimit> {
        self.try_limit_file_extent(bytes)?
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EBUSY).into())
    }

    /// None defers capture while another lease or admitted retry owns priority.
    pub fn try_limit_file_extent(&mut self, bytes: usize) -> Result<Option<crate::FileGrowthLimit>> {
        if self.growth.is_some() { return Ok(None); }
        if self.txn.env.real_disk_size()? > bytes as u64 {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL).into());
        }
        let Some(lease) = self.txn.env.acquire_growth_limit(bytes)? else { return Ok(None); };
        unsafe { mdb_result(ffi::mdb_txn_limit_file_extent(self.txn.txn, bytes))? };
        self.growth = Some(lease.outcome());
        Ok(Some(lease))
    }

    /// Retain this transaction's growth outcome for inspection after it ends.
    pub fn growth_outcome(&self) -> Option<crate::GrowthOutcome> { self.growth.clone() }

    /// Reserve a retry under the current growth lease, if one is active.
    pub fn reserve_growth_retry(&self) -> Result<Option<crate::GrowthRetry>> {
        self.growth.as_ref().map(crate::GrowthOutcome::reserve_retry).transpose()
    }

    pub(crate) fn nested(env: &'p Env, parent: &'p mut RwTxn) -> Result<RwTxn<'p>> {
        let mut txn: *mut ffi::MDB_txn = ptr::null_mut();
        let parent_ptr: *mut ffi::MDB_txn = parent.txn.txn;

        unsafe { mdb_result(ffi::mdb_txn_begin(env.env_mut_ptr(), parent_ptr, 0, &mut txn))? };

        Ok(RwTxn { txn: RoTxn { txn, env: Cow::Borrowed(env) },
            growth: parent.growth.as_ref().map(crate::GrowthOutcome::nested) })
    }

    pub(crate) fn env_mut_ptr(&self) -> *mut ffi::MDB_env {
        self.txn.env.env_mut_ptr()
    }

    /// Commit all the operations of a transaction into the database.
    /// The transaction is reset.
    pub fn commit(mut self) -> Result<()> {
        self.record_growth_rejection();
        let result = unsafe { mdb_result(ffi::mdb_txn_commit(self.txn.txn)) };
        self.txn.txn = ptr::null_mut();
        if matches!(result, Err(crate::MdbError::GrowthLimit)) {
            if let Some(outcome) = &self.growth { outcome.reject(); }
        }
        result.map_err(Into::into)
    }

    /// Abandon all the operations of the transaction instead of saving them.
    /// The transaction is reset.
    pub fn abort(mut self) {
        self.record_growth_rejection();
        abort_txn(self.txn.txn);
        self.txn.txn = ptr::null_mut();
    }
}

impl RwTxn<'_> {
    fn record_growth_rejection(&self) {
        if let Some(outcome) = &self.growth {
            if !self.txn.txn.is_null() && unsafe { ffi::mdb_txn_growth_rejected(self.txn.txn) } != 0 {
                outcome.reject();
            }
        }
    }
}
impl Drop for RwTxn<'_> {
    fn drop(&mut self) { self.record_growth_rejection(); }
}

impl<'p> Deref for RwTxn<'p> {
    type Target = RoTxn<'p>;

    fn deref(&self) -> &Self::Target {
        &self.txn
    }
}
