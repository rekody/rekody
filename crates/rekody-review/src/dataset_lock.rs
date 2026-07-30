//! Cross-process lock serializing writers of a dataset's `manifest.jsonl`.
//!
//! Two independent processes write the manifest: the dictation daemon
//! APPENDS a row when a training pair is saved, and the review store
//! REWRITES the whole file (read, mutate, rename) when a decision lands.
//! An append that lands between the store's read and its rename is
//! silently discarded by the rename — a user's dictation vanishes from
//! the dataset while its audio file stays behind (issue #114).
//!
//! The lock is an advisory exclusive file lock on a `manifest.jsonl.lock`
//! sidecar (never on the manifest itself: the rewrite path renames over
//! the manifest, which would orphan a lock held on the old inode).
//! Rewriters block until the lock is theirs; the daemon's append uses a
//! bounded wait and, on timeout, appends anyway with a warning — losing
//! the lock guarantee beats losing a user's dictation to a wedged
//! reviewer.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;

/// RAII guard: the lock releases when dropped (and, as a backstop, when
/// the owning process exits — advisory locks die with their process).
#[derive(Debug)]
pub struct DatasetLock {
    file: File,
}

impl Drop for DatasetLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_file(root: &Path) -> Result<File> {
    File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(root.join("manifest.jsonl.lock"))
        .context("opening manifest.jsonl.lock")
}

/// Block until the dataset's manifest lock is acquired. For rewriters
/// (the review store), where waiting on a concurrent append is always
/// the right call — appends hold the lock for microseconds.
pub fn lock_exclusive(root: &Path) -> Result<DatasetLock> {
    let file = lock_file(root)?;
    FileExt::lock_exclusive(&file).context("locking manifest.jsonl.lock")?;
    Ok(DatasetLock { file })
}

/// Try to acquire the lock, polling for at most `timeout`. Returns
/// `None` when it stays contended — the caller decides whether to
/// proceed unlocked (the daemon's append path does, with a warning).
pub fn lock_with_timeout(root: &Path, timeout: Duration) -> Result<Option<DatasetLock>> {
    let file = lock_file(root)?;
    let deadline = Instant::now() + timeout;
    loop {
        if FileExt::try_lock_exclusive(&file).context("try-locking manifest.jsonl.lock")? {
            return Ok(Some(DatasetLock { file }));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_then_timeout_then_reacquire() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock_exclusive(dir.path()).unwrap();
        // Contended: a bounded wait gives up rather than deadlocking.
        let missed = lock_with_timeout(dir.path(), Duration::from_millis(120)).unwrap();
        assert!(missed.is_none());
        drop(held);
        let got = lock_with_timeout(dir.path(), Duration::from_millis(120)).unwrap();
        assert!(got.is_some());
    }
}
