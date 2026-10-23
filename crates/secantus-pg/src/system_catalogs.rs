//! PostgreSQL catalog tables RP-070 found missing, each with its columns as
//! PostgreSQL 15.19 declares them (`catalog_meta::system_relation_columns`)
//! and the rows PostgreSQL would hold for what this server keeps:
//!
//! - the built-in objects, dumped from a fresh 15.19 database
//!   (`pg15_catalog_rows.txt`, `tools/probes/pg15_system_catalog_dump.py`),
//!   with the user aggregates after them in `pg_aggregate`;
//! - the server's own state: database settings (`pg_db_role_setting`), large
//!   objects (`pg_largeobject`), comments on databases and roles
//!   (`pg_shdescription`);
//! - nothing, for a catalog whose every writer this server refuses (each is
//!   named below).

use super::*;
use std::collections::HashMap;

const ROWS: &str = include_str!("pg15_catalog_rows.txt");

/// The built-in rows, by relation.
const STATIC: &[&str] = &[
    "information_schema.sql_features",
    "information_schema.sql_implementation_info",
    "information_schema.sql_parts",
    "information_schema.sql_sizing",
    "pg_aggregate",
    "pg_amop",
    "pg_amproc",
    "pg_conversion",
    "pg_init_privs",
    "pg_opfamily",
];

/// Catalogs that stay empty because the statement or function that writes
/// them is refused here.
const EMPTY: &[(&str, &str)] = &[
    ("pg_transform", "CREATE TRANSFORM"),
    ("pg_seclabel", "SECURITY LABEL: no label provider is loaded"),
    (
        "pg_shseclabel",
        "SECURITY LABEL: no label provider is loaded",
    ),
    ("pg_replication_origin", "pg_replication_origin_create()"),
    ("pg_subscription", "CREATE SUBSCRIPTION"),
    ("pg_subscription_rel", "CREATE SUBSCRIPTION"),
    // The views over state nothing here can make.
    ("pg_replication_slots", "pg_create_*_replication_slot()"),
    (
        "pg_stat_replication_slots",
        "pg_create_*_replication_slot()",
    ),
    (
        "pg_replication_origin_status",
        "pg_replication_origin_create()",
    ),
    ("pg_stat_subscription", "CREATE SUBSCRIPTION"),
    ("pg_stat_subscription_stats", "CREATE SUBSCRIPTION"),
    (
        "pg_stat_replication",
        "the replication protocol: no WAL sender runs",
    ),
    (
        "pg_stat_wal_receiver",
        "standby mode: this server is a primary",
    ),
];

/// Catalogs whose rows are the server's own state.
const STATEFUL: &[&str] = &[
    "pg_db_role_setting",
    "pg_largeobject",
    "pg_shdescription",
    "pg_available_extensions",
    "pg_available_extension_versions",
    "pg_stat_gssapi",
    "pg_stat_database_conflicts",
    "pg_stat_recovery_prefetch",
    "pg_shdepend",
    "pg_hba_file_rules",
    "pg_ident_file_mappings",
    "pg_file_settings",
    "pg_parameter_acl",
    "pg_config",
    "pg_statistic",
    "pg_statistic_ext_data",
    "pg_stats_ext",
    "pg_stats_ext_exprs",
];

/// The extensions `CREATE EXTENSION` installs here, with their control files'
/// `superuser`, `trusted`, `relocatable` and `schema` (PostgreSQL 15.19's,
/// and PostGIS's own); each has the one version this server carries.
const EXTENSIONS: &[(&str, bool, bool, bool, Option<&str>)] = &[
    ("btree_gin", true, true, true, None),
    ("btree_gist", true, true, true, None),
    ("citext", true, true, true, None),
    ("hstore", true, true, true, None),
    ("pg_trgm", true, true, true, None),
    ("pgcrypto", true, true, true, None),
    ("plpgsql", true, true, false, Some("pg_catalog")),
    ("postgis", true, false, false, None),
];

/// PostgreSQL's `LOBLKSIZE`: a large object is stored in pages this long.
const LOBLKSIZE: usize = 2048;

/// The comments `initdb` leaves on the three databases (`pg_shdescription`).
const DATABASE_COMMENTS: &[(&str, &str)] = &[
    ("template1", "default template for new databases"),
    ("template0", "unmodifiable empty database"),
    ("postgres", "default administrative connection database"),
];

/// `pg_shdescription.classoid` for each kind of shared object.
const DATABASE_CLASS: i64 = 1262;
const ROLE_CLASS: i64 = 1260;

pub(crate) const SHARED_COMMENT_COLLECTION: &str = "__sql_shared_comments__";

pub(crate) fn is_system_catalog(name: &str) -> bool {
    STATIC.contains(&name) || STATEFUL.contains(&name) || EMPTY.iter().any(|(n, _)| *n == name)
}

/// The catalog's columns, as PostgreSQL declares them.
pub(crate) fn def(name: &str) -> Option<TableDef> {
    if !is_system_catalog(name) {
        return None;
    }
    let columns = catalog_meta::system_relation_columns(name)?;
    Some(TableDef::new(
        name,
        columns
            .iter()
            .map(|(col, oid, _)| Column::new(col, &type_name(*oid), false))
            .collect(),
    ))
}

/// A system column's type, by oid.
fn type_name(oid: i64) -> String {
    match oid {
        18 => secantus_pgplan::QUOTED_CHAR.to_string(),
        // `pg_statistic.stavalues*`: an array of the column's own type.
        2277 => "anyarray".to_string(),
        _ => secantus_pgplan::pgtypes::type_name_of_oid(oid).unwrap_or_else(|| "text".to_string()),
    }
}

/// The dumped COPY text of one relation.
/// The built-in conversions as dumped: `(conforencoding, contoencoding,
/// conproc)`, each conversion function's pairs.
pub(crate) fn builtin_conversion_pairs() -> Vec<(i64, i64, i64)> {
    dumped("pg_conversion")
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some((
                f.get(4)?.parse().ok()?,
                f.get(5)?.parse().ok()?,
                f.get(6)?.parse().ok()?,
            ))
        })
        .collect()
}

/// The built-in operator classes as dumped: `(opcmethod, opcname,
/// opcintype, opcdefault)`.
pub(crate) fn builtin_opclasses() -> Vec<secantus_pgplan::misc_alter::OpClass> {
    dumped("pg_opclass")
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Some(secantus_pgplan::misc_alter::OpClass {
                method: f.get(1)?.parse().ok()?,
                name: (*f.get(2)?).to_string(),
                intype: f.get(6)?.parse().ok()?,
                default: *f.get(7)? == "t",
            })
        })
        .collect()
}

fn dumped(name: &str) -> &'static str {
    let key = if name.contains('.') {
        name.to_string()
    } else {
        format!("pg_catalog.{name}")
    };
    let header = format!("\\relation {key}\n");
    let Some(start) = ROWS.find(&header).map(|i| i + header.len()) else {
        return "";
    };
    let end = ROWS[start..]
        .find("\\relation ")
        .map_or(ROWS.len(), |i| start + i);
    &ROWS[start..end]
}

/// A built-in object's ACL as `initdb` leaves it (`pg_init_privs`), by
/// `(classoid, objoid, objsubid)`: `pg_subscription`'s columns are
/// `{=r/postgres}`. PostgreSQL's current ACL of a system object is this
/// until someone GRANTs on it, which pg_dump compares against.
pub(crate) fn initial_acl(classoid: i64, objoid: i64, objsubid: i32) -> Option<Bson> {
    static MAP: std::sync::OnceLock<HashMap<(i64, i64, i32), Vec<String>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        copy_text::parse(
            dumped("pg_init_privs"),
            false,
            &secantus_pgplan::copy_opts::CopyOptions::default(),
            &[],
            &[],
        )
        .into_iter()
        .filter_map(|r| {
            let field = |i: usize| r.get(i).cloned().flatten();
            let acl = field(4)?;
            let items = acl
                .trim_start_matches('{')
                .trim_end_matches('}')
                .split(',')
                .map(str::to_string)
                .collect();
            Some((
                (
                    field(1)?.parse().ok()?,
                    field(0)?.parse().ok()?,
                    field(2)?.parse().ok()?,
                ),
                items,
            ))
        })
        .collect()
    })
    .get(&(classoid, objoid, objsubid))
    .map(|items| Bson::Array(items.iter().cloned().map(Bson::String).collect()))
}

impl PgHandler {
    pub(crate) fn system_catalog_rows(&self, name: &str, def: &TableDef) -> Vec<Document> {
        if STATIC.contains(&name) {
            let mut rows = self.static_rows(name, def);
            if name == "pg_aggregate" {
                rows.extend(self.user_aggregate_rows(def));
            }
            if name == "pg_conversion" {
                rows.extend(self.user_conversion_rows(def));
            }
            return rows;
        }
        match name {
            "pg_db_role_setting" => self.db_role_setting_rows(def),
            "pg_largeobject" => self.largeobject_rows(def),
            "pg_shdescription" => self.shdescription_rows(def),
            "pg_available_extensions" | "pg_available_extension_versions" => {
                self.available_extension_rows(name, def)
            }
            // Each client backend's GSSAPI state: none authenticates or
            // encrypts with GSS here.
            "pg_stat_gssapi" => {
                let f = |n: &str| def.field_of(n).expect("column");
                let Some(activity) = Self::virtual_table("pg_stat_activity") else {
                    return Vec::new();
                };
                let pid = activity.field_of("pid");
                let kind = activity.field_of("backend_type");
                self.virtual_rows("pg_stat_activity", &Document::new())
                    .unwrap_or_default()
                    .iter()
                    .filter(|r| {
                        kind.as_ref().and_then(|k| r.get_str(k).ok()) == Some("client backend")
                    })
                    .map(|r| {
                        let mut d = Document::new();
                        d.insert(
                            f("pid"),
                            pid.as_ref()
                                .and_then(|p| r.get(p).cloned())
                                .unwrap_or(Bson::Null),
                        );
                        d.insert(f("gss_authenticated"), false);
                        d.insert(f("principal"), Bson::Null);
                        d.insert(f("encrypted"), false);
                        d
                    })
                    .collect()
            }
            "pg_shdepend" => self.shdepend_rows(def),
            "pg_hba_file_rules" => self.hba_file_rule_rows(def),
            "pg_ident_file_mappings" => self.ident_file_mapping_rows(def),
            "pg_config" => self.pg_config_rows(def),
            "pg_statistic" => self.pg_statistic_rows(def),
            "pg_statistic_ext_data" => self.statistic_ext_data_rows(def),
            "pg_stats_ext" => self.stats_ext_rows(def),
            "pg_stats_ext_exprs" => self.stats_ext_exprs_rows(def),
            "pg_file_settings" => self.file_setting_rows(def),
            "pg_parameter_acl" => self.parameter_acl_rows(def),
            // Recovery conflicts arise only on a standby: every database's
            // counts are zero on a primary.
            "pg_stat_database_conflicts" => {
                let f = |n: &str| def.field_of(n).expect("column");
                let mut dbs = self.databases.all(&self.storage).unwrap_or_default();
                dbs.sort_by_key(|d| (d.oid != 5, d.oid));
                dbs.into_iter()
                    .map(|db| {
                        let mut d = Document::new();
                        d.insert(f("datid"), Bson::Int64(db.oid));
                        d.insert(f("datname"), db.name);
                        for c in [
                            "confl_tablespace",
                            "confl_lock",
                            "confl_snapshot",
                            "confl_bufferpin",
                            "confl_deadlock",
                        ] {
                            d.insert(f(c), Bson::Int64(0));
                        }
                        d
                    })
                    .collect()
            }
            // A primary's prefetcher never ran: its counters as initialised
            // at startup.
            "pg_stat_recovery_prefetch" => {
                let f = |n: &str| def.field_of(n).expect("column");
                let mut d = Document::new();
                d.insert(
                    f("stats_reset"),
                    Bson::DateTime(secantus_pgplan::scalar::postmaster_start()),
                );
                for c in [
                    "prefetch",
                    "hit",
                    "skip_init",
                    "skip_new",
                    "skip_fpw",
                    "skip_rep",
                ] {
                    d.insert(f(c), Bson::Int64(0));
                }
                d.insert(f("wal_distance"), Bson::Int32(0));
                d.insert(f("block_distance"), Bson::Int32(1));
                d.insert(f("io_depth"), Bson::Int32(0));
                vec![d]
            }
            _ => Vec::new(),
        }
    }

    /// A dumped relation's rows, each column converted by the type `def`
    /// gives it (found by name: a catalog the server declares itself may
    /// order its columns differently).
    pub(crate) fn static_rows(&self, name: &str, def: &TableDef) -> Vec<Document> {
        let columns = catalog_meta::system_relation_columns(name).unwrap_or_default();
        let tz = self.session_timezone();
        let rows = copy_text::parse(
            dumped(name),
            false,
            &secantus_pgplan::copy_opts::CopyOptions::default(),
            &[],
            &[],
        );
        rows.into_iter()
            .map(|raw| {
                let mut d = Document::new();
                for ((col, oid, _), value) in columns.iter().zip(raw) {
                    let Some(c) = def.columns.iter().find(|c| c.name == *col) else {
                        continue;
                    };
                    let value = match value {
                        None => Bson::Null,
                        Some(text) => match secantus_pgplan::pgtypes::name_of_oid(*oid) {
                            // A reg* column was dumped as its oid.
                            Some(t) if t.starts_with("reg") => text
                                .parse()
                                .map_or(Bson::Null, |o| secantus_pgplan::regobj::value(t, o)),
                            // An expression, dumped as its SQL text.
                            _ if c.pg_type == "pg_node_tree" => Bson::String(text),
                            _ => {
                                let wire = wire_type(&c.pg_type);
                                copy_field(&text, &c.pg_type, &wire, &tz).unwrap_or(Bson::Null)
                            }
                        },
                    };
                    if let Some(f) = def.field_of(col) {
                        d.insert(f, value);
                    }
                }
                d
            })
            .collect()
    }

    /// `pg_aggregate` for `CREATE AGGREGATE`'s aggregates, as `DefineAggregate`
    /// fills the row an aggregate with only SFUNC, STYPE, FINALFUNC and
    /// INITCOND gets.
    fn user_aggregate_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n);
        let none = || self.regproc_bson("");
        self.user_aggregates()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let key = Self::aggregate_key(&a.name, &a.arg_types);
                let mut d = Document::new();
                let mut put = |k: &str, v: Bson| {
                    if let Some(field) = f(k) {
                        d.insert(field, v);
                    }
                };
                put(
                    "aggfnoid",
                    secantus_pgplan::regobj::value(
                        "regproc",
                        Self::index_oid(&format!("agg:{key}")),
                    ),
                );
                put("aggkind", Bson::String("n".into()));
                put("aggnumdirectargs", Bson::Int32(0));
                put("aggtransfn", self.regproc_bson(&a.sfunc));
                put(
                    "aggfinalfn",
                    a.finalfunc
                        .as_deref()
                        .map_or_else(none, |fun| self.regproc_bson(fun)),
                );
                for k in [
                    "aggcombinefn",
                    "aggserialfn",
                    "aggdeserialfn",
                    "aggmtransfn",
                    "aggminvtransfn",
                    "aggmfinalfn",
                ] {
                    put(k, none());
                }
                put("aggfinalextra", Bson::Boolean(false));
                put("aggmfinalextra", Bson::Boolean(false));
                put("aggfinalmodify", Bson::String("r".into()));
                put("aggmfinalmodify", Bson::String("r".into()));
                put("aggsortop", Bson::Int64(0));
                put(
                    "aggtranstype",
                    Bson::Int64(
                        secantus_pgplan::pgtypes::oid_of_name(&a.stype)
                            .or_else(|| self.type_oid_by_name(&a.stype))
                            .unwrap_or(0),
                    ),
                );
                put("aggtransspace", Bson::Int32(0));
                put("aggmtranstype", Bson::Int64(0));
                put("aggmtransspace", Bson::Int32(0));
                put("agginitval", a.initcond.map_or(Bson::Null, Bson::String));
                put("aggminitval", Bson::Null);
                d
            })
            .collect()
    }

    /// `ALTER DATABASE ... SET`: one row per database with settings, the
    /// role 0 (every role), each setting as `name=value`.
    fn db_role_setting_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out = Vec::new();
        // `ALTER ROLE ... SET`'s rows, one per (database, role).
        let roles = self.roles().unwrap_or_default();
        let dbs = self.databases.all(&self.storage).unwrap_or_default();
        let mut grouped: std::collections::BTreeMap<(i64, i64), Vec<String>> =
            std::collections::BTreeMap::new();
        for (role, database, name, value) in self.role_setting_docs() {
            let role_oid = if role.is_empty() {
                0
            } else {
                match roles.iter().find(|r| r.name == role) {
                    Some(r) => r.oid,
                    None => continue,
                }
            };
            let db_oid = if database.is_empty() {
                0
            } else {
                match dbs.iter().find(|d| d.name == database) {
                    Some(d) => d.oid,
                    None => continue,
                }
            };
            grouped
                .entry((db_oid, role_oid))
                .or_default()
                .push(format!("{name}={value}"));
        }
        for ((db_oid, role_oid), config) in grouped {
            let mut d = Document::new();
            d.insert(f("setdatabase"), Bson::Int64(db_oid));
            d.insert(f("setrole"), Bson::Int64(role_oid));
            d.insert(
                f("setconfig"),
                Bson::Array(config.into_iter().map(Bson::String).collect()),
            );
            out.push(d);
        }
        for db in self.databases.all(&self.storage).unwrap_or_default() {
            let settings = self.db_setting_defaults_of(&db.name);
            if settings.is_empty() {
                continue;
            }
            let mut d = Document::new();
            d.insert(f("setdatabase"), Bson::Int64(db.oid));
            d.insert(f("setrole"), Bson::Int64(0));
            d.insert(
                f("setconfig"),
                Bson::Array(
                    settings
                        .into_iter()
                        .map(|(k, v)| Bson::String(format!("{k}={v}")))
                        .collect(),
                ),
            );
            out.push(d);
        }
        out
    }

    /// `pg_shdepend`: who owns what (`o`) and who an ACL names (`a`),
    /// never the bootstrap superuser (`isSharedObjectPinned`). Each
    /// database's owner, and this database's relations, functions and types.
    /// Another database's objects are not read here.
    fn shdepend_rows(&self, def: &TableDef) -> Vec<Document> {
        const PG_AUTHID: i64 = 1260;
        let f = |n: &str| def.field_of(n).expect("column");
        let roles = self.roles().unwrap_or_default();
        let role_oid = |name: &str| roles.iter().find(|r| r.name == name).map(|r| r.oid);
        let databases = self.databases.all(&self.storage).unwrap_or_default();
        let here = databases
            .iter()
            .find(|d| d.name == self.db())
            .map_or(0, |d| d.oid);
        let mut out = Vec::new();
        let mut push = |dbid: i64, classid: i64, objid: i64, role: i64, deptype: &str| {
            if role == RoleInfo::BOOTSTRAP_OID {
                return;
            }
            let mut d = Document::new();
            d.insert(f("dbid"), Bson::Int64(dbid));
            d.insert(f("classid"), Bson::Int64(classid));
            d.insert(f("objid"), Bson::Int64(objid));
            d.insert(f("objsubid"), Bson::Int32(0));
            d.insert(f("refclassid"), Bson::Int64(PG_AUTHID));
            d.insert(f("refobjid"), Bson::Int64(role));
            d.insert(f("deptype"), deptype);
            out.push(d);
        };
        for db in &databases {
            if let Some(o) = (!db.owner.is_empty())
                .then(|| role_oid(&db.owner))
                .flatten()
            {
                push(0, 1262, db.oid, o, "o");
            }
        }
        // A parameter's grants name their grantees and grantors.
        let mut seen = Vec::new();
        for (oid, _, role) in self.parameter_acl_roles() {
            if let Some(o) = role_oid(&role) {
                if !seen.contains(&(oid, o)) {
                    seen.push((oid, o));
                    push(0, parameter_acl::PG_PARAMETER_ACL, oid, o, "a");
                }
            }
        }
        // pg_shdepend is shared: every database's objects, each read as a
        // connection to that database reads its catalogs.
        for db in &databases {
            let objects = if db.oid == here {
                self.owned_objects(&role_oid)
            } else {
                let other = PgHandler::new(Arc::clone(&self.storage), Arc::clone(&self.databases));
                let _ = other.db.set(db.name.clone());
                other.owned_objects(&role_oid)
            };
            for (classid, oid, owner, acl) in objects {
                push(db.oid, classid, oid, owner, "o");
                for o in acl {
                    push(db.oid, classid, oid, o, "a");
                }
            }
        }
        if databases.iter().any(|d| d.oid != here) {
            // The other databases' reads published their catalogs to this
            // thread's planner; put this connection's back.
            self.install_user_types();
        }
        out
    }

    /// `(class oid, object oid, owner, other roles its ACL names)` for every
    /// user object this database's pg_class, pg_proc and pg_type list.
    fn owned_objects(
        &self,
        role_oid: &dyn Fn(&str) -> Option<i64>,
    ) -> Vec<(i64, i64, i64, Vec<i64>)> {
        let mut out = Vec::new();
        // (catalog, its class oid, the owner and ACL columns).
        for (catalog, classid, owner_col, acl_col) in [
            ("pg_namespace", 2615, "nspowner", "nspacl"),
            ("pg_class", 1259, "relowner", "relacl"),
            ("pg_proc", 1255, "proowner", "proacl"),
            ("pg_type", 1247, "typowner", "typacl"),
        ] {
            let Some(cdef) = Self::virtual_table(catalog) else {
                continue;
            };
            let (Some(oid_f), Some(owner_f)) = (cdef.field_of("oid"), cdef.field_of(owner_col))
            else {
                continue;
            };
            let acl_f = cdef.field_of(acl_col);
            // The standalone composite types' oids: a row type with a
            // relation of its own kind `c`.
            let standalone: HashSet<i64> = if catalog == "pg_type" {
                self.virtual_rows("pg_class", &Document::new())
                    .unwrap_or_default()
                    .iter()
                    .filter(|r| {
                        Self::virtual_table("pg_class")
                            .and_then(|d| d.field_of("relkind"))
                            .is_some_and(|k| r.get_str(&k) == Ok("c"))
                    })
                    .filter_map(|r| {
                        Self::virtual_table("pg_class")
                            .and_then(|d| d.field_of("reltype"))
                            .and_then(|k| r.get(&k).and_then(crate::bson_i64))
                    })
                    .collect()
            } else {
                HashSet::new()
            };
            for r in self
                .virtual_rows(catalog, &Document::new())
                .unwrap_or_default()
            {
                let int = |k: &str| match r.get(k) {
                    Some(Bson::Int64(v)) => Some(*v),
                    Some(Bson::Int32(v)) => Some(i64::from(*v)),
                    _ => None,
                };
                let (Some(oid), Some(owner)) = (int(&oid_f), int(&owner_f)) else {
                    continue;
                };
                if oid < 16384 {
                    continue;
                }
                // What `recordDependencyOnOwner` is never called for: an
                // index and a composite type's relation (their owner is
                // the table's, the type's), a relation's row type and an
                // array type (`isDependentType`).
                let text = |k: &str| {
                    cdef.field_of(k)
                        .and_then(|f| r.get_str(&f).ok().map(str::to_string))
                        .unwrap_or_default()
                };
                let dependent = match catalog {
                    "pg_class" => matches!(text("relkind").as_str(), "i" | "I" | "c"),
                    "pg_type" => {
                        let field_int =
                            |k: &str| cdef.field_of(k).and_then(|f| int(&f)).unwrap_or(0);
                        let array = field_int("typelem") != 0 && text("typname").starts_with('_');
                        array || (field_int("typrelid") != 0 && !standalone.contains(&oid))
                    }
                    _ => false,
                };
                if dependent {
                    continue;
                }
                let mut named: Vec<i64> = Vec::new();
                if let Some(Bson::Array(items)) = acl_f.as_ref().and_then(|a| r.get(a)) {
                    for item in items.iter().filter_map(Bson::as_str) {
                        let Some((grantee, rest)) = item.split_once('=') else {
                            continue;
                        };
                        let grantor = rest.rsplit_once('/').map_or("", |(_, g)| g);
                        for name in [grantee, grantor] {
                            if let Some(o) = (!name.is_empty()).then(|| role_oid(name)).flatten() {
                                if o != owner && !named.contains(&o) {
                                    named.push(o);
                                }
                            }
                        }
                    }
                }
                out.push((classid, oid, owner, named));
            }
        }
        out
    }

    /// `pg_available_extensions` / `pg_available_extension_versions`: the
    /// extensions this server can install, each with its installed version.
    fn available_extension_rows(&self, name: &str, def: &TableDef) -> Vec<Document> {
        let installed = self.extensions().unwrap_or_default();
        let f = |n: &str| def.field_of(n);
        EXTENSIONS
            .iter()
            .map(|(ext, superuser, trusted, relocatable, schema)| {
                let version = if *ext == "plpgsql" {
                    "1.0"
                } else {
                    Self::available_extension(ext).map_or("", |(v, _, _)| v)
                };
                let installed_version = installed
                    .iter()
                    .find(|(n, _, _)| n == ext)
                    .map(|_| version.to_string());
                let mut d = Document::new();
                let mut put = |k: &str, v: Bson| {
                    if let Some(field) = f(k) {
                        d.insert(field, v);
                    }
                };
                put("name", Bson::String((*ext).to_string()));
                put(
                    "comment",
                    catalog_fill::extension_description(ext)
                        .map_or(Bson::Null, |c| Bson::String(c.to_string())),
                );
                if name == "pg_available_extensions" {
                    put("default_version", Bson::String(version.to_string()));
                    put(
                        "installed_version",
                        installed_version.map_or(Bson::Null, Bson::String),
                    );
                } else {
                    put("version", Bson::String(version.to_string()));
                    put("installed", Bson::Boolean(installed_version.is_some()));
                    put("superuser", Bson::Boolean(*superuser));
                    put("trusted", Bson::Boolean(*trusted));
                    put("relocatable", Bson::Boolean(*relocatable));
                    put(
                        "schema",
                        schema.map_or(Bson::Null, |s| Bson::String(s.into())),
                    );
                    put("requires", Bson::Null);
                }
                d
            })
            .collect()
    }

    /// `pg_largeobject`: each page a write reached, as long as its furthest
    /// byte (the object's page map). An older store's object, which has no
    /// map, shows its bytes in contiguous `LOBLKSIZE` pages.
    fn largeobject_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out = Vec::new();
        for (oid, data) in self.large_objects() {
            if let Ok(Some(pages)) = self.lo_pages(oid) {
                for (pageno, len) in pages {
                    let start = (pageno as usize) * LOBLKSIZE;
                    let end = (start + len as usize).min(data.len());
                    let mut d = Document::new();
                    d.insert(f("loid"), Bson::Int64(oid));
                    d.insert(f("pageno"), Bson::Int32(pageno as i32));
                    d.insert(
                        f("data"),
                        Bson::Binary(bson::Binary {
                            subtype: bson::spec::BinarySubtype::Generic,
                            bytes: data.get(start..end).unwrap_or_default().to_vec(),
                        }),
                    );
                    out.push(d);
                }
                continue;
            }
            for (pageno, page) in data.chunks(LOBLKSIZE).enumerate() {
                let mut d = Document::new();
                d.insert(f("loid"), Bson::Int64(oid));
                d.insert(f("pageno"), Bson::Int32(pageno as i32));
                d.insert(
                    f("data"),
                    Bson::Binary(bson::Binary {
                        subtype: bson::spec::BinarySubtype::Generic,
                        bytes: page.to_vec(),
                    }),
                );
                out.push(d);
            }
        }
        out
    }

    /// The comments on shared objects: `initdb`'s on the three databases,
    /// then the ones COMMENT ON DATABASE / ROLE set, for objects that exist.
    fn shdescription_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let databases = self.databases.all(&self.storage).unwrap_or_default();
        let roles = self.roles().unwrap_or_default();
        let stored = self.shared_comment_docs();
        let mut comments: Vec<(i64, i64, String)> = Vec::new();
        for (name, comment) in DATABASE_COMMENTS {
            if let Some(db) = databases.iter().find(|d| d.name == *name) {
                comments.push((db.oid, DATABASE_CLASS, (*comment).to_string()));
            }
        }
        for d in &stored {
            let (Ok(kind), Ok(oid)) = (d.get_str("kind"), d.get_i64("oid")) else {
                continue;
            };
            let class = if kind == "database" {
                DATABASE_CLASS
            } else {
                ROLE_CLASS
            };
            let exists = if class == DATABASE_CLASS {
                databases.iter().any(|db| db.oid == oid)
            } else {
                roles.iter().any(|r| r.oid == oid)
            };
            comments.retain(|(o, c, _)| !(*o == oid && *c == class));
            if let (true, Ok(text)) = (exists, d.get_str("comment")) {
                comments.push((oid, class, text.to_string()));
            }
        }
        comments
            .into_iter()
            .map(|(oid, class, text)| {
                let mut d = Document::new();
                d.insert(f("objoid"), Bson::Int64(oid));
                d.insert(f("classoid"), Bson::Int64(class));
                d.insert(f("description"), text);
                d
            })
            .collect()
    }

    fn shared_comment_docs(&self) -> Vec<Document> {
        let ns = DatabaseRegistry::NAMESPACE;
        let Ok(true) = self
            .storage
            .collection_exists(ns, SHARED_COMMENT_COLLECTION)
        else {
            return Vec::new();
        };
        self.storage
            .find_matching(ns, SHARED_COMMENT_COLLECTION, &Document::new())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .collect()
    }

    /// `COMMENT ON DATABASE / ROLE`: kept beside the databases, since every
    /// database sees it. A missing database is a WARNING (`CommentObject`),
    /// a missing role 42704. A NULL comment is kept too, as the removal of
    /// `initdb`'s.
    pub(crate) fn comment_on_shared(
        &self,
        kind: &str,
        name: &str,
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let oid = if kind == "database" {
            match self
                .databases
                .all(&self.storage)?
                .into_iter()
                .find(|d| d.name == name)
            {
                Some(db) => db.oid,
                None => {
                    self.warning("3D000", format!("database \"{name}\" does not exist"));
                    return Ok(());
                }
            }
        } else {
            match self.role(name)? {
                Some(r) => r.oid,
                None => {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{name}\" does not exist"),
                    ))
                }
            }
        };
        let ns = DatabaseRegistry::NAMESPACE;
        if !self
            .storage
            .collection_exists(ns, SHARED_COMMENT_COLLECTION)
            .map_err(|e| Self::storage_err("could not record the comment", e))?
        {
            self.storage
                .create_collection(ns, SHARED_COMMENT_COLLECTION)
                .map_err(|e| Self::storage_err("could not record the comment", e))?;
        }
        let id = format!("{kind}:{oid}");
        self.storage
            .delete_matching(
                ns,
                SHARED_COMMENT_COLLECTION,
                &bson::doc! {"_id": &id},
                0,
                &Document::new(),
                None,
            )
            .map_err(|e| Self::storage_err("could not record the comment", e))?;
        let mut doc = bson::doc! {"_id": &id, "kind": kind, "oid": oid};
        if let Some(c) = comment {
            doc.insert("comment", c);
        }
        let bytes =
            encode_doc(&doc).map_err(|e| Self::storage_err("could not encode the comment", e))?;
        self.storage
            .insert(ns, SHARED_COMMENT_COLLECTION, vec![bytes], true)
            .map_err(|e| Self::storage_err("could not record the comment", e))?;
        Ok(())
    }
}
