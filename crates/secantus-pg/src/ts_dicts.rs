//! `CREATE / ALTER / DROP TEXT SEARCH DICTIONARY` (RP-169), kept as
//! PostgreSQL keeps a dictionary: a `pg_ts_dict` row (name, schema, owner,
//! template, options). One record per dictionary in the database's catalog
//! (`__sql_ts_dicts__`), installed for the planner per statement
//! (`ts_catalog::set_user_dicts`), which serves `regdictionary`, the
//! catalog and every text-search function from it. A built-in dictionary an
//! ALTER changes is kept the same way under its own oid, hiding the dumped
//! row.

use secantus_pgplan::ts_catalog::{self, UserDict};
use secantus_pgplan::ts_configs::{QualifiedName, TsConfigOp};
use secantus_pgplan::ts_dicts::{self, TsDictOp};

use super::*;

pub(crate) const TS_DICT_COLLECTION: &str = "__sql_ts_dicts__";

/// `pg_ts_dict`'s oid, the class `pg_description` keys a comment by.
pub(crate) const PG_TS_DICT: i64 = 3600;

/// The first oid `initdb` gives an object it does not pin
/// (`FirstGenbkiObjectId`): a dictionary below it is the bootstrap's own.
const FIRST_UNPINNED_OID: i64 = 10000;

fn written(name: &QualifiedName) -> String {
    name.join(".")
}

fn quoted(name: &QualifiedName) -> String {
    name.iter()
        .map(|p| secantus_pgplan::scalar::quote_identifier(p))
        .collect::<Vec<_>>()
        .join(".")
}

fn key_of(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

fn dict_of_doc(d: &Document) -> Option<UserDict> {
    Some(UserDict {
        oid: d.get("oid").and_then(bson_i64)?,
        schema: d.get_str("schema").ok()?.to_string(),
        name: d.get_str("name").ok()?.to_string(),
        owner: d.get_str("owner").unwrap_or_default().to_string(),
        template: d.get("template").and_then(bson_i64)?,
        options: d
            .get_array("options")
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        let o = o.as_array()?;
                        Some((
                            o.first()?.as_str()?.to_string(),
                            o.get(1)?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        numeric: d
            .get_array("numeric")
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

impl PgHandler {
    /// The database's dictionaries.
    pub(crate) fn user_ts_dicts(&self) -> Vec<UserDict> {
        self.type_catalog_docs(TS_DICT_COLLECTION)
            .map(|docs| docs.iter().filter_map(dict_of_doc).collect())
            .unwrap_or_default()
    }

    fn ts_dict_doc(&self, oid: i64) -> Option<Document> {
        self.type_catalog_docs(TS_DICT_COLLECTION)
            .ok()?
            .iter()
            .find(|d| d.get("oid").and_then(bson_i64) == Some(oid))
            .cloned()
    }

    fn write_ts_dict(&self, d: &UserDict, comment: Option<&str>) -> PgWireResult<()> {
        let key = key_of(&d.schema, &d.name);
        let options: Vec<Bson> = d
            .options
            .iter()
            .map(|(k, v)| Bson::Array(vec![Bson::String(k.clone()), Bson::String(v.clone())]))
            .collect();
        let mut doc = bson::doc! {
            "_id": &key,
            "schema": &d.schema,
            "name": &d.name,
            "owner": &d.owner,
            "oid": d.oid,
            "template": d.template,
            "options": options,
            "numeric": d.numeric.clone(),
        };
        if let Some(text) = comment {
            doc.insert("comment", text);
        }
        self.put(TS_DICT_COLLECTION, &key, doc)?;
        self.publish_ts_dicts();
        Ok(())
    }

    /// The rows the planner reads, after a change this statement made.
    fn publish_ts_dicts(&self) {
        ts_catalog::set_user_dicts(self.user_ts_dicts());
    }

    /// The dictionary `name` names, or 42704.
    fn resolve_ts_dict(&self, name: &QualifiedName) -> PgWireResult<i64> {
        self.publish_ts_dicts();
        if let [schema, _] = name.as_slice() {
            self.require_schema(schema)?;
        }
        ts_catalog::oid_of(&quoted(name), true).ok_or_else(|| {
            Self::user_error(
                "42704",
                format!(
                    "text search dictionary \"{}\" does not exist",
                    written(name)
                ),
            )
        })
    }

    /// A dictionary a statement changes, which the role owns (or a
    /// superuser): a built-in one as its dumped row, to be kept as the
    /// database's own.
    fn changeable_ts_dict(&self, name: &QualifiedName) -> PgWireResult<UserDict> {
        let oid = self.resolve_ts_dict(name)?;
        let d = match self.user_ts_dicts().into_iter().find(|d| d.oid == oid) {
            Some(d) => d,
            None => {
                let (bare, template, options) = ts_catalog::builtin_dict(oid).ok_or_else(|| {
                    Self::user_error(
                        "42704",
                        format!(
                            "text search dictionary \"{}\" does not exist",
                            written(name)
                        ),
                    )
                })?;
                UserDict {
                    oid,
                    schema: "pg_catalog".into(),
                    name: bare,
                    owner: Self::bootstrap_role_name(),
                    template,
                    options,
                    numeric: Vec::new(),
                }
            }
        };
        let me = self.current_role_name();
        if !self.is_superuser(&me) && !self.role_is_member(&me, &d.owner) {
            return Err(Self::user_error(
                "42501",
                format!("must be owner of text search dictionary {}", written(name)),
            ));
        }
        Ok(d)
    }

    /// 23505 on `pg_ts_dict_dictname_index`, unless the name is free.
    fn free_ts_dict_name(&self, schema: &str, name: &str) -> PgWireResult<()> {
        if ts_catalog::oid_in(schema, name, true).is_none() {
            return Ok(());
        }
        let ns = self.namespace_oid(schema);
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "23505".into(),
            "duplicate key value violates unique constraint \"pg_ts_dict_dictname_index\"".into(),
        );
        info.detail = Some(format!(
            "Key (dictname, dictnamespace)=({name}, {ns}) already exists."
        ));
        Err(PgWireError::UserError(Box::new(info)))
    }

    /// A template's `init` over `options`, its error as PostgreSQL's.
    fn check_ts_dict_options(template: i64, options: &[(String, String)]) -> PgWireResult<()> {
        ts_dicts::init(template, options)
            .map(|_| ())
            .map_err(|e| Self::err(&e))
    }

    /// The configurations whose map offers a token to dictionary `oid`:
    /// `(configuration oid, a user one's name)`, in oid order.
    fn ts_dict_dependents(&self, oid: i64) -> Vec<(i64, Option<QualifiedName>)> {
        let mut out: Vec<(i64, Option<QualifiedName>)> = Vec::new();
        for c in self.user_ts_configs() {
            if c.map.iter().any(|(_, _, d)| *d == oid) {
                out.push((c.oid, Some(vec![c.schema.clone(), c.name.clone()])));
            }
        }
        if let Some(cat) = ts_catalog::catalog("pg_ts_config_map") {
            let at = |c: &str| cat.columns.iter().position(|x| *x == c);
            if let (Some(cfg), Some(dict)) = (at("mapcfg"), at("mapdict")) {
                for row in &cat.rows {
                    let (Some(c), Some(d)) = (
                        row.get(cfg)
                            .copied()
                            .flatten()
                            .and_then(|v| v.parse::<i64>().ok()),
                        row.get(dict)
                            .copied()
                            .flatten()
                            .and_then(|v| v.parse::<i64>().ok()),
                    ) else {
                        continue;
                    };
                    if d == oid
                        && !ts_catalog::builtin_hidden(c)
                        && !out.iter().any(|(o, _)| *o == c)
                    {
                        out.push((c, None));
                    }
                }
            }
        }
        out.sort_by_key(|(o, _)| *o);
        out
    }

    pub(crate) fn ts_dict_statement(&self, op: TsDictOp) -> PgWireResult<Vec<Response>> {
        let tag = |t: &str| Ok(vec![Response::Execution(Tag::new(t))]);
        match op {
            TsDictOp::Create {
                name,
                options,
                numeric,
            } => {
                let (schema, bare) = match name.as_slice() {
                    [bare] => (
                        secantus_pgplan::schemas::creation_schema()
                            .map_err(|e| Self::user_error("3F000", e.to_string()))?,
                        bare.clone(),
                    ),
                    [schema, bare] => {
                        self.require_schema(schema)?;
                        (schema.clone(), bare.clone())
                    }
                    _ => {
                        return Err(Self::user_error(
                            "42601",
                            format!(
                                "improper qualified name (too many dotted names): {}",
                                written(&name)
                            ),
                        ))
                    }
                };
                self.publish_ts_dicts();
                // DefineTSDictionary: the template apart, the rest its
                // options.
                let mut template: Option<i64> = None;
                let mut rest: Vec<(String, String)> = Vec::new();
                for (k, v) in options {
                    if k == "template" {
                        let parts: Vec<String> = v
                            .clone()
                            .unwrap_or_default()
                            .split('.')
                            .map(str::to_string)
                            .collect();
                        template = Some(ts_dicts::template_oid(&parts).ok_or_else(|| {
                            Self::user_error(
                                "42704",
                                format!(
                                    "text search template \"{}\" does not exist",
                                    v.unwrap_or_default()
                                ),
                            )
                        })?);
                    } else {
                        rest.push((k, v.unwrap_or_default()));
                    }
                }
                let template = template.ok_or_else(|| {
                    Self::user_error("42P17", "text search template is required".into())
                })?;
                Self::check_ts_dict_options(template, &rest)?;
                self.free_ts_dict_name(&schema, &bare)?;
                let dict = UserDict {
                    oid: self.mint_composite_oid()?,
                    schema,
                    name: bare,
                    owner: self.current_role_name(),
                    template,
                    numeric: numeric
                        .into_iter()
                        .filter(|n| rest.iter().any(|(k, _)| k == n))
                        .collect(),
                    options: rest,
                };
                self.write_ts_dict(&dict, None)?;
                tag("CREATE TEXT SEARCH DICTIONARY")
            }
            TsDictOp::Alter {
                name,
                options,
                numeric,
            } => {
                let mut d = self.changeable_ts_dict(&name)?;
                let comment = self.ts_dict_comment(d.oid);
                // AlterTSDictionary: each option named replaces the one it
                // names, moved to the end; one with no value is removed.
                for (k, v) in options {
                    d.options.retain(|(o, _)| *o != k);
                    d.numeric.retain(|o| *o != k);
                    if let Some(v) = v {
                        if numeric.contains(&k) {
                            d.numeric.push(k.clone());
                        }
                        d.options.push((k, v));
                    }
                }
                Self::check_ts_dict_options(d.template, &d.options)?;
                self.write_ts_dict(&d, comment.as_deref())?;
                tag("ALTER TEXT SEARCH DICTIONARY")
            }
            TsDictOp::Rename { name, to } => {
                let d = self.changeable_ts_dict(&name)?;
                if ts_catalog::oid_in(&d.schema, &to, true).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!(
                            "text search dictionary \"{to}\" already exists in schema \"{}\"",
                            d.schema
                        ),
                    ));
                }
                let comment = self.ts_dict_comment(d.oid);
                self.delete_type_doc(TS_DICT_COLLECTION, &key_of(&d.schema, &d.name))?;
                self.write_ts_dict(&UserDict { name: to, ..d }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH DICTIONARY")
            }
            TsDictOp::SetSchema { name, schema } => {
                let d = self.changeable_ts_dict(&name)?;
                self.require_schema(&schema)?;
                if ts_catalog::oid_in(&schema, &d.name, true).is_some_and(|o| o != d.oid) {
                    return Err(Self::user_error(
                        "42710",
                        format!(
                            "text search dictionary \"{}\" already exists in schema \"{schema}\"",
                            d.name
                        ),
                    ));
                }
                let comment = self.ts_dict_comment(d.oid);
                self.delete_type_doc(TS_DICT_COLLECTION, &key_of(&d.schema, &d.name))?;
                self.write_ts_dict(&UserDict { schema, ..d }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH DICTIONARY")
            }
            TsDictOp::Owner { name, owner } => {
                let d = self.changeable_ts_dict(&name)?;
                let owner = self.grantee_name(&owner)?;
                if self.role(&owner)?.is_none() && owner != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{owner}\" does not exist"),
                    ));
                }
                let comment = self.ts_dict_comment(d.oid);
                self.write_ts_dict(&UserDict { owner, ..d }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH DICTIONARY")
            }
            TsDictOp::Drop {
                names,
                if_exists,
                cascade,
            } => {
                self.publish_ts_dicts();
                let mut targets: Vec<(QualifiedName, i64)> = Vec::new();
                for name in names {
                    if let [schema, _] = name.as_slice() {
                        if !secantus_pgplan::schemas::schema_exists(schema)
                            && schema != "pg_catalog"
                        {
                            if if_exists {
                                self.notice(
                                    "00000",
                                    format!("schema \"{schema}\" does not exist, skipping"),
                                    None,
                                );
                                continue;
                            }
                            self.require_schema(schema)?;
                        }
                    }
                    match ts_catalog::oid_of(&quoted(&name), true) {
                        Some(oid) => targets.push((name, oid)),
                        None if if_exists => self.notice(
                            "00000",
                            format!(
                                "text search dictionary \"{}\" does not exist, skipping",
                                written(&name)
                            ),
                            None,
                        ),
                        None => {
                            return Err(Self::user_error(
                                "42704",
                                format!(
                                    "text search dictionary \"{}\" does not exist",
                                    written(&name)
                                ),
                            ))
                        }
                    }
                }
                for (name, oid) in &targets {
                    let d = self.changeable_ts_dict(name)?;
                    let shown =
                        ts_catalog::display_of(*oid, true).unwrap_or_else(|| d.name.clone());
                    if *oid < FIRST_UNPINNED_OID {
                        return Err(Self::user_error(
                            "2BP01",
                            format!(
                                "cannot drop text search dictionary {shown} because it is required by the database system"
                            ),
                        ));
                    }
                    let dependents = self.ts_dict_dependents(*oid);
                    let describe = |c: i64| {
                        format!(
                            "text search configuration {}",
                            ts_catalog::display_of(c, false).unwrap_or_default()
                        )
                    };
                    if !dependents.is_empty() && !cascade {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "2BP01".into(),
                            format!(
                                "cannot drop text search dictionary {shown} because other objects depend on it"
                            ),
                        );
                        info.detail = Some(
                            dependents
                                .iter()
                                .map(|(c, _)| {
                                    format!(
                                        "{} depends on text search dictionary {shown}",
                                        describe(*c)
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                        info.hint =
                            Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    if dependents.iter().any(|(_, user)| user.is_none()) {
                        return Err(Self::user_error(
                            "0A000",
                            format!(
                                "dropping text search dictionary {shown} with the built-in configurations that use it is not supported"
                            ),
                        ));
                    }
                    if !dependents.is_empty() {
                        let lines: Vec<String> =
                            dependents.iter().map(|(c, _)| describe(*c)).collect();
                        let msg = if lines.len() == 1 {
                            format!("drop cascades to {}", lines[0])
                        } else {
                            format!("drop cascades to {} other objects", lines.len())
                        };
                        let detail = (lines.len() > 1).then(|| {
                            lines
                                .iter()
                                .map(|l| format!("drop cascades to {l}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        });
                        self.notice("00000", msg, detail);
                        let configs: Vec<QualifiedName> =
                            dependents.into_iter().filter_map(|(_, n)| n).collect();
                        self.ts_config_statement(TsConfigOp::Drop {
                            names: configs,
                            if_exists: true,
                            cascade: true,
                        })?;
                    }
                    if self.user_ts_dicts().iter().any(|u| u.oid == d.oid) {
                        self.delete_type_doc(TS_DICT_COLLECTION, &key_of(&d.schema, &d.name))?;
                    }
                }
                self.publish_ts_dicts();
                tag("DROP TEXT SEARCH DICTIONARY")
            }
        }
    }

    fn ts_dict_comment(&self, oid: i64) -> Option<String> {
        self.ts_dict_doc(oid)
            .and_then(|d| d.get_str("comment").ok().map(str::to_string))
    }

    /// `COMMENT ON TEXT SEARCH DICTIONARY`.
    pub(crate) fn comment_on_ts_dict(
        &self,
        names: &[String],
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let d = self.changeable_ts_dict(&names.to_vec())?;
        self.write_ts_dict(&d, comment.as_deref())
    }

    /// `(oid, comment)` of each dictionary with one.
    pub(crate) fn ts_dict_comments(&self) -> Vec<(i64, String)> {
        self.type_catalog_docs(TS_DICT_COLLECTION)
            .map(|docs| {
                docs.iter()
                    .filter_map(|d| {
                        Some((
                            d.get("oid").and_then(bson_i64)?,
                            d.get_str("comment").ok()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The `pg_ts_dict` rows of the database's dictionaries.
pub(crate) fn dict_rows(
    def: &TableDef,
    dicts: &[UserDict],
    role_oid: impl Fn(&str) -> i64,
) -> Vec<Document> {
    let f = |c: &str| def.field_of(c);
    dicts
        .iter()
        .map(|d| {
            let ns = if d.schema == "pg_catalog" {
                11
            } else {
                secantus_pgplan::regobj::resolve("regnamespace", &d.schema).unwrap_or(0)
            };
            let mut doc = Document::new();
            for (column, value) in [
                ("oid", Bson::Int64(d.oid)),
                ("dictname", Bson::String(d.name.clone())),
                ("dictnamespace", Bson::Int64(ns)),
                ("dictowner", Bson::Int64(role_oid(&d.owner))),
                ("dicttemplate", Bson::Int64(d.template)),
                (
                    "dictinitoption",
                    ts_dicts::options_text(&d.options, &d.numeric).map_or(Bson::Null, Bson::String),
                ),
            ] {
                if let Some(field) = f(column) {
                    doc.insert(field, value);
                }
            }
            doc
        })
        .collect()
}
