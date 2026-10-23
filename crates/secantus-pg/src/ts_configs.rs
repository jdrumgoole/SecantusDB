//! `CREATE / ALTER / DROP TEXT SEARCH CONFIGURATION`, kept as PostgreSQL
//! keeps a configuration: a `pg_ts_config` row (name, schema, owner,
//! parser) and its `pg_ts_config_map` rows (token type, seqno, dictionary).
//! One record per configuration in the database's catalog
//! (`__sql_ts_configs__`), installed for the planner per statement
//! (`ts_catalog::set_user_configs`), which serves `regconfig`, the catalogs
//! and every text-search function from it.
//!
//! The built-in configurations are `initdb`'s and fixed here: altering or
//! dropping one is refused by name (RP-165).

use pg_query::protobuf::node::Node as N;
use secantus_pgplan::ts_catalog::{self, UserConfig};
use secantus_pgplan::ts_configs::{MapAction, QualifiedName, TsConfigOp, TsSource};

use super::*;

pub(crate) const TS_CONFIG_COLLECTION: &str = "__sql_ts_configs__";

/// `pg_ts_config`'s oid, the class `pg_description` keys a comment by.
pub(crate) const PG_TS_CONFIG: i64 = 3602;

/// The functions whose leading `regconfig` argument a constant names: the
/// configuration an expression depends on (`find_expr_references` on the
/// constant).
const CONFIG_FUNCTIONS: [&str; 9] = [
    "to_tsvector",
    "to_tsquery",
    "plainto_tsquery",
    "phraseto_tsquery",
    "websearch_to_tsquery",
    "ts_headline",
    "json_to_tsvector",
    "jsonb_to_tsvector",
    "ts_debug",
];

/// A name as written, for a message: its parts joined.
fn written(name: &QualifiedName) -> String {
    name.join(".")
}

/// `name` as text `ts_catalog::oid_of` reads.
fn quoted(name: &QualifiedName) -> String {
    name.iter()
        .map(|p| secantus_pgplan::scalar::quote_identifier(p))
        .collect::<Vec<_>>()
        .join(".")
}

fn key_of(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

/// A configuration's description in a dependency message.
fn described(c: &UserConfig) -> String {
    let shown = ts_catalog::display_of(c.oid, false).unwrap_or_else(|| c.name.clone());
    format!("text search configuration {shown}")
}

impl PgHandler {
    /// The database's configurations.
    pub(crate) fn user_ts_configs(&self) -> Vec<UserConfig> {
        self.type_catalog_docs(TS_CONFIG_COLLECTION)
            .map(|docs| docs.iter().filter_map(config_of_doc).collect())
            .unwrap_or_default()
    }

    fn ts_config_doc(&self, oid: i64) -> Option<Document> {
        self.type_catalog_docs(TS_CONFIG_COLLECTION)
            .ok()?
            .iter()
            .find(|d| d.get("oid").and_then(bson_i64) == Some(oid))
            .cloned()
    }

    fn write_ts_config(&self, c: &UserConfig, comment: Option<&str>) -> PgWireResult<()> {
        let map: Vec<Bson> = c
            .map
            .iter()
            .map(|(t, s, d)| Bson::Array(vec![Bson::Int32(*t), Bson::Int32(*s), Bson::Int64(*d)]))
            .collect();
        let key = key_of(&c.schema, &c.name);
        let mut doc = bson::doc! {
            "_id": &key,
            "schema": &c.schema,
            "name": &c.name,
            "owner": &c.owner,
            "oid": c.oid,
            "parser": c.parser,
            "map": map,
        };
        if let Some(text) = comment {
            doc.insert("comment", text);
        }
        self.put(TS_CONFIG_COLLECTION, &key, doc)?;
        self.publish_ts_configs();
        Ok(())
    }

    /// The rows the planner reads, after a change this statement made.
    fn publish_ts_configs(&self) {
        ts_catalog::set_user_configs(self.user_ts_configs());
    }

    /// The configuration `name` names, or 42704.
    fn resolve_ts_config(&self, name: &QualifiedName) -> PgWireResult<i64> {
        self.publish_ts_configs();
        if let [schema, _] = name.as_slice() {
            self.require_schema(schema)?;
        }
        ts_catalog::oid_of(&quoted(name), false).ok_or_else(|| {
            Self::user_error(
                "42704",
                format!(
                    "text search configuration \"{}\" does not exist",
                    written(name)
                ),
            )
        })
    }

    pub(crate) fn require_schema(&self, schema: &str) -> PgWireResult<()> {
        if schema == "pg_catalog" || secantus_pgplan::schemas::schema_exists(schema) {
            return Ok(());
        }
        Err(Self::user_error(
            "3F000",
            format!("schema \"{schema}\" does not exist"),
        ))
    }

    /// A configuration a statement changes: a user one, which the role
    /// owns (or a superuser).
    fn changeable_ts_config(&self, name: &QualifiedName) -> PgWireResult<UserConfig> {
        let oid = self.resolve_ts_config(name)?;
        let Some(c) = self.user_ts_configs().into_iter().find(|c| c.oid == oid) else {
            return Err(Self::user_error(
                "0A000",
                "changing a built-in text search configuration is not supported".into(),
            ));
        };
        let me = self.current_role_name();
        if !self.is_superuser(&me) && !self.role_is_member(&me, &c.owner) {
            return Err(Self::user_error(
                "42501",
                format!(
                    "must be owner of text search configuration {}",
                    written(name)
                ),
            ));
        }
        Ok(c)
    }

    /// A schema and name for a new configuration, unless one exists there:
    /// 23505 on `pg_ts_config_cfgname_index`, as PostgreSQL's insert says.
    fn free_ts_config_name(&self, schema: &str, name: &str) -> PgWireResult<()> {
        if ts_catalog::oid_in(schema, name, false).is_none() {
            return Ok(());
        }
        let ns = self.namespace_oid(schema);
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "23505".into(),
            "duplicate key value violates unique constraint \"pg_ts_config_cfgname_index\"".into(),
        );
        info.detail = Some(format!(
            "Key (cfgname, cfgnamespace)=({name}, {ns}) already exists."
        ));
        Err(PgWireError::UserError(Box::new(info)))
    }

    pub(crate) fn namespace_oid(&self, schema: &str) -> i64 {
        if schema == "pg_catalog" {
            return 11;
        }
        self.namespaces()
            .into_iter()
            .find(|(n, _)| n == schema)
            .map_or(0, |(_, o)| o)
    }

    /// The token types of the default parser named, by number, or 22023.
    fn token_types(tokens: &[String]) -> PgWireResult<Vec<i32>> {
        tokens
            .iter()
            .map(|t| {
                secantus_pgplan::wparser::TOKEN_TYPES
                    .iter()
                    .position(|(alias, _)| alias == t)
                    .map(|i| i as i32 + 1)
                    .ok_or_else(|| {
                        Self::user_error("22023", format!("token type \"{t}\" does not exist"))
                    })
            })
            .collect()
    }

    fn dictionary(name: &QualifiedName) -> PgWireResult<i64> {
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

    pub(crate) fn ts_config_statement(&self, op: TsConfigOp) -> PgWireResult<Vec<Response>> {
        let tag = |t: &str| Ok(vec![Response::Execution(Tag::new(t))]);
        match op {
            TsConfigOp::Create { name, source } => {
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
                self.publish_ts_configs();
                let (parser, map) = match source {
                    TsSource::Parser(p) => {
                        let default = match p.as_slice() {
                            [n] => n == "default",
                            [s, n] => s == "pg_catalog" && n == "default",
                            _ => false,
                        };
                        if !default {
                            return Err(Self::user_error(
                                "42704",
                                format!("text search parser \"{}\" does not exist", written(&p)),
                            ));
                        }
                        (ts_catalog::DEFAULT_PARSER, Vec::new())
                    }
                    TsSource::Copy(c) => {
                        let oid = self.resolve_ts_config(&c)?;
                        (
                            ts_catalog::DEFAULT_PARSER,
                            ts_catalog::map_of(oid).unwrap_or_default(),
                        )
                    }
                };
                self.free_ts_config_name(&schema, &bare)?;
                let config = UserConfig {
                    oid: self.mint_composite_oid()?,
                    schema,
                    name: bare,
                    owner: self.current_role_name(),
                    parser,
                    map,
                };
                self.write_ts_config(&config, None)?;
                tag("CREATE TEXT SEARCH CONFIGURATION")
            }
            TsConfigOp::Alter { name, action } => {
                let mut c = self.changeable_ts_config(&name)?;
                let comment = self.ts_config_comment(c.oid);
                match action {
                    MapAction::Add { tokens, dicts } => {
                        let types = Self::token_types(&tokens)?;
                        let dicts: Vec<i64> = dicts
                            .iter()
                            .map(Self::dictionary)
                            .collect::<PgWireResult<_>>()?;
                        for t in types {
                            for (i, d) in dicts.iter().enumerate() {
                                let seq = i as i32 + 1;
                                if c.map.iter().any(|(mt, ms, _)| *mt == t && *ms == seq) {
                                    let mut info = ErrorInfo::new(
                                        "ERROR".into(),
                                        "23505".into(),
                                        "duplicate key value violates unique constraint \"pg_ts_config_map_index\"".into(),
                                    );
                                    info.detail = Some(format!(
                                        "Key (mapcfg, maptokentype, mapseqno)=({}, {t}, {seq}) already exists.",
                                        c.oid
                                    ));
                                    return Err(PgWireError::UserError(Box::new(info)));
                                }
                                c.map.push((t, seq, *d));
                            }
                        }
                    }
                    MapAction::AlterFor { tokens, dicts } => {
                        let types = Self::token_types(&tokens)?;
                        let dicts: Vec<i64> = dicts
                            .iter()
                            .map(Self::dictionary)
                            .collect::<PgWireResult<_>>()?;
                        c.map.retain(|(t, _, _)| !types.contains(t));
                        for t in types {
                            for (i, d) in dicts.iter().enumerate() {
                                c.map.push((t, i as i32 + 1, *d));
                            }
                        }
                    }
                    MapAction::Replace { tokens, old, new } => {
                        let types = Self::token_types(&tokens)?;
                        let (old, new) = (Self::dictionary(&old)?, Self::dictionary(&new)?);
                        for (t, _, d) in &mut c.map {
                            if *d == old && (types.is_empty() || types.contains(t)) {
                                *d = new;
                            }
                        }
                    }
                    MapAction::Drop { tokens, missing_ok } => {
                        let types = Self::token_types(&tokens)?;
                        for (t, alias) in types.iter().zip(&tokens) {
                            if !c.map.iter().any(|(mt, _, _)| mt == t) {
                                let msg =
                                    format!("mapping for token type \"{alias}\" does not exist");
                                if !missing_ok {
                                    return Err(Self::user_error("42704", msg));
                                }
                                self.notice("00000", format!("{msg}, skipping"), None);
                            }
                            c.map.retain(|(mt, _, _)| mt != t);
                        }
                    }
                }
                c.map.sort();
                self.write_ts_config(&c, comment.as_deref())?;
                tag("ALTER TEXT SEARCH CONFIGURATION")
            }
            TsConfigOp::Rename { name, to } => {
                let c = self.changeable_ts_config(&name)?;
                if ts_catalog::oid_in(&c.schema, &to, false).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!(
                            "text search configuration \"{to}\" already exists in schema \"{}\"",
                            c.schema
                        ),
                    ));
                }
                let comment = self.ts_config_comment(c.oid);
                self.delete_type_doc(TS_CONFIG_COLLECTION, &key_of(&c.schema, &c.name))?;
                self.write_ts_config(&UserConfig { name: to, ..c }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH CONFIGURATION")
            }
            TsConfigOp::SetSchema { name, schema } => {
                let c = self.changeable_ts_config(&name)?;
                self.require_schema(&schema)?;
                if ts_catalog::oid_in(&schema, &c.name, false).is_some_and(|o| o != c.oid) {
                    return Err(Self::user_error(
                        "42710",
                        format!(
                            "text search configuration \"{}\" already exists in schema \"{schema}\"",
                            c.name
                        ),
                    ));
                }
                let comment = self.ts_config_comment(c.oid);
                self.delete_type_doc(TS_CONFIG_COLLECTION, &key_of(&c.schema, &c.name))?;
                self.write_ts_config(&UserConfig { schema, ..c }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH CONFIGURATION")
            }
            TsConfigOp::Owner { name, owner } => {
                let c = self.changeable_ts_config(&name)?;
                let owner = self.grantee_name(&owner)?;
                if self.role(&owner)?.is_none() && owner != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{owner}\" does not exist"),
                    ));
                }
                let comment = self.ts_config_comment(c.oid);
                self.write_ts_config(&UserConfig { owner, ..c }, comment.as_deref())?;
                tag("ALTER TEXT SEARCH CONFIGURATION")
            }
            TsConfigOp::Drop {
                names,
                if_exists,
                cascade,
            } => {
                let mut doomed = Vec::new();
                for name in &names {
                    match self.resolve_ts_config(name) {
                        Ok(_) => doomed.push(self.changeable_ts_config(name)?),
                        Err(e) if if_exists => {
                            if let [schema, _] = name.as_slice() {
                                self.require_schema(schema)?;
                            }
                            let _ = e;
                            self.notice(
                                "00000",
                                format!(
                                    "text search configuration \"{}\" does not exist, skipping",
                                    written(name)
                                ),
                                None,
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                let dependents = self.ts_config_dependents(&doomed)?;
                if !dependents.is_empty() {
                    if !cascade {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "2BP01".into(),
                            format!(
                                "cannot drop {} because other objects depend on it",
                                described(&doomed[0])
                            ),
                        );
                        info.detail = Some(
                            dependents
                                .iter()
                                .map(|(d, on, _)| format!("{d} depends on {}", described(on)))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                        info.hint =
                            Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                    let descs: Vec<String> = dependents.iter().map(|(d, _, _)| d.clone()).collect();
                    self.cascade_notice(&descs);
                    // One ALTER TABLE per table, its actions together, as
                    // one statement changes a table's definition.
                    let mut drops: Vec<secantus_pgplan::Statement> = Vec::new();
                    for (_, _, drop) in dependents {
                        use secantus_pgplan::Statement;
                        if let Statement::AlterTable { table, actions, .. } = &drop {
                            if let Some(Statement::AlterTable { actions: all, .. }) =
                                drops.iter_mut().find(|d| {
                                    matches!(d, Statement::AlterTable { table: t, .. } if t == table)
                                })
                            {
                                all.extend(actions.iter().cloned());
                                continue;
                            }
                        }
                        drops.push(drop);
                    }
                    for drop in drops {
                        self.execute_inner(drop, 0)?;
                    }
                }
                for c in &doomed {
                    self.delete_type_doc(TS_CONFIG_COLLECTION, &key_of(&c.schema, &c.name))?;
                }
                self.publish_ts_configs();
                tag("DROP TEXT SEARCH CONFIGURATION")
            }
        }
    }

    fn ts_config_comment(&self, oid: i64) -> Option<String> {
        self.ts_config_doc(oid)
            .and_then(|d| d.get_str("comment").ok().map(str::to_string))
    }

    /// `COMMENT ON TEXT SEARCH CONFIGURATION`.
    pub(crate) fn comment_on_ts_config(
        &self,
        names: &[String],
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let c = self.changeable_ts_config(&names.to_vec())?;
        self.write_ts_config(&c, comment.as_deref())
    }

    /// `(oid, comment)` of each configuration with one.
    pub(crate) fn ts_config_comments(&self) -> Vec<(i64, String)> {
        self.type_catalog_docs(TS_CONFIG_COLLECTION)
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

    /// The configurations a schema holds, for DROP SCHEMA.
    pub(crate) fn ts_configs_in(&self, schema: &str) -> Vec<UserConfig> {
        self.user_ts_configs()
            .into_iter()
            .filter(|c| c.schema == schema)
            .collect()
    }

    /// The objects whose expressions name one of `configs` by a constant:
    /// a column's default or generation expression, a check constraint, an
    /// index's expressions or predicate, a view. Each with the
    /// configuration, and the statement CASCADE drops it with.
    fn ts_config_dependents(
        &self,
        configs: &[UserConfig],
    ) -> PgWireResult<Vec<(String, UserConfig, secantus_pgplan::Statement)>> {
        use secantus_pgplan::{AlterTableAction, Statement};
        let mut out = Vec::new();
        if configs.is_empty() {
            return Ok(out);
        }
        let names_one = |sql: &str| -> Option<UserConfig> {
            config_constants(sql)
                .into_iter()
                .filter_map(|n| ts_catalog::oid_of(&n, false))
                .find_map(|oid| configs.iter().find(|c| c.oid == oid).cloned())
        };
        let alter = |table: &str, action: AlterTableAction| Statement::AlterTable {
            table: table.to_string(),
            missing_ok: true,
            actions: vec![action],
        };
        let tables = self.all_table_defs()?;
        for t in &tables {
            let shown = secantus_pgplan::schemas::visible_name(&t.name);
            for c in &t.columns {
                // An expression default, or one folded to its value at
                // CREATE and kept as written.
                if let Some(cfg) = c
                    .default_expr()
                    .or_else(|| c.default_sql())
                    .and_then(|e| names_one(&format!("select {e}")))
                {
                    out.push((
                        format!("default value for column {} of table {shown}", c.name),
                        cfg,
                        alter(
                            &t.name,
                            AlterTableAction::SetDefault {
                                column: c.name.clone(),
                                value: None,
                                expr: None,
                                sql: None,
                            },
                        ),
                    ));
                }
                if let Some(cfg) = c
                    .extra
                    .get_str("generated")
                    .ok()
                    .and_then(|e| names_one(&format!("select {e}")))
                {
                    out.push((
                        format!("column {} of table {shown}", c.name),
                        cfg,
                        alter(
                            &t.name,
                            AlterTableAction::DropColumn {
                                name: c.name.clone(),
                                if_exists: true,
                            },
                        ),
                    ));
                }
            }
            for k in &t.check_constraints {
                if let Some(cfg) = names_one(&format!("select {}", k.expression)) {
                    out.push((
                        format!("constraint {} on table {shown}", k.name),
                        cfg,
                        alter(
                            &t.name,
                            AlterTableAction::DropConstraint {
                                name: k.name.clone(),
                                if_exists: true,
                            },
                        ),
                    ));
                }
            }
        }
        for t in &tables {
            for ix in self
                .storage
                .list_indexes(self.db(), &t.name)
                .unwrap_or_default()
            {
                let Some((exprs, _)) = Self::index_expressions(&ix) else {
                    continue;
                };
                let Some(cfg) = exprs.iter().find_map(|e| names_one(&format!("select {e}"))) else {
                    continue;
                };
                let name = ix.get_str("name").unwrap_or_default().to_string();
                out.push((
                    format!("index {}", secantus_pgplan::schemas::visible_name(&name)),
                    cfg,
                    Statement::DropIndex {
                        names: vec![name],
                        if_exists: true,
                    },
                ));
            }
        }
        for (name, definition) in self.views_shared()?.iter() {
            if let Some(cfg) = names_one(definition) {
                out.push((
                    format!("view {}", secantus_pgplan::schemas::visible_name(name)),
                    cfg,
                    Statement::DropView {
                        names: vec![name.clone()],
                        if_exists: true,
                        cascade: true,
                    },
                ));
            }
        }
        Ok(out)
    }
}

/// The record's configuration.
fn config_of_doc(d: &Document) -> Option<UserConfig> {
    let map = d
        .get_array("map")
        .ok()?
        .iter()
        .filter_map(|e| {
            let e = e.as_array()?;
            Some((
                e.first().and_then(bson_i64)? as i32,
                e.get(1).and_then(bson_i64)? as i32,
                e.get(2).and_then(bson_i64)?,
            ))
        })
        .collect();
    Some(UserConfig {
        oid: d.get("oid").and_then(bson_i64)?,
        schema: d.get_str("schema").ok()?.to_string(),
        name: d.get_str("name").ok()?.to_string(),
        owner: d.get_str("owner").unwrap_or_default().to_string(),
        parser: d
            .get("parser")
            .and_then(bson_i64)
            .unwrap_or(ts_catalog::DEFAULT_PARSER),
        map,
    })
}

/// The configuration names a statement's constants give: a string cast to
/// `regconfig`, or the string leading a text-search function's arguments.
fn config_constants(sql: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let string = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
        Some(N::AConst(c)) => match c.val.as_ref() {
            Some(pg_query::protobuf::a_const::Val::Sval(s)) => Some(s.sval.clone()),
            _ => None,
        },
        _ => None,
    };
    let mut out = Vec::new();
    for (node, _, _, _) in parsed.protobuf.nodes() {
        match node {
            pg_query::NodeRef::TypeCast(tc) => {
                let to_regconfig = tc.type_name.as_ref().is_some_and(|t| {
                    t.names.iter().any(
                        |n| matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "regconfig"),
                    )
                });
                if let Some(v) = tc.arg.as_deref().and_then(string).filter(|_| to_regconfig) {
                    out.push(v);
                }
            }
            pg_query::NodeRef::FuncCall(f) => {
                let name = f.funcname.iter().rev().find_map(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.clone()),
                    _ => None,
                });
                let leads = match name.as_deref() {
                    Some("ts_headline") => f.args.len() == 4,
                    Some("json_to_tsvector" | "jsonb_to_tsvector") => f.args.len() == 3,
                    Some(n) if CONFIG_FUNCTIONS.contains(&n) => f.args.len() == 2,
                    _ => false,
                };
                if let Some(v) = f.args.first().and_then(string).filter(|_| leads) {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    out
}
