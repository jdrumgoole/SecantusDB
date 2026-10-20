//! The ObjectIds the server mints, in mongod's layout: four bytes of time,
//! five bytes of a process value, three bytes of a counter.
//!
//! The process value is this module's own rather than the BSON library's,
//! because mongod lets a client replace it (`features` with `oidReset`,
//! 8.2.11) and `features` reports its first four bytes as `oidMachine`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use bson::oid::ObjectId;

static PROCESS: Mutex<Option<[u8; 5]>> = Mutex::new(None);
static COUNTER: AtomicU32 = AtomicU32::new(0);
static COUNTER_SEEDED: std::sync::Once = std::sync::Once::new();

fn process_value() -> [u8; 5] {
    let mut guard = PROCESS.lock().unwrap_or_else(|e| e.into_inner());
    *guard.get_or_insert_with(rand::random)
}

/// A new ObjectId: now, the process value, the next count.
pub fn new() -> ObjectId {
    COUNTER_SEEDED
        .call_once(|| COUNTER.store(rand::random::<u32>() & 0x00ff_ffff, Ordering::Relaxed));
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed) & 0x00ff_ffff;
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(&secs.to_be_bytes());
    bytes[4..9].copy_from_slice(&process_value());
    bytes[9..].copy_from_slice(&count.to_be_bytes()[1..]);
    ObjectId::from_bytes(bytes)
}

/// The first four bytes of the process value, read little-endian: what
/// mongod reports as `oidMachine` (measured 8.2.11).
pub fn machine() -> u32 {
    let p = process_value();
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

/// Replace the process value, as `oidReset` does: the machine value before
/// and after.
pub fn reset() -> (u32, u32) {
    let old = machine();
    *PROCESS.lock().unwrap_or_else(|e| e.into_inner()) = Some(rand::random());
    (old, machine())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_carry_the_process_value_until_it_is_reset() {
        let a = new().bytes();
        let b = new().bytes();
        assert_eq!(a[4..9], b[4..9]);
        assert_ne!(a[9..], b[9..]);
        assert_eq!(u32::from_le_bytes([a[4], a[5], a[6], a[7]]), machine());
        let (old, now) = reset();
        assert_eq!(old, u32::from_le_bytes([a[4], a[5], a[6], a[7]]));
        assert_eq!(now, machine());
        assert_eq!(new().bytes()[4..8], now.to_le_bytes());
    }
}
