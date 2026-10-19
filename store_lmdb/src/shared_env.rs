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
        // Finish closing before another opener can acquire this path.
        drop(env);
    }
}
