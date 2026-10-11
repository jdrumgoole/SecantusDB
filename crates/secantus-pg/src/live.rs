//! What a running server re-reads on a reload: its TLS files, and its
//! `pg_hba.conf` with the user name maps that names. PostgreSQL does the
//! same on `pg_ctl reload`: a connection made afterwards gets what the files
//! hold now, one already open keeps what it has, and a file that cannot be
//! used leaves the server with what it had.

use std::io;
use std::sync::Arc;

use crate::hba::LiveHba;
use crate::tls::LiveTls;

pub(crate) struct Live {
    pub tls: Option<LiveTls>,
    pub hba: Option<LiveHba>,
}

impl Live {
    /// Read every file again. Each kind stands alone, as in PostgreSQL: a
    /// `pg_hba.conf` that cannot be read does not stop new TLS files from
    /// being taken up, and the reverse. The error is the first met.
    fn reload(&self) -> io::Result<()> {
        let hba = self.hba.as_ref().map_or(Ok(()), LiveHba::reload);
        let tls = self.tls.as_ref().map_or(Ok(()), LiveTls::reload);
        hba.and(tls)
    }

    /// Is there anything to re-read?
    fn has_files(&self) -> bool {
        self.tls.is_some() || self.hba.is_some()
    }
}

/// Re-reads a running server's login files: the TLS certificate and key,
/// the CA file and revocation list, `pg_hba.conf`, and the user name map.
/// What PostgreSQL does on `pg_ctl reload`.
#[derive(Clone)]
pub struct Reloader(pub(crate) Arc<Live>);

impl Reloader {
    /// Read the files again. A connection made from here on gets what they
    /// hold now. On an error the server keeps what it had for that file.
    pub fn reload(&self) -> io::Result<()> {
        self.0.reload()
    }

    /// Whether the server was started with any file a reload would read.
    pub fn has_files(&self) -> bool {
        self.0.has_files()
    }
}

impl std::fmt::Debug for Reloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reloader")
    }
}
