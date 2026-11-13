//! Planner tables published per catalog version but derived only when a
//! statement first looks one up (RP-911).
//!
//! PostgreSQL fills its catalog caches on demand (`SearchSysCache`): a
//! statement that never names a composite type builds no composite entry.
//! The executor republishes the planner's tables after every change of the
//! catalog, and deriving each one from every relation made the statement
//! after any DDL grow with the schema. A [`Deferred`] holds what the
//! derivation reads, captured when the table is published (shared catalog
//! documents, cheap to hold), and runs the derivation at the first lookup.
//! Since the inputs are taken at publication, the table derived later is
//! the one an eager publication would have built.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// A value, or the pure derivation that makes it on first use.
pub struct Deferred<T> {
    value: OnceCell<T>,
    build: Cell<Option<Box<dyn FnOnce() -> T>>>,
}

impl<T> Deferred<T> {
    /// A value already made.
    pub fn ready(value: T) -> Self {
        Self {
            value: OnceCell::from(value),
            build: Cell::new(None),
        }
    }

    /// A value `build` makes when it is first asked for. `build` must read
    /// only what it captured: running later, it must not see a catalog
    /// newer than the one the table was published for.
    pub fn new(build: impl FnOnce() -> T + 'static) -> Self {
        Self {
            value: OnceCell::new(),
            build: Cell::new(Some(Box::new(build))),
        }
    }

    /// The value, made now if it has not been.
    pub fn get(&self) -> &T {
        self.value.get_or_init(|| {
            let build = self
                .build
                .take()
                .expect("a Deferred holds either its value or its derivation");
            build()
        })
    }
}

/// A thread's published table: replaced by each publication, read through
/// [`Slot::current`] so that no borrow is held while a derivation runs.
pub struct Slot<T: 'static>(RefCell<Rc<Deferred<T>>>);

impl<T: Default + 'static> Default for Slot<T> {
    fn default() -> Self {
        Self(RefCell::new(Rc::new(Deferred::ready(T::default()))))
    }
}

impl<T: 'static> Slot<T> {
    /// A slot holding `value`, for a table with no `Default`.
    pub fn new(value: T) -> Self {
        Self(RefCell::new(Rc::new(Deferred::ready(value))))
    }

    /// Publish a value already made.
    pub fn set(&self, value: T) {
        *self.0.borrow_mut() = Rc::new(Deferred::ready(value));
    }

    /// Publish a derivation, run at the first lookup.
    pub fn defer(&self, build: impl FnOnce() -> T + 'static) {
        *self.0.borrow_mut() = Rc::new(Deferred::new(build));
    }

    /// The published table, for one lookup.
    pub fn current(&self) -> Rc<Deferred<T>> {
        Rc::clone(&self.0.borrow())
    }
}

/// `f` over the table published in `slot`.
pub fn read<T: 'static, R>(
    slot: &'static std::thread::LocalKey<Slot<T>>,
    f: impl FnOnce(&T) -> R,
) -> R {
    let table = slot.with(Slot::current);
    f(table.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static TABLE: Slot<Vec<i64>> = Slot::default();
    }

    #[test]
    fn a_deferred_table_is_derived_once_at_its_first_lookup() {
        let runs = Rc::new(Cell::new(0));
        let counter = Rc::clone(&runs);
        TABLE.with(|t| {
            t.defer(move || {
                counter.set(counter.get() + 1);
                vec![1, 2]
            })
        });
        assert_eq!(runs.get(), 0, "publishing derives nothing");
        assert_eq!(read(&TABLE, |v| v.len()), 2);
        assert_eq!(read(&TABLE, |v| v[1]), 2);
        assert_eq!(runs.get(), 1, "derived once");
    }

    #[test]
    fn a_derivation_sees_the_inputs_it_was_published_with() {
        let mut input = vec![7];
        let captured = input.clone();
        TABLE.with(|t| t.defer(move || captured));
        input.push(8);
        assert_eq!(read(&TABLE, Clone::clone), vec![7]);
        TABLE.with(|t| t.set(input));
        assert_eq!(read(&TABLE, Clone::clone), vec![7, 8]);
    }

    #[test]
    fn a_lookup_during_a_derivation_reads_another_slot() {
        thread_local! {
            static OTHER: Slot<i64> = Slot::default();
        }
        OTHER.with(|o| o.set(5));
        TABLE.with(|t| t.defer(|| vec![read(&OTHER, |v| *v)]));
        assert_eq!(read(&TABLE, Clone::clone), vec![5]);
    }
}
