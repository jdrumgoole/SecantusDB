//! `pg_describe_object(classid, objid, objsubid)` (RP-199): an object's
//! description, as PostgreSQL 15.19's `getObjectDescription` writes it --
//! `table pg_class`, `column relname of table pg_class`, `operator class c
//! for access method btree`. Each catalog's row is read through this
//! server's own catalogs, so the description names what `pg_class`,
//! `pg_proc` and the rest list. An object that does not exist is NULL, as
//! with PostgreSQL's `missing_ok`; a class that is no catalog is XX000.

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
        let regclass = |name: &str| {
            self.object_row(&format!(
                "select {}::regclass::oid::int8",
                secantus_pgplan::scalar::quote_literal(name)
            ))
            .map_or(0, |r| int(r.first()))
        };
        let classid = match args.first() {
            Some(Bson::String(name)) => regclass(name),
            other => int(other),
        };
        // A `regclass` objid arrives as its name too.
        let objid = match args.get(1) {
            Some(Bson::String(name)) => regclass(name),
            other => int(other),
        };
        let subid = int(args.get(2));
        Ok(self
            .describe_object(classid, objid, subid)?
            .map_or(Bson::Null, Bson::String))
    }
}
