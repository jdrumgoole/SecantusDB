//! Spike 1 — BSON byte-fidelity probe.
//!
//! Reads a stream of concatenated BSON documents from stdin (each
//! self-delimited by its own leading little-endian int32 length, exactly as
//! on the wire / on disk), decodes each one through the `bson` crate, and
//! re-encodes it. Two round-trip strengths are exercised per document:
//!
//!   1. `RawDocument` borrow -> owned `Vec<u8>` re-encode (the zero-copy path
//!      the production core would actually use at the byte seam), and
//!   2. full decode to `bson::Document` -> re-encode (the strict, ordered,
//!      typed round-trip).
//!
//! The re-encoded bytes (path 2) are written back to stdout, concatenated in
//! the same framing. The Python harness (`spike_bson_harness.py`) feeds a
//! corpus built with pymongo and asserts the output is byte-identical to the
//! input. Any divergence is a bson-crate <-> pymongo fidelity gap — the whole
//! point of the spike.
//!
//! Exit code is non-zero if any document fails to decode or if the two paths
//! disagree, so the harness gets a hard signal as well as the byte diff.

use std::io::{Read, Write};

use bson::{Document, RawDocument};

fn read_i32_le(buf: &[u8], at: usize) -> Option<i32> {
    let end = at.checked_add(4)?;
    let slice = buf.get(at..end)?;
    Some(i32::from_le_bytes(slice.try_into().ok()?))
}

fn main() {
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("read stdin: {e}");
        std::process::exit(2);
    }

    let mut out = Vec::with_capacity(input.len());
    let mut offset = 0usize;
    let mut doc_index = 0usize;
    let mut failures = 0usize;

    while offset < input.len() {
        let declared = match read_i32_le(&input, offset) {
            Some(n) if n >= 5 => n as usize,
            other => {
                eprintln!("doc {doc_index}: bad/length-prefix {other:?} at offset {offset}");
                std::process::exit(2);
            }
        };
        let end = offset + declared;
        if end > input.len() {
            eprintln!("doc {doc_index}: declared len {declared} runs past buffer");
            std::process::exit(2);
        }
        let raw_bytes = &input[offset..end];

        // Path 1: zero-copy RawDocument borrow, then re-encode by walking to an
        // owned Document. This is the API the core would lean on at the seam.
        let raw = match RawDocument::from_bytes(raw_bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("doc {doc_index}: RawDocument parse failed: {e}");
                failures += 1;
                offset = end;
                doc_index += 1;
                continue;
            }
        };
        let via_raw: Document = match raw.try_into() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("doc {doc_index}: RawDocument -> Document failed: {e}");
                failures += 1;
                offset = end;
                doc_index += 1;
                continue;
            }
        };

        // Path 2: strict typed decode -> re-encode.
        let doc: Document = match bson::from_slice(raw_bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("doc {doc_index}: typed decode failed: {e}");
                failures += 1;
                offset = end;
                doc_index += 1;
                continue;
            }
        };

        let mut reencoded = Vec::new();
        if let Err(e) = doc.to_writer(&mut reencoded) {
            eprintln!("doc {doc_index}: re-encode failed: {e}");
            failures += 1;
            offset = end;
            doc_index += 1;
            continue;
        }

        let mut reencoded_raw = Vec::new();
        if via_raw.to_writer(&mut reencoded_raw).is_ok() && reencoded_raw != reencoded {
            eprintln!("doc {doc_index}: RawDocument path and typed path disagree");
            failures += 1;
        }

        out.extend_from_slice(&reencoded);
        offset = end;
        doc_index += 1;
    }

    if let Err(e) = std::io::stdout().write_all(&out) {
        eprintln!("write stdout: {e}");
        std::process::exit(2);
    }
    eprintln!("roundtrip: {doc_index} docs, {failures} internal failures");
    if failures > 0 {
        std::process::exit(1);
    }
}
