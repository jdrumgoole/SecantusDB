//! What PostgreSQL 15.19's own catalogs hold, as the catalog-read model
//! (RP-225, [`crate::catalog_reads`]) needs it: built once per process from
//! the same dumps the server answers `pg_type`, `pg_proc`, `pg_operator`,
//! `pg_amop`, `pg_amproc`, `pg_opclass` and `pg_aggregate` from, and from
//! the catalog relations' own `pg_class` / `pg_index` rows.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

pub(crate) const BOOL: i64 = 16;
pub(crate) const NAME: i64 = 19;
pub(crate) const INT8: i64 = 20;
pub(crate) const INT2: i64 = 21;
pub(crate) const INT4: i64 = 23;
pub(crate) const TEXT: i64 = 25;
pub(crate) const TID: i64 = 27;
pub(crate) const OID: i64 = 26;
pub(crate) const UNKNOWN: i64 = 705;
pub(crate) const NUMERIC: i64 = 1700;
pub(crate) const RECORD: i64 = 2249;
pub(crate) const ANY: i64 = 2276;
pub(crate) const PG_CATALOG: i64 = 11;
pub(crate) const PUBLIC: i64 = 2200;
pub(crate) const HEAP_AM: i64 = 2;
pub(crate) const BTREE_AM: i64 = 403;

/// The polymorphic pseudo-types (`IsPolymorphicType`).
pub(crate) fn is_polymorphic(t: i64) -> bool {
    matches!(
        t,
        2283 | 2277 | 2776 | 3500 | 3831 | 4537 | 5077 | 5078 | 5079 | 5080 | 4538
    )
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TypeFact {
    pub name: String,
    pub nsp: i64,
    pub len: i64,
    pub typtype: char,
    pub category: char,
    pub preferred: bool,
    pub relid: i64,
    pub elem: i64,
    pub array: i64,
    pub basetype: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProcFact {
    pub oid: i64,
    pub name: String,
    pub nsp: i64,
    pub kind: char,
    pub retset: bool,
    pub variadic: i64,
    pub nargs: usize,
    pub ndefaults: usize,
    pub ret: i64,
    pub args: Vec<i64>,
    pub lang: i64,
    pub volatile: char,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OperFact {
    pub oid: i64,
    pub name: String,
    pub nsp: i64,
    pub kind: char,
    pub canmerge: bool,
    pub canhash: bool,
    pub left: i64,
    pub right: i64,
    pub result: i64,
    pub code: i64,
}

/// One `pg_amop` row.
#[derive(Debug, Clone, Default)]
pub(crate) struct AmopFact {
    pub purpose: char,
    pub family: i64,
    pub method: i64,
    pub strategy: i64,
    pub left: i64,
    pub right: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OpclassFact {
    pub oid: i64,
    pub method: i64,
    pub name: String,
    pub nsp: i64,
    pub family: i64,
    pub intype: i64,
    pub default: bool,
}

/// One of PostgreSQL's own relations.
#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogRel {
    pub oid: i64,
    pub name: String,
    pub nsp: String,
    pub kind: char,
    pub natts: i64,
    pub shared: bool,
    pub relam: i64,
    pub tablespace: i64,
    /// `RelationIdIsInInitFile`: its relation cache entry comes from the
    /// init file, so a backend never builds it.
    pub in_init_file: bool,
    /// Non-empty at `initdb`'s VACUUM and ANALYZE: `relpages` > 0, and a
    /// `pg_statistic` row for every column (but `pg_statistic`'s own).
    pub analyzed: bool,
    pub has_stats: bool,
}

/// One of PostgreSQL's own indexes.
#[derive(Debug, Clone, Default)]
pub(crate) struct CatalogIndex {
    pub oid: i64,
    pub table: i64,
    pub natts: i64,
    pub keys: Vec<i64>,
    pub classes: Vec<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct Builtin {
    pub types: HashMap<i64, TypeFact>,
    pub type_by_name: HashMap<(String, i64), i64>,
    pub procs: HashMap<i64, ProcFact>,
    pub procs_by_name: HashMap<String, Vec<i64>>,
    pub opers: HashMap<i64, OperFact>,
    pub opers_by_name: HashMap<String, Vec<i64>>,
    /// `pg_amop` rows by operator: `(purpose, family)`.
    pub amop_by_opr: HashMap<i64, Vec<(char, i64)>>, /// Each operator's `pg_amop` rows, in `pg_amop_opr_fam_index` order.
    pub amop_rows: HashMap<i64, Vec<AmopFact>>,
    /// `(family, lefttype, righttype, strategy)` -> operator.
    pub amop_strategy: HashMap<(i64, i64, i64, i64), i64>,
    /// `(family, lefttype, righttype)` -> how many support procedures.
    pub amproc_count: HashMap<(i64, i64, i64), i64>,
    pub amproc_num: HashSet<(i64, i64, i64, i64)>,
    pub opclasses: HashMap<i64, OpclassFact>,
    /// Each access method's operator classes in `pg_opclass_am_name_nsp_index`
    /// order (name, then namespace).
    pub opclasses_by_am: HashMap<i64, Vec<i64>>,
    pub casts: HashMap<(i64, i64), (i64, char, char)>,
    /// `pg_aggregate`: `aggfnoid` -> (`aggtransfn`, `aggfinalfn`, `aggtranstype`).
    pub aggregates: HashMap<i64, (i64, i64, i64)>,
    pub relations: HashMap<i64, CatalogRel>,
    pub relation_by_name: HashMap<(String, String), i64>,
    pub indexes: HashMap<i64, CatalogIndex>,
    /// Each catalog's indexes, by oid.
    pub indexes_of: HashMap<i64, Vec<i64>>,
}

/// The relations `RelationSupportsSysCache` names (every catalog and index
/// a system cache reads, `syscache.c`), and the four `RelationIdIsInInitFile`
/// adds: `pg_shseclabel`, `pg_trigger_tgrelid_tgname_index`,
/// `pg_database_datname_index` and `pg_shseclabel_object_index`.
const IN_INIT_FILE: &[i64] = &[
    112, 113, 174, 175, 548, 549, 826, 827, 1213, 1247, 1249, 1255, 1259, 1260, 1261, 1262, 1417,
    1418, 2224, 2228, 2328, 2600, 2601, 2602, 2603, 2605, 2606, 2607, 2610, 2612, 2615, 2616, 2617,
    2618, 2619, 2650, 2651, 2652, 2653, 2654, 2655, 2658, 2659, 2661, 2662, 2663, 2667, 2668, 2669,
    2670, 2672, 2676, 2677, 2679, 2681, 2682, 2684, 2685, 2686, 2687, 2688, 2689, 2690, 2691, 2693,
    2694, 2695, 2696, 2697, 2703, 2704, 2753, 2754, 2755, 3079, 3080, 3085, 3118, 3119, 3164, 3350,
    3351, 3380, 3381, 3429, 3433, 3456, 3466, 3467, 3468, 3501, 3502, 3503, 3541, 3542, 3574, 3575,
    3576, 3600, 3601, 3602, 3603, 3604, 3605, 3606, 3607, 3608, 3609, 3712, 3764, 3766, 3767, 3997,
    5002, 6000, 6001, 6002, 6100, 6102, 6104, 6106, 6110, 6111, 6112, 6113, 6114, 6115, 6117, 6237,
    6238, 6239, 6243, 6246, 6247, // the four extras
    3592, 2701, 2671, 3593,
];

/// The catalogs `initdb` leaves empty in PostgreSQL 15.19, so its VACUUM
/// leaves them `relpages` 0 and its ANALYZE gives them no statistics.
const EMPTY_AT_INITDB: &[&str] = &[
    "pg_default_acl",
    "pg_shdepend",
    "pg_foreign_server",
    "pg_user_mapping",
    "pg_sequence",
    "pg_foreign_data_wrapper",
    "pg_attrdef",
    "pg_inherits",
    "pg_largeobject",
    "pg_trigger",
    "pg_db_role_setting",
    "pg_largeobject_metadata",
    "pg_foreign_table",
    "pg_policy",
    "pg_partitioned_table",
    "pg_statistic_ext",
    "pg_statistic_ext_data",
    "pg_event_trigger",
    "pg_enum",
    "pg_transform",
    "pg_shseclabel",
    "pg_seclabel",
    "pg_replication_origin",
    "pg_subscription",
    "pg_subscription_rel",
    "pg_publication",
    "pg_publication_rel",
    "pg_publication_namespace",
    "pg_parameter_acl",
];

/// A dumped catalog's rows, each a map of column name to its text.
fn rows(name: &str) -> Vec<HashMap<String, String>> {
    let columns = crate::catalog_meta::system_relation_columns(name).unwrap_or_default();
    crate::copy_text::parse(
        crate::system_catalogs::dumped(name),
        false,
        &secantus_pgplan::copy_opts::CopyOptions::default(),
        &[],
        &[],
    )
    .into_iter()
    .map(|raw| {
        columns
            .iter()
            .zip(raw)
            .filter_map(|((c, _, _), v)| Some((c.clone(), v?)))
            .collect()
    })
    .collect()
}

fn int(r: &HashMap<String, String>, k: &str) -> i64 {
    r.get(k).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn flag(r: &HashMap<String, String>, k: &str) -> bool {
    r.get(k).is_some_and(|v| v == "t")
}

fn ch(r: &HashMap<String, String>, k: &str) -> char {
    r.get(k).and_then(|v| v.chars().next()).unwrap_or(' ')
}

fn text(r: &HashMap<String, String>, k: &str) -> String {
    r.get(k).cloned().unwrap_or_default()
}

/// The built-in facts, read once.
pub(crate) fn builtin() -> &'static Builtin {
    static B: OnceLock<Builtin> = OnceLock::new();
    B.get_or_init(load)
}

fn load() -> Builtin {
    let mut b = Builtin::default();
    for r in rows("pg_type") {
        let oid = int(&r, "oid");
        let t = TypeFact {
            name: text(&r, "typname"),
            nsp: int(&r, "typnamespace"),
            len: int(&r, "typlen"),
            typtype: ch(&r, "typtype"),
            category: ch(&r, "typcategory"),
            preferred: flag(&r, "typispreferred"),
            relid: int(&r, "typrelid"),
            elem: int(&r, "typelem"),
            array: int(&r, "typarray"),
            basetype: int(&r, "typbasetype"),
        };
        b.type_by_name.insert((t.name.clone(), t.nsp), oid);
        b.types.insert(oid, t);
    }
    for r in rows("pg_proc") {
        let oid = int(&r, "oid");
        let args: Vec<i64> = text(&r, "proargtypes")
            .split_whitespace()
            .filter_map(|a| a.parse().ok())
            .collect();
        let p = ProcFact {
            oid,
            name: text(&r, "proname"),
            nsp: int(&r, "pronamespace"),
            kind: ch(&r, "prokind"),
            retset: flag(&r, "proretset"),
            variadic: int(&r, "provariadic"),
            nargs: int(&r, "pronargs") as usize,
            ndefaults: int(&r, "pronargdefaults") as usize,
            ret: int(&r, "prorettype"),
            args,
            lang: int(&r, "prolang"),
            volatile: ch(&r, "provolatile"),
        };
        b.procs_by_name.entry(p.name.clone()).or_default().push(oid);
        b.procs.insert(oid, p);
    }
    for r in rows("pg_operator") {
        let oid = int(&r, "oid");
        let o = OperFact {
            oid,
            name: text(&r, "oprname"),
            nsp: int(&r, "oprnamespace"),
            kind: ch(&r, "oprkind"),
            canmerge: flag(&r, "oprcanmerge"),
            canhash: flag(&r, "oprcanhash"),
            left: int(&r, "oprleft"),
            right: int(&r, "oprright"),
            result: int(&r, "oprresult"),
            code: int(&r, "oprcode"),
        };
        b.opers_by_name.entry(o.name.clone()).or_default().push(oid);
        b.opers.insert(oid, o);
    }
    for r in rows("pg_amop") {
        let opr = int(&r, "amopopr");
        let fam = int(&r, "amopfamily");
        b.amop_by_opr
            .entry(opr)
            .or_default()
            .push((ch(&r, "amoppurpose"), fam)); b.amop_rows.entry(opr).or_default().push(AmopFact { purpose: ch(&r, "amoppurpose"), family: fam, method: int(&r, "amopmethod"), strategy: int(&r, "amopstrategy"), left: int(&r, "amoplefttype"), right: int(&r, "amoprighttype") });
        b.amop_strategy.insert(
            (
                fam,
                int(&r, "amoplefttype"),
                int(&r, "amoprighttype"),
                int(&r, "amopstrategy"),
            ),
            opr,
        );
    }
    for v in b.amop_rows.values_mut() {
        v.sort_by_key(|a| (a.purpose, a.family));
    }
    for v in b.amop_by_opr.values_mut() {
        v.sort();
    }
    for r in rows("pg_amproc") {
        let key = (
            int(&r, "amprocfamily"),
            int(&r, "amproclefttype"),
            int(&r, "amprocrighttype"),
        );
        *b.amproc_count.entry(key).or_default() += 1;
        b.amproc_num
            .insert((key.0, key.1, key.2, int(&r, "amprocnum")));
    }
    let mut by_am: HashMap<i64, Vec<(String, i64, i64)>> = HashMap::new();
    for r in rows("pg_opclass") {
        let o = OpclassFact {
            oid: int(&r, "oid"),
            method: int(&r, "opcmethod"),
            name: text(&r, "opcname"),
            nsp: int(&r, "opcnamespace"),
            family: int(&r, "opcfamily"),
            intype: int(&r, "opcintype"),
            default: flag(&r, "opcdefault"),
        };
        by_am
            .entry(o.method)
            .or_default()
            .push((o.name.clone(), o.nsp, o.oid));
        b.opclasses.insert(o.oid, o);
    }
    for (am, mut v) in by_am {
        v.sort();
        b.opclasses_by_am
            .insert(am, v.into_iter().map(|(_, _, o)| o).collect());
    }
    for (_, s, t, func, context, method) in secantus_pgplan::pg_casts::BUILTIN_CASTS {
        b.casts
            .insert((*s, *t), (*func, *context as char, *method as char));
    }
    for r in rows("pg_aggregate") {
        b.aggregates.insert(
            int(&r, "aggfnoid"),
            (
                int(&r, "aggtransfn"),
                int(&r, "aggfinalfn"),
                int(&r, "aggtranstype"),
            ),
        );
    }
    let in_init: HashSet<i64> = IN_INIT_FILE.iter().copied().collect();
    for line in crate::catalog_meta::system_class_lines() {
        let c: Vec<&str> = line.split('\t').collect();
        let [oid, relname, nsp, kind, natts, _hasindex, shared, _reltype, relam, _toast, _persistence, _hasrules, _checks, _filenode, tablespace] =
            c.as_slice()
        else {
            continue;
        };
        let oid: i64 = oid.parse().unwrap_or(0);
        let analyzed = *kind == "r" && *nsp == "pg_catalog" && !EMPTY_AT_INITDB.contains(relname);
        let rel = CatalogRel {
            oid,
            name: (*relname).to_string(),
            nsp: (*nsp).to_string(),
            kind: kind.chars().next().unwrap_or('r'),
            natts: natts.parse().unwrap_or(0),
            shared: *shared == "1",
            relam: relam.parse().unwrap_or(0),
            tablespace: tablespace.parse().unwrap_or(0),
            in_init_file: in_init.contains(&oid),
            analyzed,
            has_stats: analyzed && *relname != "pg_statistic",
        };
        b.relation_by_name
            .insert((rel.nsp.clone(), rel.name.clone()), oid);
        b.relations.insert(oid, rel);
    }
    for line in crate::catalog_meta::system_index_lines() {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 19 {
            continue;
        }
        let ints = |s: &str| -> Vec<i64> {
            s.split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect()
        };
        let ix = CatalogIndex {
            oid: c[0].parse().unwrap_or(0),
            table: c[1].parse().unwrap_or(0),
            natts: c[2].parse().unwrap_or(0),
            keys: ints(c[15]),
            classes: ints(c[17]),
        };
        b.indexes_of.entry(ix.table).or_default().push(ix.oid);
        b.indexes.insert(ix.oid, ix);
    }
    for v in b.indexes_of.values_mut() {
        v.sort_unstable();
    }
    b
}

/// Whether `oid` is one of PostgreSQL's shared relations: their counts are
/// kept under database 0, whichever database read them.
pub(crate) fn is_shared_relation(oid: i64) -> bool {
    oid > 0
        && oid < 16384
        && builtin()
            .relations
            .get(&oid)
            .is_some_and(|r| r.shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_facts_hold_postgresqls_catalogs() {
        let b = builtin();
        assert_eq!(b.types.get(&INT4).map(|t| t.name.as_str()), Some("int4"));
        assert_eq!(b.procs_by_name.get("length").map(Vec::len), Some(8));
        // btree's integer_ops for int4: four support procedures.
        assert_eq!(b.amproc_count.get(&(1976, 23, 23)), Some(&4));
        assert_eq!(b.amproc_count.get(&(1990, 26, 26)), Some(&3));
        assert_eq!(b.amop_by_opr.get(&96).map(Vec::len), Some(5));
        assert_eq!(b.opclasses_by_am.get(&BTREE_AM).map(Vec::len), Some(44));
        let class = &b.relations[&1259];
        assert!(class.in_init_file && class.analyzed);
        assert!(!b.relations[&2678].in_init_file);
        assert_eq!(b.indexes_of.get(&1259).map(Vec::len), Some(3));
        assert!(is_shared_relation(1262) && !is_shared_relation(1259));
    }
}
