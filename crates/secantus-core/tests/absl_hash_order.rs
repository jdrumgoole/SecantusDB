//! The absl port (`secantus_core::absl_hash`) against the real library.
//!
//! The fixtures were recorded by `tools/probes/absl_order/harness.cc` built
//! against abseil `20250512.1-mongo-1`, the fork mongod 8.2 vendors: once for
//! x86-64 (SSE2, group width 16 -- the Linux exemplar's) and once for arm64
//! (NEON, width 8). Each records the process's hash seed (the hash of `""`
//! is the seed plus `0x57`), string hashes across every length class of the
//! hash, and the iteration order of 42 tables of 1 to 600 keys.
//!
//! The harness's tables are its thread's only absl tables, so table `t` drew
//! the per-table seed `s0 + (t + 1) * 0xad53` for one unknown `s0` (the
//! thread-local's address). The test finds the `s0` that gives every order;
//! one `s0` must explain all 42 tables.

use secantus_core::absl_hash::{flat_hash_map_order_with, hash_string};

struct Fixture {
    seed: u64,
    hashes: Vec<(usize, usize, u64)>,
    orders: Vec<Vec<usize>>,
}

fn load(text: &str) -> Fixture {
    let mut seed = None;
    let mut hashes = Vec::new();
    let mut orders = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut words = line.split(' ');
        match words.next() {
            Some("seed_probe") => {
                seed = Some(
                    words
                        .next()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                        .wrapping_sub(0x57),
                );
            }
            Some("hash") => {
                let v: Vec<u64> = words.map(|w| w.parse().unwrap()).collect();
                hashes.push((v[0] as usize, v[1] as usize, v[2]));
            }
            Some("order") => orders.push(words.map(|w| w.parse().unwrap()).collect()),
            other => panic!("unexpected fixture line {other:?}"),
        }
    }
    Fixture {
        seed: seed.expect("a seed_probe line"),
        hashes,
        orders,
    }
}

/// `hash_input` in the harness.
fn hash_input(j: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|t| ((j * 31 + t * 7 + 1) & 0xff) as u8)
        .collect()
}

/// `key` in the harness.
fn key(table: usize, i: usize) -> Vec<u8> {
    let mut s = format!("k{table}.{i}").into_bytes();
    s.extend(std::iter::repeat_n(b'x', (i * 7 + table * 3) % 41));
    s
}

/// Does some thread seed make the port give every recorded order at `width`?
fn check(text: &str, width: usize) -> bool {
    let fx = load(text);
    for &(j, len, want) in &fx.hashes {
        assert_eq!(
            hash_string(&hash_input(j, len), fx.seed),
            want,
            "hash of input {j} length {len}"
        );
    }
    let tables: Vec<Vec<Vec<u8>>> = fx
        .orders
        .iter()
        .enumerate()
        .map(|(t, order)| (0..order.len()).map(|i| key(t, i)).collect())
        .collect();
    let explains = |s0: u16| {
        tables
            .iter()
            .zip(&fx.orders)
            .enumerate()
            .all(|(t, (keys, want))| {
                let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
                let table_seed = s0.wrapping_add((t as u16 + 1).wrapping_mul(0xad53));
                flat_hash_map_order_with(&refs, fx.seed, table_seed, width) == *want
            })
    };
    (0..=u16::MAX).any(explains)
}

#[test]
fn port_matches_absl_on_x86_64_sse2() {
    let fixture = include_str!("fixtures/absl_order_w16.txt");
    assert!(
        check(fixture, 16),
        "no thread seed explains every width-16 table order"
    );
    // The orders are specific enough to tell the widths apart.
    assert!(!check(fixture, 8));
}

#[test]
fn port_matches_absl_on_arm64_neon() {
    let fixture = include_str!("fixtures/absl_order_w8.txt");
    assert!(
        check(fixture, 8),
        "no thread seed explains every width-8 table order"
    );
    assert!(!check(fixture, 16));
}
