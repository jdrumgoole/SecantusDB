//! PostgreSQL 15's configuration parameters, as `pg_settings` lists them.
//!
//! `pg15_settings.tsv` is a dump of a stock PostgreSQL 15 cluster's
//! `pg_settings` (machine-specific paths, the port and the zones left out):
//! `name, SHOW value, setting, unit, category, short_desc, context, vartype,
//! min_val, max_val, boot_val`. Every parameter there is SHOW-able and
//! RESET-able here with PostgreSQL's default, so a client that reads one this
//! server does not obey (`work_mem`, `maintenance_work_mem`, ...) gets
//! PostgreSQL's answer rather than 42704.

/// One `pg_settings` row of PostgreSQL 15.
pub(crate) struct Pg15Setting {
    pub name: &'static str,
    /// What `SHOW` answers for the default (with its unit, `4MB`).
    pub show: &'static str,
    /// `pg_settings.setting` for the default (in `unit`s, `4096`).
    pub setting: &'static str,
    pub unit: &'static str,
    pub category: &'static str,
    pub short_desc: &'static str,
    pub context: &'static str,
    pub vartype: &'static str,
    pub min_val: &'static str,
    pub max_val: &'static str,
    pub boot_val: &'static str,
}

const TSV: &str = include_str!("pg15_settings.tsv");

/// Every row of the dump, parsed once.
pub(crate) fn pg15_settings() -> &'static [Pg15Setting] {
    static ROWS: std::sync::OnceLock<Vec<Pg15Setting>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        TSV.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let f: Vec<&'static str> = l.split('\t').collect();
                let g = |i: usize| f.get(i).copied().unwrap_or("");
                Pg15Setting {
                    name: g(0),
                    show: g(1),
                    setting: g(2),
                    unit: g(3),
                    category: g(4),
                    short_desc: g(5),
                    context: g(6),
                    vartype: g(7),
                    min_val: g(8),
                    max_val: g(9),
                    boot_val: g(10),
                }
            })
            .collect()
    })
}

/// The PostgreSQL 15 row for `name` (settings are keyed as `canonical_setting`
/// spells them; the dump's names compare case-insensitively).
pub(crate) fn pg15_setting(name: &str) -> Option<&'static Pg15Setting> {
    pg15_settings()
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dump_parses_and_carries_work_mem() {
        assert!(pg15_settings().len() > 300);
        assert!(pg15_settings().iter().all(|s| !s.name.is_empty()));
        let w = pg15_setting("work_mem").expect("work_mem");
        assert_eq!((w.show, w.setting, w.unit), ("4MB", "4096", "kB"));
    }
}
