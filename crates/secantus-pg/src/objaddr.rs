//! `pg_describe_object(classid, objid, objsubid)` (RP-199): an object's
//! description, as PostgreSQL 15.19's `getObjectDescription` writes it --
//! `table pg_class`, `column relname of table pg_class`, `operator class c
//! for access method btree`. Each catalog's row is read through this
//! server's own catalogs, so the description names what `pg_class`,
//! `pg_proc` and the rest list. An object that does not exist is NULL, as
//! with PostgreSQL's `missing_ok`; a class that is no catalog is XX000.

use std::collections::HashMap;

use bson::Bson;
use pgwire::error::PgWireResult;

use crate::{bson_i64, PgHandler};

fn text(v: Option<&Bson>) -> String {
    match v {
        Some(Bson::String(s)) => s.clone(),
        Some(other) => bson_i64(other).map_or_else(String::new, |i| i.to_string()),
        None => String::new(),
    }
}

/// An oid, a number or any `reg*` value (`'int4'::regtype`).
fn int(v: Option<&Bson>) -> i64 {
    v.and_then(|v| {
        bson_i64(v)
            .or_else(|| secantus_pgplan::regtype_oid(v))
            .or_else(|| secantus_pgplan::regobj::from_bson(v).map(|(_, o)| o))
            .or_else(|| secantus_pgplan::regclass_oid(v))
    })
    .unwrap_or(0)
}

impl PgHandler {
    /// An oid argument as the executor hook passes it: a number, a `reg*`
    /// value, or -- for anything that reads as a `regclass` -- that
    /// relation's text, which may be the oid itself.
    fn oid_arg(&self, v: Option<&Bson>) -> i64 {
        match v {
            Some(Bson::String(name)) => name.trim().parse().unwrap_or_else(|_| {
                self.object_row(&format!(
                    "select {}::regclass::oid::int8",
                    secantus_pgplan::scalar::quote_literal(name)
                ))
                .map_or(0, |r| int(r.first()))
            }),
            other => int(other),
        }
    }

    /// The first row of `sql`, or `None` (no row, or a catalog this server
    /// does not serve).
    fn object_row(&self, sql: &str) -> Option<Vec<Bson>> {
        self.run_sql_rows(sql, &[]).ok()?.into_iter().next()
    }

    /// The schemas a bare name is looked up in, in order: the session's
    /// temporary schema, `pg_catalog` (unless the path places it), then
    /// the path.
    fn lookup_schemas(&self) -> Vec<String> {
        let path = secantus_pgplan::schemas::search_path();
        let mut out = vec![self.temp_schema_name()];
        if !path.iter().any(|s| s == "pg_catalog") {
            out.push("pg_catalog".into());
        }
        out.extend(path);
        out
    }

    /// Is the object in `schema` the one its bare name finds? `in_schema`
    /// says whether a schema holds an object of the name.
    fn object_visible(&self, schema: &str, in_schema: impl Fn(&str) -> bool) -> bool {
        for s in self.lookup_schemas() {
            if s == schema {
                return true;
            }
            if in_schema(&s) {
                return false;
            }
        }
        false
    }

    /// `quote_qualified_identifier`, the schema left out when visible.
    fn qualified_name(&self, visible: bool, schema: &str, name: &str) -> String {
        let q = secantus_pgplan::scalar::quote_identifier;
        if visible {
            q(name)
        } else {
            format!("{}.{}", q(schema), q(name))
        }
    }

    /// `[schema.]name` of a catalog's object, by `(oid column, name column,
    /// namespace column)`: qualified when an earlier schema on the path
    /// holds one of the name.
    fn named_in_schema(&self, catalog: &str, cols: (&str, &str, &str), oid: i64) -> Option<String> {
        let (oid_col, name_col, ns_col) = cols;
        let row = self.object_row(&format!(
            "select o.{name_col}::text, n.nspname::text from pg_catalog.{catalog} o \
             join pg_catalog.pg_namespace n on n.oid = o.{ns_col} where o.{oid_col} = {oid}"
        ))?;
        let (name, schema) = (text(row.first()), text(row.get(1)));
        let visible = self.object_visible(&schema, |s| {
            self.object_row(&format!(
                "select 1 from pg_catalog.{catalog} o join pg_catalog.pg_namespace n \
                 on n.oid = o.{ns_col} where o.{name_col}::text = {} and n.nspname = {}",
                secantus_pgplan::scalar::quote_literal(&name),
                secantus_pgplan::scalar::quote_literal(s)
            ))
            .is_some()
        });
        Some(self.qualified_name(visible, &schema, &name))
    }

    /// `getRelationDescription`.
    fn relation_description(&self, oid: i64) -> Option<String> {
        let row = self.object_row(&format!(
            "select c.relkind::text from pg_catalog.pg_class c where c.oid = {oid}"
        ))?;
        let word = match text(row.first()).as_str() {
            "r" | "p" => "table",
            "i" | "I" => "index",
            "S" => "sequence",
            "t" => "toast table",
            "v" => "view",
            "m" => "materialized view",
            "c" => "composite type",
            "f" => "foreign table",
            _ => "relation",
        };
        let name = self.named_in_schema("pg_class", ("oid", "relname", "relnamespace"), oid)?;
        Some(format!("{word} {name}"))
    }

    /// `getOpFamilyDescription`.
    fn opfamily_description_of(&self, oid: i64) -> Option<String> {
        let row = self.object_row(&format!(
            "select a.amname::text from pg_catalog.pg_opfamily f join pg_catalog.pg_am a \
             on a.oid = f.opfmethod where f.oid = {oid}"
        ))?;
        let name = self.named_in_schema("pg_opfamily", ("oid", "opfname", "opfnamespace"), oid)?;
        Some(format!(
            "operator family {name} for access method {}",
            text(row.first())
        ))
    }

    fn role_name_of(&self, oid: i64) -> Option<String> {
        if oid == 0 {
            return Some("public".into());
        }
        self.object_row(&format!(
            "select rolname::text from pg_catalog.pg_roles where oid = {oid}"
        ))
        .map(|r| text(r.first()))
    }

    /// `getObjectDescription(classid, objid, objsubid)`, `None` for an
    /// object that does not exist.
    pub(crate) fn describe_object(
        &self,
        classid: i64,
        objid: i64,
        subid: i64,
    ) -> PgWireResult<Option<String>> {
        let t = secantus_pgplan::regtype_text;
        let row = |sql: String| self.object_row(&sql);
        let one = |catalog: &str, col: &str| {
            row(format!(
                "select {col}::text from pg_catalog.{catalog} where oid = {objid}"
            ))
            .map(|r| text(r.first()))
        };
        Ok(match classid {
            // pg_class
            1259 if subid == 0 => self.relation_description(objid),
            1259 => {
                let att = row(format!(
                    "select attname::text from pg_catalog.pg_attribute where attrelid = {objid} \
                     and attnum = {subid} and not attisdropped"
                ));
                match (att, self.relation_description(objid)) {
                    (Some(a), Some(rel)) => Some(format!("column {} of {rel}", text(a.first()))),
                    _ => None,
                }
            }
            // pg_proc
            1255 => one("pg_proc", "oid")
                .map(|_| format!("function {}", secantus_pgplan::regobj::text("regprocedure", objid))),
            // pg_type
            1247 => one("pg_type", "oid").map(|_| format!("type {}", t(objid))),
            // pg_cast
            2605 => row(format!(
                "select castsource::int8, casttarget::int8 from pg_catalog.pg_cast where oid = {objid}"
            ))
            .map(|r| format!("cast from {} to {}", t(int(r.first())), t(int(r.get(1))))),
            // pg_collation
            3456 => self
                .named_in_schema("pg_collation", ("oid", "collname", "collnamespace"), objid)
                .map(|n| format!("collation {n}")),
            // pg_constraint
            2606 => row(format!(
                "select conname::text, conrelid::int8 from pg_catalog.pg_constraint where oid = {objid}"
            ))
            .and_then(|r| {
                let name = text(r.first());
                match int(r.get(1)) {
                    0 => Some(format!("constraint {name}")),
                    rel => self
                        .relation_description(rel)
                        .map(|d| format!("constraint {name} on {d}")),
                }
            }),
            // pg_conversion
            2607 => self
                .named_in_schema("pg_conversion", ("oid", "conname", "connamespace"), objid)
                .map(|n| format!("conversion {n}")),
            // pg_attrdef
            2604 => match row(format!(
                "select adrelid::int8, adnum::int8 from pg_catalog.pg_attrdef where oid = {objid}"
            )) {
                Some(r) => self
                    .describe_object(1259, int(r.first()), int(r.get(1)))?
                    .map(|c| format!("default value for {c}")),
                None => None,
            },
            // pg_language
            2612 => one("pg_language", "lanname").map(|n| format!("language {n}")),
            // pg_largeobject_metadata
            2613 => one("pg_largeobject_metadata", "oid").map(|_| format!("large object {objid}")),
            // pg_operator
            2617 => row(format!(
                "select oprname::text, oprleft::int8, oprright::int8 from pg_catalog.pg_operator \
                 where oid = {objid}"
            ))
            .map(|r| {
                let side = |o: i64| if o == 0 { "NONE".to_string() } else { t(o) };
                format!(
                    "operator {}({},{})",
                    text(r.first()),
                    side(int(r.get(1))),
                    side(int(r.get(2)))
                )
            })
            .or_else(|| {
                let shown = secantus_pgplan::regobj::text("regoperator", objid);
                (shown != objid.to_string()).then(|| format!("operator {shown}"))
            }),
            // pg_opclass
            2616 => row(format!(
                "select a.amname::text from pg_catalog.pg_opclass c join pg_catalog.pg_am a \
                 on a.oid = c.opcmethod where c.oid = {objid}"
            ))
            .and_then(|r| {
                self.named_in_schema("pg_opclass", ("oid", "opcname", "opcnamespace"), objid)
                    .map(|n| format!("operator class {n} for access method {}", text(r.first())))
            }),
            // pg_opfamily
            2753 => self.opfamily_description_of(objid),
            // pg_am
            2601 => one("pg_am", "amname").map(|n| format!("access method {n}")),
            // pg_amop
            2602 => row(format!(
                "select amopstrategy::int8, amoplefttype::int8, amoprighttype::int8, \
                 amopfamily::int8, amopopr::int8 from pg_catalog.pg_amop where oid = {objid}"
            ))
            .and_then(|r| {
                let family = self.opfamily_description_of(int(r.get(3)))?;
                Some(format!(
                    "operator {} ({}, {}) of {family}: {}",
                    int(r.first()),
                    t(int(r.get(1))),
                    t(int(r.get(2))),
                    secantus_pgplan::regobj::text("regoperator", int(r.get(4)))
                ))
            }),
            // pg_amproc
            2603 => row(format!(
                "select amprocnum::int8, amproclefttype::int8, amprocrighttype::int8, \
                 amprocfamily::int8, amproc::oid::int8 from pg_catalog.pg_amproc where oid = {objid}"
            ))
            .and_then(|r| {
                let family = self.opfamily_description_of(int(r.get(3)))?;
                Some(format!(
                    "function {} ({}, {}) of {family}: {}",
                    int(r.first()),
                    t(int(r.get(1))),
                    t(int(r.get(2))),
                    secantus_pgplan::regobj::text("regprocedure", int(r.get(4)))
                ))
            }),
            // pg_rewrite
            2618 => row(format!(
                "select rulename::text, ev_class::int8 from pg_catalog.pg_rewrite where oid = {objid}"
            ))
            .and_then(|r| {
                self.relation_description(int(r.get(1)))
                    .map(|d| format!("rule {} on {d}", text(r.first())))
            }),
            // pg_trigger
            2620 => row(format!(
                "select tgname::text, tgrelid::int8 from pg_catalog.pg_trigger where oid = {objid}"
            ))
            .and_then(|r| {
                self.relation_description(int(r.get(1)))
                    .map(|d| format!("trigger {} on {d}", text(r.first())))
            }),
            // pg_namespace
            2615 => one("pg_namespace", "nspname").map(|n| format!("schema {n}")),
            // pg_statistic_ext
            3381 => self
                .named_in_schema("pg_statistic_ext", ("oid", "stxname", "stxnamespace"), objid)
                .map(|n| format!("statistics object {n}")),
            // The text-search catalogs.
            3601 => self
                .named_in_schema("pg_ts_parser", ("oid", "prsname", "prsnamespace"), objid)
                .map(|n| format!("text search parser {n}")),
            3600 => self
                .named_in_schema("pg_ts_dict", ("oid", "dictname", "dictnamespace"), objid)
                .map(|n| format!("text search dictionary {n}")),
            3764 => self
                .named_in_schema("pg_ts_template", ("oid", "tmplname", "tmplnamespace"), objid)
                .map(|n| format!("text search template {n}")),
            3602 => self
                .named_in_schema("pg_ts_config", ("oid", "cfgname", "cfgnamespace"), objid)
                .map(|n| format!("text search configuration {n}")),
            // pg_authid
            1260 => one("pg_roles", "rolname").map(|n| format!("role {n}")),
            // pg_auth_members
            1261 => row(format!(
                "select member::int8, roleid::int8 from pg_catalog.pg_auth_members where oid = {objid}"
            ))
            .and_then(|r| {
                Some(format!(
                    "membership of role {} in role {}",
                    self.role_name_of(int(r.first()))?,
                    self.role_name_of(int(r.get(1)))?
                ))
            }),
            1262 => one("pg_database", "datname").map(|n| format!("database {n}")),
            1213 => one("pg_tablespace", "spcname").map(|n| format!("tablespace {n}")),
            2328 => one("pg_foreign_data_wrapper", "fdwname")
                .map(|n| format!("foreign-data wrapper {n}")),
            1417 => one("pg_foreign_server", "srvname").map(|n| format!("server {n}")),
            1418 => row(format!(
                "select umuser::int8, umserver::int8 from pg_catalog.pg_user_mapping where oid = {objid}"
            ))
            .and_then(|r| {
                let server = row(format!(
                    "select srvname::text from pg_catalog.pg_foreign_server where oid = {}",
                    int(r.get(1))
                ))?;
                Some(format!(
                    "user mapping for {} on server {}",
                    self.role_name_of(int(r.first()))?,
                    text(server.first())
                ))
            }),
            // pg_default_acl
            826 => row(format!(
                "select defaclrole::int8, defaclnamespace::int8, defaclobjtype::text \
                 from pg_catalog.pg_default_acl where oid = {objid}"
            ))
            .and_then(|r| {
                let role = self.role_name_of(int(r.first()))?;
                let schema = match int(r.get(1)) {
                    0 => None,
                    ns => row(format!(
                        "select nspname::text from pg_catalog.pg_namespace where oid = {ns}"
                    ))
                    .map(|n| text(n.first())),
                };
                let what = match text(r.get(2)).as_str() {
                    "r" => Some("new relations"),
                    "S" => Some("new sequences"),
                    "f" => Some("new functions"),
                    "T" => Some("new types"),
                    "n" => Some("new schemas"),
                    _ => None,
                };
                Some(match (what, schema) {
                    (Some("new schemas"), _) => {
                        format!("default privileges on new schemas belonging to role {role}")
                    }
                    (Some(w), Some(s)) => {
                        format!("default privileges on {w} belonging to role {role} in schema {s}")
                    }
                    (Some(w), None) => format!("default privileges on {w} belonging to role {role}"),
                    (None, Some(s)) => {
                        format!("default privileges belonging to role {role} in schema {s}")
                    }
                    (None, None) => format!("default privileges belonging to role {role}"),
                })
            }),
            3079 => one("pg_extension", "extname").map(|n| format!("extension {n}")),
            3466 => one("pg_event_trigger", "evtname").map(|n| format!("event trigger {n}")),
            6243 => one("pg_parameter_acl", "parname").map(|n| format!("parameter {n}")),
            // pg_policy
            3256 => row(format!(
                "select polname::text, polrelid::int8 from pg_catalog.pg_policy where oid = {objid}"
            ))
            .and_then(|r| {
                self.relation_description(int(r.get(1)))
                    .map(|d| format!("policy {} on {d}", text(r.first())))
            }),
            6104 => one("pg_publication", "pubname").map(|n| format!("publication {n}")),
            // pg_publication_namespace
            6237 => row(format!(
                "select n.nspname::text, p.pubname::text from pg_catalog.pg_publication_namespace pn \
                 join pg_catalog.pg_namespace n on n.oid = pn.pnnspid \
                 join pg_catalog.pg_publication p on p.oid = pn.pnpubid where pn.oid = {objid}"
            ))
            .map(|r| {
                format!(
                    "publication of schema {} in publication {}",
                    text(r.first()),
                    text(r.get(1))
                )
            }),
            // pg_publication_rel
            6106 => row(format!(
                "select pr.prrelid::int8, p.pubname::text from pg_catalog.pg_publication_rel pr \
                 join pg_catalog.pg_publication p on p.oid = pr.prpubid where pr.oid = {objid}"
            ))
            .and_then(|r| {
                self.relation_description(int(r.first()))
                    .map(|d| format!("publication of {d} in publication {}", text(r.get(1))))
            }),
            6100 => one("pg_subscription", "subname").map(|n| format!("subscription {n}")),
            // pg_transform
            3576 => row(format!(
                "select trftype::int8, l.lanname::text from pg_catalog.pg_transform t \
                 join pg_catalog.pg_language l on l.oid = t.trflang where t.oid = {objid}"
            ))
            .map(|r| format!("transform for {} language {}", t(int(r.first())), text(r.get(1)))),
            _ => {
                return Err(Self::user_error(
                    "XX000",
                    format!("unrecognized object class: {classid}"),
                ))
            }
        })
    }

    /// `pg_describe_object(classid, objid, objsubid)`: a `regclass` arrives
    /// as its name.
    pub(crate) fn describe_object_call(&self, args: &[Bson]) -> PgWireResult<Bson> {
        if args.iter().any(|a| matches!(a, Bson::Null)) {
            return Ok(Bson::Null);
        }
        let classid = self.oid_arg(args.first());
        // A `regclass` objid arrives as its name too.
        let objid = self.oid_arg(args.get(1));
        let subid = int(args.get(2));
        Ok(self
            .describe_object(classid, objid, subid)?
            .map_or(Bson::Null, Bson::String))
    }
}

/// `pg_X_is_visible`'s catalog: `(catalog, name column, namespace column,
/// the columns an earlier object must share to hide this one)`.
fn visibility_catalog(
    function: &str,
) -> Option<(
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
)> {
    Some(match function {
        "pg_type_is_visible" => ("pg_type", "typname", "typnamespace", &[]),
        "pg_function_is_visible" => ("pg_proc", "proname", "pronamespace", &["proargtypes"]),
        "pg_operator_is_visible" => (
            "pg_operator",
            "oprname",
            "oprnamespace",
            &["oprleft", "oprright"],
        ),
        "pg_opclass_is_visible" => ("pg_opclass", "opcname", "opcnamespace", &["opcmethod"]),
        "pg_opfamily_is_visible" => ("pg_opfamily", "opfname", "opfnamespace", &["opfmethod"]),
        "pg_collation_is_visible" => ("pg_collation", "collname", "collnamespace", &[]),
        "pg_conversion_is_visible" => ("pg_conversion", "conname", "connamespace", &[]),
        "pg_statistics_obj_is_visible" => ("pg_statistic_ext", "stxname", "stxnamespace", &[]),
        "pg_ts_parser_is_visible" => ("pg_ts_parser", "prsname", "prsnamespace", &[]),
        "pg_ts_dict_is_visible" => ("pg_ts_dict", "dictname", "dictnamespace", &[]),
        "pg_ts_template_is_visible" => ("pg_ts_template", "tmplname", "tmplnamespace", &[]),
        "pg_ts_config_is_visible" => ("pg_ts_config", "cfgname", "cfgnamespace", &[]),
        _ => return None,
    })
}

/// The visibility functions this server answers by looking the object up.
pub(crate) const VISIBILITY_FUNCTIONS: &[&str] = &[
    "pg_type_is_visible",
    "pg_function_is_visible",
    "pg_operator_is_visible",
    "pg_opclass_is_visible",
    "pg_opfamily_is_visible",
    "pg_collation_is_visible",
    "pg_conversion_is_visible",
    "pg_statistics_obj_is_visible",
    "pg_ts_parser_is_visible",
    "pg_ts_dict_is_visible",
    "pg_ts_template_is_visible",
    "pg_ts_config_is_visible",
];

/// One catalog's objects for the visibility functions: by oid, `(name,
/// schema, the columns that must match)`, and by name the schemas holding
/// one with those columns.
struct Visibility {
    by_oid: HashMap<i64, (String, String, String)>,
    by_name: HashMap<(String, String), Vec<String>>,
}

type VisibilityKey = (&'static str, u64, String, u64);

thread_local! {
    /// Read once per catalog version, search path and session: a
    /// visibility test over every row of `pg_proc` (psql's `\df`) asked the
    /// catalog once per row.
    static VISIBILITY: std::cell::RefCell<Option<(VisibilityKey, std::rc::Rc<Visibility>)>> =
        const { std::cell::RefCell::new(None) };
}

impl PgHandler {
    fn visibility_table(
        &self,
        catalog: &'static str,
        name_col: &str,
        ns_col: &str,
        extra: &[&str],
    ) -> std::rc::Rc<Visibility> {
        let key: VisibilityKey = (
            catalog,
            crate::catalog_cache()
                .version
                .load(std::sync::atomic::Ordering::SeqCst),
            secantus_pgplan::schemas::search_path().join(","),
            if self.block_wrote_catalog() {
                self.session_serial
            } else {
                0
            },
        );
        if let Some(t) = VISIBILITY.with(|v| {
            v.borrow()
                .as_ref()
                .filter(|(k, _)| *k == key)
                .map(|(_, t)| std::rc::Rc::clone(t))
        }) {
            return t;
        }
        // The raw columns, no join and no casts: a text cast of every
        // `proargtypes` cost most of a second.
        let extra_sel: String = extra.iter().map(|c| format!(", o.{c}")).collect();
        let rows = self.within_block(|| {
            self.run_sql_rows(
                &format!(
                    "select o.oid, o.{name_col}, o.{ns_col}{extra_sel} from pg_catalog.{catalog} o"
                ),
                &[],
            )
            .unwrap_or_default()
        });
        let schemas: HashMap<i64, String> =
            self.namespaces().into_iter().map(|(n, o)| (o, n)).collect();
        let mut table = Visibility {
            by_oid: HashMap::new(),
            by_name: HashMap::new(),
        };
        for r in rows {
            let (oid, name) = (int(r.first()), text(r.get(1)));
            let schema = schemas.get(&int(r.get(2))).cloned().unwrap_or_default();
            let same = format!("{:?}", &r[3.min(r.len())..]);
            table
                .by_name
                .entry((name.clone(), same.clone()))
                .or_default()
                .push(schema.clone());
            table.by_oid.insert(oid, (name, schema, same));
        }
        let table = std::rc::Rc::new(table);
        VISIBILITY.with(|v| *v.borrow_mut() = Some((key, std::rc::Rc::clone(&table))));
        table
    }

    /// `pg_type_is_visible(oid)` and its siblings (RP-210): NULL for an oid
    /// that names nothing; else whether the object's bare name -- with its
    /// argument types, for a function or an operator -- finds it on the
    /// search path.
    pub(crate) fn is_visible_call(&self, function: &str, args: &[Bson]) -> PgWireResult<Bson> {
        let Some((catalog, name_col, ns_col, extra)) = visibility_catalog(function) else {
            return Ok(Bson::Null);
        };
        if matches!(args.first(), None | Some(Bson::Null)) {
            return Ok(Bson::Null);
        }
        let oid = self.oid_arg(args.first());
        let table = self.visibility_table(catalog, name_col, ns_col, extra);
        let Some((name, schema, same)) = table.by_oid.get(&oid) else {
            return Ok(Bson::Null);
        };
        let holders = table.by_name.get(&(name.clone(), same.clone()));
        Ok(Bson::Boolean(self.object_visible(schema, |s| {
            holders.is_some_and(|h| h.iter().any(|x| x == s))
        })))
    }
}

/// An object's identity, as `getObjectIdentityParts` builds it: the text
/// `pg_identify_object` reports, and the name and argument lists that
/// `pg_identify_object_as_address` reports and `pg_get_object_address`
/// reads back.
pub(crate) struct Identity {
    pub(crate) identity: String,
    pub(crate) names: Vec<String>,
    pub(crate) args: Vec<String>,
}

/// `format_type_be_qualified`: a type's SQL name (`integer`, `character
/// varying`) where the grammar has one, else `schema.name`, always
/// qualified; an array is its element's and `[]`.
fn qualified_type_name(h: &PgHandler, oid: i64) -> String {
    let row = h.object_row(&format!(
        "select t.typname::text, n.nspname::text, t.typelem::int8, t.typlen::int8 \
         from pg_catalog.pg_type t join pg_catalog.pg_namespace n on n.oid = t.typnamespace \
         where t.oid = {oid}"
    ));
    let Some(row) = row else {
        return "???".into();
    };
    let (name, schema, elem, len) = (
        text(row.first()),
        text(row.get(1)),
        int(row.get(2)),
        int(row.get(3)),
    );
    if elem != 0 && len == -1 {
        return format!("{}[]", qualified_type_name(h, elem));
    }
    let sql = match name.as_str() {
        "bit" => "bit",
        "bool" => "boolean",
        "bpchar" => "character",
        "float4" => "real",
        "float8" => "double precision",
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "numeric" => "numeric",
        "interval" => "interval",
        "time" => "time without time zone",
        "timetz" => "time with time zone",
        "timestamp" => "timestamp without time zone",
        "timestamptz" => "timestamp with time zone",
        "varbit" => "bit varying",
        "varchar" => "character varying",
        _ => "",
    };
    if schema == "pg_catalog" && !sql.is_empty() {
        return sql.into();
    }
    let q = secantus_pgplan::scalar::quote_identifier;
    format!("{}.{}", q(&schema), q(&name))
}

/// A catalog's `(name column, namespace column)` where `pg_identify_object`
/// reports them: a schema only where the catalog has one, a name only where
/// it identifies the object (with the schema, if any) on its own
/// (`get_object_namensp_unique`).
fn identify_columns(classid: i64) -> (Option<&'static str>, Option<&'static str>, &'static str) {
    // (name column, namespace column, catalog)
    match classid {
        1259 => (Some("relname"), Some("relnamespace"), "pg_class"),
        1247 => (Some("typname"), Some("typnamespace"), "pg_type"),
        1255 => (None, Some("pronamespace"), "pg_proc"),
        2617 => (None, Some("oprnamespace"), "pg_operator"),
        2616 => (Some("opcname"), Some("opcnamespace"), "pg_opclass"),
        2753 => (Some("opfname"), Some("opfnamespace"), "pg_opfamily"),
        3456 => (Some("collname"), Some("collnamespace"), "pg_collation"),
        2607 => (Some("conname"), Some("connamespace"), "pg_conversion"),
        2606 => (None, Some("connamespace"), "pg_constraint"),
        3381 => (Some("stxname"), Some("stxnamespace"), "pg_statistic_ext"),
        3601 => (Some("prsname"), Some("prsnamespace"), "pg_ts_parser"),
        3600 => (Some("dictname"), Some("dictnamespace"), "pg_ts_dict"),
        3764 => (Some("tmplname"), Some("tmplnamespace"), "pg_ts_template"),
        3602 => (Some("cfgname"), Some("cfgnamespace"), "pg_ts_config"),
        2615 => (Some("nspname"), None, "pg_namespace"),
        1260 => (Some("rolname"), None, "pg_roles"),
        1262 => (Some("datname"), None, "pg_database"),
        1213 => (Some("spcname"), None, "pg_tablespace"),
        2328 => (Some("fdwname"), None, "pg_foreign_data_wrapper"),
        1417 => (Some("srvname"), None, "pg_foreign_server"),
        3079 => (Some("extname"), None, "pg_extension"),
        3466 => (Some("evtname"), None, "pg_event_trigger"),
        2612 => (Some("lanname"), None, "pg_language"),
        2601 => (Some("amname"), None, "pg_am"),
        6104 => (Some("pubname"), None, "pg_publication"),
        6100 => (Some("subname"), None, "pg_subscription"),
        _ => (None, None, ""),
    }
}

impl PgHandler {
    /// `getObjectTypeDescription`: what kind of object `(classid, objid,
    /// subid)` is, worded for a missing one too (`relation`, `routine`).
    fn object_type_word(&self, classid: i64, objid: i64, subid: i64) -> PgWireResult<String> {
        let row = |sql: String| self.object_row(&sql);
        Ok(match classid {
            1259 => {
                let kind = row(format!("select relkind::text from pg_catalog.pg_class where oid = {objid}"))
                    .map(|r| text(r.first()));
                let word = match kind.as_deref() {
                    Some("r" | "p") => "table",
                    Some("i" | "I") => "index",
                    Some("S") => "sequence",
                    Some("t") => "toast table",
                    Some("v") => "view",
                    Some("m") => "materialized view",
                    Some("c") => "composite type",
                    Some("f") => "foreign table",
                    _ => "relation",
                };
                if subid != 0 && kind.is_some() {
                    format!("{word} column")
                } else {
                    word.to_string()
                }
            }
            1255 => match row(format!("select prokind::text from pg_catalog.pg_proc where oid = {objid}"))
                .map(|r| text(r.first()))
                .as_deref()
            {
                Some("a") => "aggregate".into(),
                Some("p") => "procedure".into(),
                Some(_) => "function".into(),
                None => "routine".into(),
            },
            2606 => match row(format!(
                "select conrelid::int8, contypid::int8 from pg_catalog.pg_constraint where oid = {objid}"
            )) {
                Some(r) if int(r.first()) != 0 => "table constraint".into(),
                Some(r) if int(r.get(1)) != 0 => "domain constraint".into(),
                _ => "constraint".into(),
            },
            1247 => "type".into(),
            2605 => "cast".into(),
            3456 => "collation".into(),
            2607 => "conversion".into(),
            2604 => "default value".into(),
            2612 => "language".into(),
            2613 => "large object".into(),
            2617 => "operator".into(),
            2616 => "operator class".into(),
            2753 => "operator family".into(),
            2601 => "access method".into(),
            2602 => "operator of access method".into(),
            2603 => "function of access method".into(),
            2618 => "rule".into(),
            2620 => "trigger".into(),
            2615 => "schema".into(),
            3381 => "statistics object".into(),
            3601 => "text search parser".into(),
            3600 => "text search dictionary".into(),
            3764 => "text search template".into(),
            3602 => "text search configuration".into(),
            1260 => "role".into(),
            1261 => "role membership".into(),
            1262 => "database".into(),
            1213 => "tablespace".into(),
            2328 => "foreign-data wrapper".into(),
            1417 => "server".into(),
            1418 => "user mapping".into(),
            826 => "default acl".into(),
            3079 => "extension".into(),
            3466 => "event trigger".into(),
            6243 => "parameter ACL".into(),
            3256 => "policy".into(),
            6104 => "publication".into(),
            6237 => "publication namespace".into(),
            6106 => "publication relation".into(),
            6100 => "subscription".into(),
            3576 => "transform".into(),
            _ => {
                return Err(Self::user_error(
                    "XX000",
                    format!("unrecognized object class: {classid}"),
                ))
            }
        })
    }
}

fn qualify(schema: &str, name: &str) -> String {
    let q = secantus_pgplan::scalar::quote_identifier;
    format!("{}.{}", q(schema), q(name))
}

fn oid_list(v: Option<&Bson>) -> Vec<i64> {
    match v {
        Some(Bson::Array(items)) => items.iter().map(|i| int(Some(i))).collect(),
        other => text(other)
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect(),
    }
}

impl PgHandler {
    /// `(schema, name)` of a catalog's object.
    fn schema_and_name(
        &self,
        catalog: &str,
        name_col: &str,
        ns_col: &str,
        oid: i64,
    ) -> Option<(String, String)> {
        let r = self.object_row(&format!(
            "select o.{name_col}::text, n.nspname::text from pg_catalog.{catalog} o \
             join pg_catalog.pg_namespace n on n.oid = o.{ns_col} where o.oid = {oid}"
        ))?;
        Some((text(r.get(1)), text(r.first())))
    }

    /// `getRelationIdentity`: always schema-qualified.
    fn relation_identity(&self, oid: i64) -> Option<Identity> {
        let (schema, name) = self.schema_and_name("pg_class", "relname", "relnamespace", oid)?;
        Some(Identity {
            identity: qualify(&schema, &name),
            names: vec![schema, name],
            args: Vec::new(),
        })
    }

    /// `getOpFamilyIdentity`, and an operator class's likewise.
    fn method_object_identity(&self, catalog: &str, prefix: &str, oid: i64) -> Option<Identity> {
        let r = self.object_row(&format!(
            "select o.{prefix}name::text, n.nspname::text, a.amname::text from pg_catalog.{catalog} o \
             join pg_catalog.pg_namespace n on n.oid = o.{prefix}namespace \
             join pg_catalog.pg_am a on a.oid = o.{prefix}method where o.oid = {oid}"
        ))?;
        let (name, schema, am) = (text(r.first()), text(r.get(1)), text(r.get(2)));
        Some(Identity {
            identity: format!(
                "{} USING {}",
                qualify(&schema, &name),
                secantus_pgplan::scalar::quote_identifier(&am)
            ),
            names: vec![am, schema, name],
            args: Vec::new(),
        })
    }

    fn simple_identity(&self, catalog: &str, col: &str, oid: i64) -> Option<Identity> {
        let r = self.object_row(&format!(
            "select {col}::text from pg_catalog.{catalog} where oid = {oid}"
        ))?;
        let name = text(r.first());
        Some(Identity {
            identity: secantus_pgplan::scalar::quote_identifier(&name),
            names: vec![name],
            args: Vec::new(),
        })
    }

    fn schema_identity(
        &self,
        catalog: &str,
        name_col: &str,
        ns_col: &str,
        oid: i64,
    ) -> Option<Identity> {
        let (schema, name) = self.schema_and_name(catalog, name_col, ns_col, oid)?;
        Some(Identity {
            identity: qualify(&schema, &name),
            names: vec![schema, name],
            args: Vec::new(),
        })
    }

    /// `getObjectIdentityParts`, `None` for an object that does not exist.
    pub(crate) fn object_identity(
        &self,
        classid: i64,
        objid: i64,
        subid: i64,
    ) -> PgWireResult<Option<Identity>> {
        let row = |sql: String| self.object_row(&sql);
        let ty = |o: i64| qualified_type_name(self, o);
        let on_relation = |name: String, rel: i64| {
            self.relation_identity(rel).map(|r| Identity {
                identity: format!(
                    "{} on {}",
                    secantus_pgplan::scalar::quote_identifier(&name),
                    r.identity
                ),
                names: r.names.into_iter().chain([name]).collect(),
                args: Vec::new(),
            })
        };
        let role = |o: i64| -> Option<String> {
            if o == 0 {
                Some("public".into())
            } else {
                self.object_row(&format!(
                    "select rolname::text from pg_catalog.pg_roles where oid = {o}"
                ))
                .map(|r| text(r.first()))
            }
        };
        Ok(match classid {
            1259 if subid == 0 => self.relation_identity(objid),
            1259 => {
                let att = row(format!(
                    "select attname::text from pg_catalog.pg_attribute where attrelid = {objid} \
                     and attnum = {subid} and not attisdropped"
                ));
                match (att, self.relation_identity(objid)) {
                    (Some(a), Some(mut r)) => {
                        let a = text(a.first());
                        r.identity = format!("{}.{}", r.identity, secantus_pgplan::scalar::quote_identifier(&a));
                        r.names.push(a);
                        Some(r)
                    }
                    _ => None,
                }
            }
            1255 => row(format!(
                "select p.proname::text, n.nspname::text, p.proargtypes from pg_catalog.pg_proc p \
                 join pg_catalog.pg_namespace n on n.oid = p.pronamespace where p.oid = {objid}"
            ))
            .map(|r| {
                let (name, schema) = (text(r.first()), text(r.get(1)));
                let args: Vec<String> = oid_list(r.get(2)).into_iter().map(ty).collect();
                Identity {
                    identity: format!("{}({})", qualify(&schema, &name), args.join(",")),
                    names: vec![schema, name],
                    args,
                }
            }),
            1247 => one_row(self, "pg_type", objid).map(|_| {
                let name = ty(objid);
                Identity { identity: name.clone(), names: vec![name], args: Vec::new() }
            }),
            2605 => row(format!(
                "select castsource::int8, casttarget::int8 from pg_catalog.pg_cast where oid = {objid}"
            ))
            .map(|r| {
                let (s, t) = (ty(int(r.first())), ty(int(r.get(1))));
                Identity { identity: format!("({s} AS {t})"), names: vec![s], args: vec![t] }
            }),
            3456 => self.schema_identity("pg_collation", "collname", "collnamespace", objid),
            2607 => self.schema_identity("pg_conversion", "conname", "connamespace", objid),
            3381 => self.schema_identity("pg_statistic_ext", "stxname", "stxnamespace", objid),
            3601 => self.schema_identity("pg_ts_parser", "prsname", "prsnamespace", objid),
            3600 => self.schema_identity("pg_ts_dict", "dictname", "dictnamespace", objid),
            3764 => self.schema_identity("pg_ts_template", "tmplname", "tmplnamespace", objid),
            3602 => self.schema_identity("pg_ts_config", "cfgname", "cfgnamespace", objid),
            2606 => row(format!(
                "select conname::text, conrelid::int8, contypid::int8 from pg_catalog.pg_constraint \
                 where oid = {objid}"
            ))
            .and_then(|r| {
                let name = text(r.first());
                match (int(r.get(1)), int(r.get(2))) {
                    (0, 0) => None,
                    (0, domain) => {
                        let d = ty(domain);
                        Some(Identity {
                            identity: format!("{} on {d}", secantus_pgplan::scalar::quote_identifier(&name)),
                            names: vec![d],
                            args: vec![name],
                        })
                    }
                    (rel, _) => on_relation(name, rel),
                }
            }),
            2604 => match row(format!(
                "select adrelid::int8, adnum::int8 from pg_catalog.pg_attrdef where oid = {objid}"
            )) {
                Some(r) => self.object_identity(1259, int(r.first()), int(r.get(1)))?.map(|c| Identity {
                    identity: format!("for {}", c.identity),
                    ..c
                }),
                None => None,
            },
            2612 => self.simple_identity("pg_language", "lanname", objid),
            2613 => row(format!(
                "select 1 from pg_catalog.pg_largeobject_metadata where oid = {objid}"
            ))
            .map(|_| Identity { identity: objid.to_string(), names: vec![objid.to_string()], args: Vec::new() }),
            2617 => row(format!(
                "select o.oprname::text, n.nspname::text, o.oprleft::int8, o.oprright::int8 \
                 from pg_catalog.pg_operator o join pg_catalog.pg_namespace n on n.oid = o.oprnamespace \
                 where o.oid = {objid}"
            ))
            .map(|r| {
                let (name, schema) = (text(r.first()), text(r.get(1)));
                let (l, rt) = (int(r.get(2)), int(r.get(3)));
                let side = |o: i64| if o == 0 { "NONE".to_string() } else { ty(o) };
                Identity {
                    identity: format!(
                        "{}.{name}({},{})",
                        secantus_pgplan::scalar::quote_identifier(&schema),
                        side(l),
                        side(rt)
                    ),
                    names: vec![schema, name],
                    args: [l, rt].into_iter().filter(|o| *o != 0).map(ty).collect(),
                }
            }),
            2616 => self.method_object_identity("pg_opclass", "opc", objid),
            2753 => self.method_object_identity("pg_opfamily", "opf", objid),
            2601 => self.simple_identity("pg_am", "amname", objid),
            2602 | 2603 => {
                let (catalog, p, word) = if classid == 2602 {
                    ("pg_amop", "amop", "operator")
                } else {
                    ("pg_amproc", "amproc", "function")
                };
                let num = if classid == 2602 { "amopstrategy" } else { "amprocnum" };
                row(format!(
                    "select {num}::int8, {p}lefttype::int8, {p}righttype::int8, {p}family::int8 \
                     from pg_catalog.{catalog} where oid = {objid}"
                ))
                .and_then(|r| {
                    let family = self.method_object_identity("pg_opfamily", "opf", int(r.get(3)))?;
                    let (l, rt) = (ty(int(r.get(1))), ty(int(r.get(2))));
                    Some(Identity {
                        identity: format!("{word} {} ({l}, {rt}) of {}", int(r.first()), family.identity),
                        names: family.names.into_iter().chain([int(r.first()).to_string()]).collect(),
                        args: vec![l, rt],
                    })
                })
            }
            2618 => row(format!(
                "select rulename::text, ev_class::int8 from pg_catalog.pg_rewrite where oid = {objid}"
            ))
            .and_then(|r| on_relation(text(r.first()), int(r.get(1)))),
            2620 => row(format!(
                "select tgname::text, tgrelid::int8 from pg_catalog.pg_trigger where oid = {objid}"
            ))
            .and_then(|r| on_relation(text(r.first()), int(r.get(1)))),
            3256 => row(format!(
                "select polname::text, polrelid::int8 from pg_catalog.pg_policy where oid = {objid}"
            ))
            .and_then(|r| on_relation(text(r.first()), int(r.get(1)))),
            2615 => self.simple_identity("pg_namespace", "nspname", objid),
            1260 => self.simple_identity("pg_roles", "rolname", objid),
            1262 => self.simple_identity("pg_database", "datname", objid),
            1213 => self.simple_identity("pg_tablespace", "spcname", objid),
            2328 => self.simple_identity("pg_foreign_data_wrapper", "fdwname", objid),
            1417 => self.simple_identity("pg_foreign_server", "srvname", objid),
            3079 => self.simple_identity("pg_extension", "extname", objid),
            3466 => self.simple_identity("pg_event_trigger", "evtname", objid),
            6104 => self.simple_identity("pg_publication", "pubname", objid),
            6100 => self.simple_identity("pg_subscription", "subname", objid),
            6243 => row(format!(
                "select parname::text from pg_catalog.pg_parameter_acl where oid = {objid}"
            ))
            .map(|r| {
                let n = text(r.first());
                Identity { identity: n.clone(), names: vec![n], args: Vec::new() }
            }),
            1261 => row(format!(
                "select member::int8, roleid::int8 from pg_catalog.pg_auth_members where oid = {objid}"
            ))
            .and_then(|r| {
                let (m, g) = (role(int(r.first()))?, role(int(r.get(1)))?);
                Some(Identity {
                    identity: format!("membership of role {m} in role {g}"),
                    names: Vec::new(),
                    args: Vec::new(),
                })
            }),
            1418 => row(format!(
                "select umuser::int8, s.srvname::text from pg_catalog.pg_user_mapping m \
                 join pg_catalog.pg_foreign_server s on s.oid = m.umserver where m.oid = {objid}"
            ))
            .and_then(|r| {
                let (user, server) = (role(int(r.first()))?, text(r.get(1)));
                Some(Identity {
                    identity: format!("{} on server {}", secantus_pgplan::scalar::quote_identifier(&user), secantus_pgplan::scalar::quote_identifier(&server)),
                    names: vec![user],
                    args: vec![server],
                })
            }),
            826 => row(format!(
                "select defaclrole::int8, defaclnamespace::int8, defaclobjtype::text \
                 from pg_catalog.pg_default_acl where oid = {objid}"
            ))
            .and_then(|r| {
                let who = role(int(r.first()))?;
                let schema = match int(r.get(1)) {
                    0 => None,
                    ns => row(format!("select nspname::text from pg_catalog.pg_namespace where oid = {ns}"))
                        .map(|n| text(n.first())),
                };
                let kind = text(r.get(2));
                let what = match kind.as_str() {
                    "r" => "tables",
                    "S" => "sequences",
                    "f" => "functions",
                    "T" => "types",
                    "n" => "schemas",
                    _ => "",
                };
                let q = secantus_pgplan::scalar::quote_identifier;
                let identity = match &schema {
                    Some(s) => format!("for role {} in schema {} on {what}", q(&who), q(s)),
                    None => format!("for role {} on {what}", q(&who)),
                };
                Some(Identity {
                    identity,
                    names: std::iter::once(who).chain(schema).collect(),
                    args: vec![kind],
                })
            }),
            6237 => row(format!(
                "select n.nspname::text, p.pubname::text from pg_catalog.pg_publication_namespace pn \
                 join pg_catalog.pg_namespace n on n.oid = pn.pnnspid \
                 join pg_catalog.pg_publication p on p.oid = pn.pnpubid where pn.oid = {objid}"
            ))
            .map(|r| {
                let (ns, pubname) = (text(r.first()), text(r.get(1)));
                let q = secantus_pgplan::scalar::quote_identifier;
                Identity {
                    identity: format!("{} in publication {}", q(&ns), q(&pubname)),
                    names: vec![pubname],
                    args: vec![ns],
                }
            }),
            6106 => row(format!(
                "select pr.prrelid::int8, p.pubname::text from pg_catalog.pg_publication_rel pr \
                 join pg_catalog.pg_publication p on p.oid = pr.prpubid where pr.oid = {objid}"
            ))
            .and_then(|r| {
                let rel = self.relation_identity(int(r.first()))?;
                let pubname = text(r.get(1));
                Some(Identity {
                    identity: format!("{} in publication {}", rel.identity, secantus_pgplan::scalar::quote_identifier(&pubname)),
                    names: rel.names,
                    args: vec![pubname],
                })
            }),
            3576 => row(format!(
                "select t.trftype::int8, l.lanname::text from pg_catalog.pg_transform t \
                 join pg_catalog.pg_language l on l.oid = t.trflang where t.oid = {objid}"
            ))
            .map(|r| {
                let (t, lang) = (ty(int(r.first())), text(r.get(1)));
                Identity {
                    identity: format!("for {t} on language {}", secantus_pgplan::scalar::quote_identifier(&lang)),
                    names: vec![t],
                    args: vec![lang],
                }
            }),
            _ => {
                return Err(Self::user_error(
                    "XX000",
                    format!("unrecognized object class: {classid}"),
                ))
            }
        })
    }
}

fn one_row(h: &PgHandler, catalog: &str, oid: i64) -> Option<Vec<Bson>> {
    h.object_row(&format!(
        "select 1 from pg_catalog.{catalog} where oid = {oid}"
    ))
}

impl PgHandler {
    /// The first oid `sql` answers, or the error PostgreSQL raises.
    fn address_oid(&self, sql: &str, code: &str, missing: String) -> PgWireResult<i64> {
        match self.object_row(sql) {
            Some(r) => Ok(int(r.first())),
            None => Err(Self::user_error(code, missing)),
        }
    }

    /// An object in a schema, named `[schema.]name`: the schema's, or the
    /// first on the search path that holds one.
    fn schema_object_oid(
        &self,
        catalog: &str,
        name_col: &str,
        ns_col: &str,
        names: &[String],
    ) -> Option<i64> {
        let lit = secantus_pgplan::scalar::quote_literal;
        let (schemas, name) = match names {
            [name] => (self.lookup_schemas(), name),
            [schema, name] => (vec![schema.clone()], name),
            _ => return None,
        };
        for s in schemas {
            if let Some(r) = self.object_row(&format!(
                "select o.oid::int8 from pg_catalog.{catalog} o join pg_catalog.pg_namespace n \
                 on n.oid = o.{ns_col} where o.{name_col}::text = {} and n.nspname = {}",
                lit(name),
                lit(&s)
            )) {
                return Some(int(r.first()));
            }
        }
        None
    }

    /// A relation named by a name list, as `makeRangeVarFromNameList` reads
    /// it: `(oid, relkind, the name as written)`.
    fn address_relation(&self, names: &[String]) -> PgWireResult<(i64, String, String)> {
        let q = secantus_pgplan::scalar::quote_identifier;
        let qualified = match names {
            [rel] => q(rel),
            [schema, rel] => format!("{}.{}", q(schema), q(rel)),
            [db, schema, rel] => {
                if *db != self.db() {
                    return Err(Self::user_error(
                        "0A000",
                        format!(
                            "cross-database references are not implemented: {}",
                            names.join(".")
                        ),
                    ));
                }
                format!("{}.{}", q(schema), q(rel))
            }
            _ => {
                return Err(Self::user_error(
                    "42601",
                    format!(
                        "improper relation name (too many dotted names): {}",
                        names.join(".")
                    ),
                ))
            }
        };
        let lit = secantus_pgplan::scalar::quote_literal(&qualified);
        let r = self.object_row(&format!(
            "select c.oid::int8, c.relkind::text from pg_catalog.pg_class c \
             where c.oid = pg_catalog.to_regclass({lit})"
        ));
        match r {
            Some(r) => Ok((
                int(r.first()),
                text(r.get(1)),
                names.last().cloned().unwrap_or_default(),
            )),
            None => Err(Self::user_error(
                "42P01",
                format!("relation \"{}\" does not exist", names.join(".")),
            )),
        }
    }

    /// `get_object_address(type, names, args)`: `(classid, objid, subid)`.
    pub(crate) fn object_address(
        &self,
        kind: &str,
        names: &[String],
        args: &[String],
    ) -> PgWireResult<(i64, i64, i64)> {
        let lit = secantus_pgplan::scalar::quote_literal;
        let exactly = |n: usize, list: &[String], what: &str| -> PgWireResult<()> {
            if list.len() == n {
                Ok(())
            } else {
                Err(Self::user_error(
                    "22023",
                    format!("{what} list length must be exactly {n}"),
                ))
            }
        };
        let none_args = || exactly(0, args, "argument");
        let relkind_word = |k: &str| match k {
            "table" => &["r", "p"][..],
            "index" => &["i", "I"][..],
            "sequence" => &["S"][..],
            "view" => &["v"][..],
            "materialized view" => &["m"][..],
            "foreign table" => &["f"][..],
            _ => &[][..],
        };
        let simple =
            |catalog: &str, col: &str, code: &str, word: &str| -> PgWireResult<(i64, i64, i64)> {
                exactly(1, names, "name")?;
                none_args()?;
                let classid = match catalog {
                    "pg_namespace" => 2615,
                    "pg_roles" => 1260,
                    "pg_database" => 1262,
                    "pg_tablespace" => 1213,
                    "pg_extension" => 3079,
                    "pg_language" => 2612,
                    "pg_am" => 2601,
                    "pg_event_trigger" => 3466,
                    "pg_foreign_data_wrapper" => 2328,
                    "pg_foreign_server" => 1417,
                    "pg_publication" => 6104,
                    _ => 6100,
                };
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.{catalog} where {col}::text = {}",
                        lit(&names[0])
                    ),
                    code,
                    format!("{word} \"{}\" does not exist", names[0]),
                )?;
                Ok((classid, oid, 0))
            };
        let in_schema = |classid: i64,
                         catalog: &str,
                         name_col: &str,
                         ns_col: &str,
                         word: &str|
         -> PgWireResult<(i64, i64, i64)> {
            none_args()?;
            match self.schema_object_oid(catalog, name_col, ns_col, names) {
                Some(oid) => Ok((classid, oid, 0)),
                None => Err(Self::user_error(
                    "42704",
                    format!("{word} \"{}\" does not exist", names.join(".")),
                )),
            }
        };
        match kind {
            "table" | "index" | "sequence" | "view" | "materialized view" | "foreign table" => {
                none_args()?;
                let (oid, relkind, name) = self.address_relation(names)?;
                if !relkind_word(kind).contains(&relkind.as_str()) {
                    let article = if kind == "index" { "an" } else { "a" };
                    return Err(Self::user_error(
                        "42809",
                        format!("\"{name}\" is not {article} {kind}"),
                    ));
                }
                Ok((1259, oid, 0))
            }
            "table column"
            | "view column"
            | "materialized view column"
            | "foreign table column"
            | "composite type column"
            | "toast table column" => {
                none_args()?;
                let Some((column, rel)) = names.split_last() else {
                    return Err(Self::user_error(
                        "42601",
                        "column name must be qualified".into(),
                    ));
                };
                if rel.is_empty() {
                    return Err(Self::user_error(
                        "42601",
                        "column name must be qualified".into(),
                    ));
                }
                let (oid, _, relname) = self.address_relation(rel)?;
                let att = self.address_oid(
                    &format!(
                        "select attnum::int8 from pg_catalog.pg_attribute where attrelid = {oid} \
                         and attname::text = {} and not attisdropped",
                        lit(column)
                    ),
                    "42703",
                    format!("column \"{column}\" of relation \"{relname}\" does not exist"),
                )?;
                Ok((1259, oid, att))
            }
            "type" | "domain" => {
                exactly(1, names, "name")?;
                none_args()?;
                let r = self.object_row(&format!(
                    "select t.oid::int8, t.typtype::text from pg_catalog.pg_type t \
                     where t.oid = pg_catalog.to_regtype({})",
                    lit(&names[0])
                ));
                let Some(r) = r else {
                    return Err(Self::user_error(
                        "42704",
                        format!("type \"{}\" does not exist", names[0]),
                    ));
                };
                if kind == "domain" && text(r.get(1)) != "d" {
                    return Err(Self::user_error(
                        "42809",
                        format!("\"{}\" is not a domain", names[0]),
                    ));
                }
                Ok((1247, int(r.first()), 0))
            }
            "function" | "aggregate" | "procedure" | "routine" => {
                let q = secantus_pgplan::scalar::quote_identifier;
                let qualified = names.iter().map(|n| q(n)).collect::<Vec<_>>().join(".");
                let signature = format!("{qualified}({})", args.join(","));
                let shown: Vec<String> = args
                    .iter()
                    .map(
                        |a| match self.object_address("type", std::slice::from_ref(a), &[]) {
                            Ok((_, oid, _)) => secantus_pgplan::regtype_text(oid),
                            Err(_) => a.clone(),
                        },
                    )
                    .collect();
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_proc where oid = pg_catalog.to_regprocedure({})",
                        lit(&signature)
                    ),
                    "42883",
                    format!(
                        "{} {}({}) does not exist",
                        if kind == "routine" { "function" } else { kind },
                        names.join("."),
                        shown.join(", ")
                    ),
                )?;
                Ok((1255, oid, 0))
            }
            "operator" => {
                exactly(2, args, "argument")?;
                let q = secantus_pgplan::scalar::quote_identifier;
                let (schema, name) = match names {
                    [name] => (None, name),
                    [schema, name] => (Some(schema), name),
                    _ => {
                        return Err(Self::user_error(
                            "42601",
                            format!(
                                "improper qualified name (too many dotted names): {}",
                                names.join(".")
                            ),
                        ))
                    }
                };
                let signature = format!(
                    "{}{name}({})",
                    schema.map(|s| format!("{}.", q(s))).unwrap_or_default(),
                    args.join(",")
                );
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_operator where oid = pg_catalog.to_regoperator({})",
                        lit(&signature)
                    ),
                    "42883",
                    format!("operator does not exist: {signature}"),
                )?;
                Ok((2617, oid, 0))
            }
            "schema" => simple("pg_namespace", "nspname", "3F000", "schema"),
            "role" => simple("pg_roles", "rolname", "42704", "role"),
            "database" => simple("pg_database", "datname", "3D000", "database"),
            "tablespace" => simple("pg_tablespace", "spcname", "42704", "tablespace"),
            "extension" => simple("pg_extension", "extname", "42704", "extension"),
            "language" => simple("pg_language", "lanname", "42704", "language"),
            "access method" => simple("pg_am", "amname", "42704", "access method"),
            "event trigger" => simple("pg_event_trigger", "evtname", "42704", "event trigger"),
            "foreign-data wrapper" => simple(
                "pg_foreign_data_wrapper",
                "fdwname",
                "42704",
                "foreign-data wrapper",
            ),
            "server" => simple("pg_foreign_server", "srvname", "42704", "server"),
            "publication" => simple("pg_publication", "pubname", "42704", "publication"),
            "subscription" => simple("pg_subscription", "subname", "42704", "subscription"),
            "collation" => in_schema(
                3456,
                "pg_collation",
                "collname",
                "collnamespace",
                "collation",
            ),
            "conversion" => in_schema(
                2607,
                "pg_conversion",
                "conname",
                "connamespace",
                "conversion",
            ),
            "statistics object" => in_schema(
                3381,
                "pg_statistic_ext",
                "stxname",
                "stxnamespace",
                "statistics object",
            ),
            "text search parser" => in_schema(
                3601,
                "pg_ts_parser",
                "prsname",
                "prsnamespace",
                "text search parser",
            ),
            "text search dictionary" => in_schema(
                3600,
                "pg_ts_dict",
                "dictname",
                "dictnamespace",
                "text search dictionary",
            ),
            "text search template" => in_schema(
                3764,
                "pg_ts_template",
                "tmplname",
                "tmplnamespace",
                "text search template",
            ),
            "text search configuration" => in_schema(
                3602,
                "pg_ts_config",
                "cfgname",
                "cfgnamespace",
                "text search configuration",
            ),
            "table constraint" | "rule" | "trigger" | "policy" => {
                none_args()?;
                let Some((object, rel)) = names.split_last().filter(|(_, r)| !r.is_empty()) else {
                    return Err(Self::user_error(
                        "42601",
                        "must specify relation and object name".into(),
                    ));
                };
                let (oid, _, relname) = self.address_relation(rel)?;
                let (classid, sql, missing) = match kind {
                    "table constraint" => (
                        2606,
                        format!("select oid::int8 from pg_catalog.pg_constraint where conrelid = {oid} and conname::text = {}", lit(object)),
                        format!("constraint \"{object}\" for table \"{relname}\" does not exist"),
                    ),
                    "rule" => (
                        2618,
                        format!("select oid::int8 from pg_catalog.pg_rewrite where ev_class = {oid} and rulename::text = {}", lit(object)),
                        format!("rule \"{object}\" for relation \"{relname}\" does not exist"),
                    ),
                    "trigger" => (
                        2620,
                        format!("select oid::int8 from pg_catalog.pg_trigger where tgrelid = {oid} and tgname::text = {}", lit(object)),
                        format!("trigger \"{object}\" for table \"{relname}\" does not exist"),
                    ),
                    _ => (
                        3256,
                        format!("select oid::int8 from pg_catalog.pg_policy where polrelid = {oid} and polname::text = {}", lit(object)),
                        format!("policy \"{object}\" for table \"{relname}\" does not exist"),
                    ),
                };
                Ok((classid, self.address_oid(&sql, "42704", missing)?, 0))
            }
            "domain constraint" => {
                exactly(1, names, "name")?;
                exactly(1, args, "argument")?;
                let (_, domain, _) = self.object_address("domain", names, &[])?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_constraint where contypid = {domain} and conname::text = {}",
                        lit(&args[0])
                    ),
                    "42704",
                    format!("constraint \"{}\" for domain {} does not exist", args[0], qualified_type_name(self, domain)),
                )?;
                Ok((2606, oid, 0))
            }
            "cast" => {
                exactly(1, names, "name")?;
                exactly(1, args, "argument")?;
                let (_, s, _) = self.object_address("type", names, &[])?;
                let (_, t, _) = self.object_address("type", &args[..1], &[])?;
                let oid = self.address_oid(
                    &format!("select oid::int8 from pg_catalog.pg_cast where castsource = {s} and casttarget = {t}"),
                    "42704",
                    format!(
                        "cast from type {} to type {} does not exist",
                        secantus_pgplan::regtype_text(s),
                        secantus_pgplan::regtype_text(t)
                    ),
                )?;
                Ok((2605, oid, 0))
            }
            "large object" => {
                exactly(1, names, "name")?;
                none_args()?;
                let n: i64 = names[0].trim().parse().map_err(|_| {
                    Self::user_error(
                        "22P02",
                        format!("invalid input syntax for type oid: \"{}\"", names[0]),
                    )
                })?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_largeobject_metadata where oid = {n}"
                    ),
                    "42704",
                    format!("large object {n} does not exist"),
                )?;
                Ok((2613, oid, 0))
            }
            "default value" => {
                none_args()?;
                let (_, oid, att) = self.object_address("table column", names, &[])?;
                let column = names.last().cloned().unwrap_or_default();
                let rel = names[..names.len() - 1].join(".");
                let def = self.address_oid(
                    &format!("select oid::int8 from pg_catalog.pg_attrdef where adrelid = {oid} and adnum = {att}"),
                    "42704",
                    format!("default value for column \"{column}\" of relation \"{rel}\" does not exist"),
                )?;
                Ok((2604, def, 0))
            }
            "operator class" | "operator family" => {
                none_args()?;
                let Some((am, rest)) = names.split_first().filter(|(_, r)| !r.is_empty()) else {
                    return Err(Self::user_error(
                        "22023",
                        "name list length must be at least 2".into(),
                    ));
                };
                let (catalog, p, classid) = if kind == "operator class" {
                    ("pg_opclass", "opc", 2616)
                } else {
                    ("pg_opfamily", "opf", 2753)
                };
                let method = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_am where amname::text = {}",
                        lit(am)
                    ),
                    "42704",
                    format!("access method \"{am}\" does not exist"),
                )?;
                let (schemas, name) = match rest {
                    [name] => (self.lookup_schemas(), name),
                    [schema, name] => (vec![schema.clone()], name),
                    _ => {
                        return Err(Self::user_error(
                            "42601",
                            format!(
                                "improper qualified name (too many dotted names): {}",
                                rest.join(".")
                            ),
                        ))
                    }
                };
                for s in schemas {
                    if let Some(r) = self.object_row(&format!(
                        "select o.oid::int8 from pg_catalog.{catalog} o join pg_catalog.pg_namespace n \
                         on n.oid = o.{p}namespace where o.{p}name::text = {} and n.nspname = {} \
                         and o.{p}method = {method}",
                        lit(name),
                        lit(&s)
                    )) {
                        return Ok((classid, int(r.first()), 0));
                    }
                }
                Err(Self::user_error(
                    "42704",
                    format!(
                        "{kind} \"{}\" does not exist for access method \"{am}\"",
                        rest.join(".")
                    ),
                ))
            }
            "user mapping" => {
                exactly(1, names, "name")?;
                exactly(1, args, "argument")?;
                let (_, server, _) = self.object_address("server", &args[..1], &[])?;
                let user = if names[0] == "public" {
                    0
                } else {
                    self.object_address("role", names, &[])?.1
                };
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_user_mapping where umuser = {user} \
                         and umserver = {server}"
                    ),
                    "42704",
                    format!(
                        "user mapping for user \"{}\" on server \"{}\" does not exist",
                        names[0], args[0]
                    ),
                )?;
                Ok((1418, oid, 0))
            }
            "transform" => {
                exactly(1, names, "name")?;
                exactly(1, args, "argument")?;
                let (_, ty, _) = self.object_address("type", names, &[])?;
                let (_, lang, _) = self.object_address("language", &args[..1], &[])?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_transform where trftype = {ty} and trflang = {lang}"
                    ),
                    "42704",
                    format!(
                        "transform for type {} language \"{}\" does not exist",
                        secantus_pgplan::regtype_text(ty),
                        args[0]
                    ),
                )?;
                Ok((3576, oid, 0))
            }
            "publication relation" => {
                exactly(1, args, "argument")?;
                let (_, rel, _) = self.object_address("table", names, &[])?;
                let (_, publication, _) = self.object_address("publication", &args[..1], &[])?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_publication_rel where prrelid = {rel} \
                         and prpubid = {publication}"
                    ),
                    "42704",
                    format!(
                        "publication relation \"{}\" in publication \"{}\" does not exist",
                        names.join("."),
                        args[0]
                    ),
                )?;
                Ok((6106, oid, 0))
            }
            "publication namespace" => {
                exactly(1, names, "name")?;
                exactly(1, args, "argument")?;
                let (_, publication, _) = self.object_address("publication", names, &[])?;
                let (_, ns, _) = self.object_address("schema", &args[..1], &[])?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_publication_namespace where pnnspid = {ns} \
                         and pnpubid = {publication}"
                    ),
                    "42704",
                    format!(
                        "publication schema \"{}\" in publication \"{}\" does not exist",
                        args[0], names[0]
                    ),
                )?;
                Ok((6237, oid, 0))
            }
            // `get_object_address_defacl`: `{role[, schema]}`, `{kind}`.
            "default acl" => {
                exactly(1, args, "argument")?;
                let (role, schema) = match names {
                    [role] => (role, None),
                    [role, schema] => (role, Some(schema)),
                    _ => {
                        return Err(Self::user_error(
                            "22023",
                            "name list length must be exactly 1 or 2".into(),
                        ))
                    }
                };
                let what = match args[0].as_str() {
                    "r" => "tables",
                    "S" => "sequences",
                    "f" => "functions",
                    "T" => "types",
                    "n" => "schemas",
                    other => {
                        let mut info = pgwire::error::ErrorInfo::new(
                            "ERROR".into(),
                            "22023".into(),
                            format!("unrecognized default ACL object type \"{other}\""),
                        );
                        info.hint = Some(
                            "Valid object types are \"r\", \"S\", \"f\", \"T\", \"n\".".into(),
                        );
                        return Err(pgwire::error::PgWireError::UserError(Box::new(info)));
                    }
                };
                let role_oid = self
                    .object_address("role", std::slice::from_ref(role), &[])?
                    .1;
                let ns = match schema {
                    Some(s) => {
                        self.object_address("schema", std::slice::from_ref(s), &[])?
                            .1
                    }
                    None => 0,
                };
                let missing = match schema {
                    Some(s) => format!("default ACL for user \"{role}\" in schema \"{s}\" on {what} does not exist"),
                    None => format!("default ACL for user \"{role}\" on {what} does not exist"),
                };
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_default_acl where defaclrole = {role_oid} \
                         and defaclnamespace = {ns} and defaclobjtype::text = {}",
                        lit(&args[0])
                    ),
                    "42704",
                    missing,
                )?;
                Ok((826, oid, 0))
            }
            // `get_object_address_opf_member`: the family's name list and
            // the member's number, `{left type, right type}`.
            "operator of access method" | "function of access method" => {
                exactly(2, args, "argument")?;
                let Some((number, family)) = names.split_last().filter(|(_, f)| f.len() >= 2)
                else {
                    return Err(Self::user_error(
                        "22023",
                        "name list length must be at least 3".into(),
                    ));
                };
                let (_, fam, _) = self.object_address("operator family", family, &[])?;
                let (_, l, _) = self.object_address("type", &args[..1], &[])?;
                let (_, r, _) = self.object_address("type", &args[1..], &[])?;
                let n: i64 = number.trim().parse().unwrap_or(0);
                let (catalog, p, num, word, classid) = if kind == "operator of access method" {
                    ("pg_amop", "amop", "amopstrategy", "operator", 2602)
                } else {
                    ("pg_amproc", "amproc", "amprocnum", "function", 2603)
                };
                let raw = |o: i64| -> String {
                    self.object_row(&format!(
                        "select n.nspname::text, t.typname::text from pg_catalog.pg_type t \
                         join pg_catalog.pg_namespace n on n.oid = t.typnamespace where t.oid = {o}"
                    ))
                    .map_or_else(String::new, |r| qualify(&text(r.first()), &text(r.get(1))))
                };
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.{catalog} where {p}family = {fam} \
                         and {p}lefttype = {l} and {p}righttype = {r} and {num} = {n}"
                    ),
                    "42704",
                    format!(
                        "{word} {n} ({}, {}) of {} does not exist",
                        raw(l),
                        raw(r),
                        self.opfamily_description_of(fam).unwrap_or_default()
                    ),
                )?;
                Ok((classid, oid, 0))
            }
            "parameter ACL" => {
                exactly(1, names, "name")?;
                none_args()?;
                let oid = self.address_oid(
                    &format!(
                        "select oid::int8 from pg_catalog.pg_parameter_acl where parname::text = {}",
                        lit(&names[0].to_lowercase())
                    ),
                    "42704",
                    format!("parameter ACL \"{}\" does not exist", names[0]),
                )?;
                Ok((6243, oid, 0))
            }
            _ => Err(Self::user_error(
                "22023",
                format!("unrecognized object type \"{kind}\""),
            )),
        }
    }
}

/// A record of `fields`, named and typed as a function's OUT parameters.
fn out_record(fields: Vec<Bson>, names: &[&str], types: &[&str]) -> Bson {
    let strings = |v: &[&str]| Bson::Array(v.iter().map(|s| Bson::String((*s).into())).collect());
    let mut d = bson::Document::new();
    d.insert(secantus_pgplan::RECORD_KEY, Bson::Array(fields));
    d.insert(secantus_pgplan::RECORD_TYPES_KEY, strings(types));
    d.insert(secantus_pgplan::RECORD_NAMES_KEY, strings(names));
    Bson::Document(d)
}

fn text_array(items: Vec<String>) -> Bson {
    Bson::Array(items.into_iter().map(Bson::String).collect())
}

impl PgHandler {
    /// `pg_identify_object(classid, objid, objsubid)`: `(type, schema,
    /// name, identity)`, the last three NULL for an object that does not
    /// exist.
    pub(crate) fn identify_object_call(&self, args: &[Bson]) -> PgWireResult<Bson> {
        if args.iter().any(|a| matches!(a, Bson::Null)) {
            return Ok(Bson::Null);
        }
        let (classid, objid, subid) = (
            self.oid_arg(args.first()),
            self.oid_arg(args.get(1)),
            int(args.get(2)),
        );
        let word = self.object_type_word(classid, objid, subid)?;
        let identity = self.object_identity(classid, objid, subid)?;
        let (mut schema, mut name) = (Bson::Null, Bson::Null);
        if identity.is_some() {
            let (name_col, ns_col, catalog) = identify_columns(classid);
            let cols: Vec<String> = name_col
                .map(|c| format!("o.{c}::text"))
                .into_iter()
                .chain(ns_col.map(|c| {
                    format!("(select nspname::text from pg_catalog.pg_namespace where oid = o.{c})")
                }))
                .collect();
            if !cols.is_empty() {
                if let Some(r) = self.object_row(&format!(
                    "select {} from pg_catalog.{catalog} o where o.oid = {objid}",
                    cols.join(", ")
                )) {
                    let mut it = r.into_iter();
                    if name_col.is_some() {
                        name = Bson::String(secantus_pgplan::scalar::quote_identifier(&text(
                            it.next().as_ref(),
                        )));
                    }
                    if ns_col.is_some() {
                        schema = Bson::String(text(it.next().as_ref()));
                    }
                }
            }
        }
        let identity = identity.map_or(Bson::Null, |i| Bson::String(i.identity));
        Ok(out_record(
            vec![Bson::String(word), schema, name, identity],
            &["type", "schema", "name", "identity"],
            &["text", "text", "text", "text"],
        ))
    }

    /// `pg_identify_object_as_address(classid, objid, objsubid)`: `(type,
    /// object_names, object_args)`.
    pub(crate) fn identify_object_as_address_call(&self, args: &[Bson]) -> PgWireResult<Bson> {
        if args.iter().any(|a| matches!(a, Bson::Null)) {
            return Ok(Bson::Null);
        }
        let (classid, objid, subid) = (
            self.oid_arg(args.first()),
            self.oid_arg(args.get(1)),
            int(args.get(2)),
        );
        let word = self.object_type_word(classid, objid, subid)?;
        let (names, object_args) = match self.object_identity(classid, objid, subid)? {
            Some(i) => (text_array(i.names), text_array(i.args)),
            None => (Bson::Null, Bson::Null),
        };
        Ok(out_record(
            vec![Bson::String(word), names, object_args],
            &["type", "object_names", "object_args"],
            &["text", "text[]", "text[]"],
        ))
    }

    /// `pg_get_object_address(type, object_names, object_args)`:
    /// `(classid, objid, objsubid)`.
    pub(crate) fn object_address_call(&self, args: &[Bson]) -> PgWireResult<Bson> {
        if args.iter().any(|a| matches!(a, Bson::Null)) {
            return Ok(Bson::Null);
        }
        let list = |v: Option<&Bson>| -> PgWireResult<Vec<String>> {
            let v = match v {
                Some(Bson::String(s)) => secantus_pgplan::cast_text_to(
                    s,
                    "text[]",
                    &secantus_pgplan::TimeZoneSetting::default(),
                )
                .map_err(|e| Self::err(&e))?,
                Some(other) => other.clone(),
                None => Bson::Array(Vec::new()),
            };
            match v {
                Bson::Array(items) => items
                    .iter()
                    .map(|i| match i {
                        Bson::Null => Err(Self::user_error(
                            "22004",
                            "name or argument lists may not contain nulls".into(),
                        )),
                        other => Ok(text(Some(other))),
                    })
                    .collect(),
                _ => Ok(Vec::new()),
            }
        };
        let kind = text(args.first());
        let (classid, objid, subid) =
            self.object_address(&kind, &list(args.get(1))?, &list(args.get(2))?)?;
        Ok(out_record(
            vec![
                Bson::Int64(classid),
                Bson::Int64(objid),
                Bson::Int32(subid as i32),
            ],
            &["classid", "objid", "objsubid"],
            &["oid", "oid", "int4"],
        ))
    }
}
