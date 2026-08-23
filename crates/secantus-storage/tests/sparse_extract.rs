//! Backup extraction must punch holes rather than write runs of zeros.
//!
//! A WiredTiger backup contains `WiredTigerLog.*`, preallocated by WT to
//! `log_file_max` (2 GiB). It is almost entirely zeros, so it compresses to
//! nothing and expands to full size on restore: a store holding 100 documents
//! archives to 2.0 MB and extracted to 2.0 GB, with every PITR restore writing
//! 2 GB regardless of database size. Under a loaded disk that took 858s
//! against a 900s timeout — the intermittent
//! `test_rust_binary_v2_archive_base_snapshot_and_restore` failure.
//!
//! The fix is extraction-side only, so the guarantee these tests pin is that
//! the restored bytes are unchanged: holes read back as zeros, so a sparse
//! file and a fully-written one are indistinguishable to WiredTiger.

use flate2::write::GzEncoder;
use flate2::Compression;
use secantus_storage::extract_backup_archive;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "secantus-sparse-{tag}-{}-{}",
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a `.tar.gz` from `(name, bytes)` pairs.
fn make_archive(path: &std::path::Path, files: &[(&str, Vec<u8>)]) {
    let f = std::fs::File::create(path).unwrap();
    let enc = GzEncoder::new(f, Compression::default());
    let mut builder = tar::Builder::new(enc);
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, data.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

/// The headline case: a large all-zero entry, exactly the preallocated WT log.
/// It must restore to the right length with the right bytes, while occupying
/// far less disk than its logical size.
#[test]
fn zero_filled_log_restores_byte_identical_but_sparse() {
    let dir = temp_dir("zerolog");
    let archive = dir.join("backup.tar.gz");
    let size = 64 * 1024 * 1024; // 64 MiB of zeros — the 2 GiB case in miniature
    make_archive(
        &archive,
        &[
            ("WiredTiger", b"wt metadata".to_vec()),
            ("WiredTigerLog.0000000001", vec![0u8; size]),
        ],
    );

    let target = dir.join("restored");
    extract_backup_archive(archive.to_str().unwrap(), target.to_str().unwrap()).unwrap();

    let log = target.join("WiredTigerLog.0000000001");
    let meta = std::fs::metadata(&log).unwrap();
    assert_eq!(
        meta.len(),
        size as u64,
        "restored log must keep its logical length"
    );
    assert!(
        std::fs::read(&log).unwrap().iter().all(|&b| b == 0),
        "restored log must read back as all zeros"
    );

    // The point of the change: the zeros occupy (almost) no blocks. Allow a
    // little slack for filesystem metadata and any non-sparse tail.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let allocated = meta.blocks() * 512;
        assert!(
            allocated < size as u64 / 4,
            "expected a sparse file, but {allocated} bytes are allocated for a \
             {size}-byte all-zero file — extraction is still writing the zeros"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Non-zero content must survive untouched, including a file that mixes data
/// with embedded zero runs — the case a naive "skip zeros" implementation gets
/// wrong by dropping interior holes or misplacing the data after them.
#[test]
fn mixed_content_round_trips_exactly() {
    let dir = temp_dir("mixed");
    let archive = dir.join("backup.tar.gz");

    let mut mixed = Vec::new();
    mixed.extend_from_slice(b"header-bytes");
    mixed.extend(std::iter::repeat_n(0u8, 1024 * 1024)); // interior hole
    mixed.extend_from_slice(b"middle-bytes");
    mixed.extend(std::iter::repeat_n(0u8, 512 * 1024));
    mixed.extend_from_slice(b"trailing-bytes");
    // A file ending in zeros: the tail must still extend the file.
    let mut zero_tail = b"data-then-zeros".to_vec();
    zero_tail.extend(std::iter::repeat_n(0u8, 3 * 1024 * 1024));

    make_archive(
        &archive,
        &[
            ("WiredTiger", b"wt metadata".to_vec()),
            ("mixed.wt", mixed.clone()),
            ("zero_tail.wt", zero_tail.clone()),
        ],
    );

    let target = dir.join("restored");
    extract_backup_archive(archive.to_str().unwrap(), target.to_str().unwrap()).unwrap();

    assert_eq!(
        std::fs::read(target.join("mixed.wt")).unwrap(),
        mixed,
        "interior zero runs must not shift or drop surrounding data"
    );
    assert_eq!(
        std::fs::read(target.join("zero_tail.wt")).unwrap(),
        zero_tail,
        "a file ending in zeros must keep its full length"
    );
    assert_eq!(
        std::fs::read(target.join("WiredTiger")).unwrap(),
        b"wt metadata",
        "small metadata files must be unaffected"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Path traversal stays refused. `tar::Archive::unpack` guards this itself, so
/// replacing it must not quietly drop the protection.
///
/// The malicious entry is built by writing the name bytes straight into the
/// header: `tar::Builder::append_data` calls `Header::set_path`, which refuses
/// `..` at *write* time — so an archive like this cannot be produced with the
/// normal API, only encountered.
#[test]
fn traversal_paths_are_refused() {
    let dir = temp_dir("traversal");
    let archive = dir.join("evil.tar.gz");

    {
        let f = std::fs::File::create(&archive).unwrap();
        let enc = GzEncoder::new(f, Compression::default());
        let mut builder = tar::Builder::new(enc);

        let meta = b"wt metadata";
        let mut ok_header = tar::Header::new_gnu();
        ok_header.set_size(meta.len() as u64);
        ok_header.set_mode(0o644);
        ok_header.set_cksum();
        builder
            .append_data(&mut ok_header, "WiredTiger", &meta[..])
            .unwrap();

        let payload = b"nope";
        let mut evil = tar::Header::new_gnu();
        evil.set_size(payload.len() as u64);
        evil.set_mode(0o644);
        let name = b"../escaped.wt";
        evil.as_old_mut().name[..name.len()].copy_from_slice(name);
        evil.set_cksum();
        builder.append(&evil, &payload[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    let target = dir.join("restored");
    let err = extract_backup_archive(archive.to_str().unwrap(), target.to_str().unwrap())
        .expect_err("extraction must refuse a traversal path");
    assert!(
        format!("{err:?}").contains("unsafe path"),
        "unexpected error: {err:?}"
    );
    assert!(
        !dir.join("escaped.wt").exists(),
        "traversal escaped the target directory"
    );
    std::fs::remove_dir_all(&dir).ok();
}
