//! PostgreSQL 15's column set and order for the system catalogs this server
//! computes, measured from PostgreSQL 15.19 (`pg15_catalog_order.tsv`:
//! `select attname, format_type(atttypid, null) from pg_attribute where
//! attrelid = '<catalog>'::regclass and attnum > 0 order by attnum`).
//!
//! A catalog's builder sets the columns it knows; `conform` then lays the
//! definition out in PostgreSQL's order, adds the columns it lacks (their
//! value is whatever `fill_catalog_columns` gives, NULL otherwise) and drops
//! a column PostgreSQL 15 does not have (a PostgreSQL 16 one), so a client
//! reading `SELECT *` by position sees what it would see there.

use secantus_pgcatalog::{Column, TableDef};

const ORDER: &str = include_str!("pg15_catalog_order.tsv");

/// PostgreSQL 15's `(column, type)` list for `catalog`, in order.
fn pg15_columns(catalog: &str) -> Vec<(&'static str, &'static str)> {
    ORDER
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let (rel, col, ty) = (it.next()?, it.next()?, it.next()?);
            (rel == catalog).then_some((col, ty))
        })
        .collect()
}

/// Reorder `def`'s columns to PostgreSQL 15's for `catalog`, adding the
/// missing ones and dropping those PostgreSQL 15 lacks. A catalog not in the
/// measured list is left alone.
pub(crate) fn conform(catalog: &str, def: &mut TableDef) {
    let wanted = pg15_columns(catalog);
    if wanted.is_empty() {
        return;
    }
    let mut existing = std::mem::take(&mut def.columns);
    def.columns = wanted
        .into_iter()
        .map(
            |(name, ty)| match existing.iter().position(|c| c.name == name) {
                Some(i) => existing.swap_remove(i),
                None => Column::new(name, ty, false),
            },
        )
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_catalog_has_columns() {
        for rel in [
            "pg_class",
            "pg_attribute",
            "pg_type",
            "pg_proc",
            "pg_trigger",
            "pg_database",
        ] {
            assert!(!pg15_columns(rel).is_empty(), "{rel}");
        }
        assert!(!pg15_columns("pg_database")
            .iter()
            .any(|(c, _)| *c == "daticurules"));
    }
}
