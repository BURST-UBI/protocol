use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use heed::{Env, EnvOpenOptions};

static ENVIRONMENTS: LazyLock<Mutex<HashMap<PathBuf, Weak<Env>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
pub(crate) struct SharedEnv(Option<Arc<Env>>);

impl SharedEnv {
    /// Default LMDB commits flush data before synchronously writing metadata.
    /// Do not flush them a second time. Any non-default flag combination keeps
    /// the explicit barrier, including relaxed durability modes used by callers.
    pub(crate) fn sync_after_commit(&self) -> heed::Result<()> {
        if self.get_flags()? != 0 {
            self.force_sync()?;
        }
        Ok(())
    }

    pub(crate) fn open(path: &Path, options: EnvOpenOptions) -> heed::Result<Self> {
        let mut environments = ENVIRONMENTS.lock().unwrap();
        // Heed validates matching options and canonicalizes aliases for existing environments.
        let env = unsafe { options.open(path)? };
        let entry = environments.entry(env.path().to_path_buf()).or_default();
        let shared = match entry.upgrade() {
            Some(shared) => shared,
            None => {
                let shared = Arc::new(env);
                *entry = Arc::downgrade(&shared);
                shared
            }
        };
        Ok(Self(Some(shared)))
    }
}

impl Deref for SharedEnv {
    type Target = Env;

    fn deref(&self) -> &Env {
        self.0.as_ref().unwrap()
    }
}

impl Drop for SharedEnv {
    fn drop(&mut self) {
        let mut environments = ENVIRONMENTS.lock().unwrap();
        let env = self.0.take().unwrap();
        if Arc::strong_count(&env) == 1 {
            environments.remove(env.path());
            // Heed 0.20 retains its own reference until closing is requested.
            // This releases that reference, never force-closing live transactions.
            env.as_ref().clone().prepare_for_closing();
        }
        // Drop this reference before releasing the opener lock. Any remaining
        // raw Env clones keep the environment in its closing state.
        drop(env);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use heed::{types::Bytes, EnvFlags, FlagSetMode};

    #[test]
    fn post_commit_barrier_preserves_default_and_relaxed_durability() {
        fn marker(name: &str, phase: &str) {
            use std::io::Write;
            std::io::stderr().write_all(format!("BURST_DURABILITY {name} {phase}\n").as_bytes()).unwrap();
        }
        for (name, flags) in [
            ("default", EnvFlags::empty()),
            ("no_sync", EnvFlags::NO_SYNC),
            ("no_meta_sync", EnvFlags::NO_META_SYNC),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut options = EnvOpenOptions::new();
            options.max_dbs(1).map_size(1 << 20);
            let env = SharedEnv::open(dir.path(), options.clone()).unwrap();
            let mut txn = env.write_txn().unwrap();
            let db = env.create_database::<Bytes, Bytes>(&mut txn, Some("intent")).unwrap();
            txn.commit().unwrap();
            // Isolated environment, no concurrent flag changes or transactions.
            if !flags.is_empty() { unsafe { env.set_flags(flags, FlagSetMode::Enable).unwrap(); } }
            assert_eq!(env.get_flags().unwrap(), flags.bits());
            let mut txn = env.write_txn().unwrap();
            db.put(&mut txn, b"final", b"choice").unwrap();
            marker(name, "commit_start");
            txn.commit().unwrap();
            marker(name, "barrier_start");
            env.sync_after_commit().unwrap();
            marker(name, "durable_return");
            drop(env);
            let reopened = SharedEnv::open(dir.path(), options).unwrap();
            let txn = reopened.read_txn().unwrap();
            let db = reopened.open_database::<Bytes, Bytes>(&txn, Some("intent")).unwrap().unwrap();
            assert_eq!(db.get(&txn, b"final").unwrap(), Some(b"choice".as_slice()));
        }
    }
}
