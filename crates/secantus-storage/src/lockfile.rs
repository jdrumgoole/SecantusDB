//! mongod's `mongod.lock` (`storage_engine_lock_file_posix.cpp`): a file in
//! the data directory, taken with an exclusive `flock` before the storage
//! engine opens, holding the process id while the store is open and emptied
//! by a clean shutdown. A lock file that is not empty when the store opens
//! is how mongod knows the last run did not shut down cleanly: it warns
//! `Detected unclean shutdown - Lock file is not empty` (22271) and reports
//! `Startup from clean shutdown?: false` on `mongod startup complete`
//! (measured 8.2.11, 2026-11-13). A missing file is a clean start, as on a
//! new dbpath.
//!
//! Only the MongoDB server asks for one ([`crate::StorageOptions::lock_file`]):
//! the PostgreSQL server keeps its own `postmaster.pid`-style state.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{Result, StorageError};

/// What taking the lock file found, for the server's startup lines.
#[derive(Debug, Clone, PartialEq)]
pub struct LockFileRecord {
    /// The lock file's path as mongod prints it: the data directory as given,
    /// joined with the file name.
    pub path: String,
    /// The file was not empty: the last run did not shut down cleanly.
    pub unclean: bool,
    /// How long taking it took (mongod's `createLockFileMillis`).
    pub took: Duration,
}

/// The held lock file. Emptied as it drops, and only then, when the store
/// it guards closed cleanly ([`LockFile::mark_clean`]).
pub(crate) struct LockFile {
    file: File,
    path: String,
    clean: AtomicBool,
}

impl LockFile {
    /// Take `home`/`name`: create it if missing (mode 0600), lock it, note
    /// whether it was empty, and write this process's id into it, synced, as
    /// mongod's `StorageEngineLockFile::open` and `writePid` do.
    pub(crate) fn acquire(home: &str, name: &str) -> Result<(LockFile, LockFileRecord)> {
        let started = Instant::now();
        let path = Path::new(home).join(name);
        let shown = path.to_string_lossy().into_owned();
        let io = |what: &str, e: std::io::Error| {
            StorageError::Internal(format!("Unable to {what} the lock file: {shown} ({e})"))
        };
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| io("create/open", e))?;
        match file.try_lock() {
            Ok(()) => {}
            // mongod 8.2.11's DBPathInUse, word for word.
            Err(TryLockError::WouldBlock) => {
                return Err(StorageError::Internal(format!(
                    "Unable to lock the lock file: {shown} (Resource temporarily unavailable). \
                     Another mongod instance is already running on the {home} directory"
                )));
            }
            Err(TryLockError::Error(e)) => return Err(io("lock", e)),
        }
        let unclean = file.metadata().map_err(|e| io("read", e))?.len() > 0;
        file.set_len(0).map_err(|e| io("write to", e))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| io("write to", e))?;
        writeln!(file, "{}", std::process::id()).map_err(|e| io("write to", e))?;
        file.sync_all().map_err(|e| io("flush", e))?;
        let record = LockFileRecord {
            path: shown.clone(),
            unclean,
            took: started.elapsed(),
        };
        Ok((
            LockFile {
                file,
                path: shown,
                clean: AtomicBool::new(false),
            },
            record,
        ))
    }

    /// The store closed cleanly: empty the file when it drops.
    pub(crate) fn mark_clean(&self) {
        self.clean.store(true, Ordering::Release);
    }
}

impl Drop for LockFile {
    /// mongod's `clearPidAndUnlock`: empty the file, synced, so the next start
    /// reads a clean shutdown; the lock goes with the handle. After an unclean
    /// close the process id stays, and the next start warns of it. A failure
    /// to empty it can only be reported here: the next start will call the
    /// shutdown unclean, which is the safe reading.
    fn drop(&mut self) {
        if !self.clean.load(Ordering::Acquire) {
            return;
        }
        if let Err(e) = self.file.set_len(0).and_then(|()| self.file.sync_all()) {
            eprintln!(
                "secantus-storage: emptying the lock file {} failed: {e}",
                self.path
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_new_directory_is_a_clean_start_and_holds_the_pid() {
        let dir = home();
        let h = dir.path().to_str().unwrap();
        let (lock, record) = LockFile::acquire(h, "mongod.lock").unwrap();
        assert!(!record.unclean);
        assert_eq!(record.path, format!("{h}/mongod.lock"));
        let text = std::fs::read_to_string(dir.path().join("mongod.lock")).unwrap();
        assert_eq!(text, format!("{}\n", std::process::id()));
        lock.mark_clean();
        drop(lock);
        assert_eq!(
            std::fs::metadata(dir.path().join("mongod.lock"))
                .unwrap()
                .len(),
            0
        );
        let (_again, record) = LockFile::acquire(h, "mongod.lock").unwrap();
        assert!(!record.unclean, "an emptied file is a clean shutdown");
    }

    #[test]
    fn a_file_left_holding_a_pid_is_an_unclean_shutdown() {
        let dir = home();
        let h = dir.path().to_str().unwrap();
        let (lock, _) = LockFile::acquire(h, "mongod.lock").unwrap();
        drop(lock); // never marked clean
        let (_again, record) = LockFile::acquire(h, "mongod.lock").unwrap();
        assert!(record.unclean);
    }

    #[test]
    fn a_held_lock_refuses_a_second_open() {
        let dir = home();
        let h = dir.path().to_str().unwrap();
        let (_held, _) = LockFile::acquire(h, "mongod.lock").unwrap();
        let err = match LockFile::acquire(h, "mongod.lock") {
            Err(StorageError::Internal(m)) => m,
            other => panic!("expected a refusal, got {:?}", other.map(|(_, r)| r)),
        };
        assert!(
            err.starts_with("Unable to lock the lock file: ")
                && err.ends_with(&format!(
                    "Another mongod instance is already running on the {h} directory"
                )),
            "{err}"
        );
    }
}
