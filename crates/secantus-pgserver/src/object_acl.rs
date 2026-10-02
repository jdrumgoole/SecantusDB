//! Privileges on schemas, sequences and functions: what `GRANT` / `REVOKE`
//! record, what `has_schema_privilege` / `has_sequence_privilege` /
//! `has_function_privilege` / `has_column_privilege` answer, and the schema
//! `USAGE` check a statement passes before its tables are checked.
//!
//! One document per object whose ACL a GRANT or REVOKE has touched, in
//! `__sql_object_acl__`: `{_id: "<kind>\0<name>", kind, name, grants:
//! [{grantee, privileges, option_privileges}]}`. No document is PostgreSQL's
//! DEFAULT ACL for the kind -- `PUBLIC` holds `USAGE` on schema `public` and
//! `EXECUTE` on every function, and nobody but the owner holds anything else
//! (measured on PostgreSQL 15). The owner holds every privilege regardless.
//! The Python server keeps no such state, so this shape is this server's own.
//!
//! A function is keyed by its catalog key (`name/nargs`, or
//! `name/nargs/types` for a second overload at one arity), so a GRANT on one
//! overload leaves the others alone, as PostgreSQL's per-signature ACL does.
//! A built-in, which has no catalog document, is keyed `name/nargs`.

use bson::{Bson, Document};
use pgwire::error::PgWireResult;

use crate::{decode_doc, PgHandler};

pub(crate) const OBJECT_ACL_COLLECTION: &str = "__sql_object_acl__";

/// The privileges an object of `kind` can hold, in PostgreSQL's order.
fn kind_privileges(kind: &str) -> &'static [&'static str] {
    match kind {
        "schema" => &["USAGE", "CREATE"],
        "sequence" => &["USAGE", "SELECT", "UPDATE"],
        _ => &["EXECUTE"],
    }
}

/// The ACL key kind for a GRANT's object kind (`procedure` and `routine`
/// are functions here).
pub(crate) fn acl_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "schema" => Some("schema"),
        "sequence" => Some("sequence"),
        "function" | "procedure" | "routine" => Some("function"),
        _ => None,
    }
}

/// Split `f(t1,t2)` into `("f", Some(["t1", "t2"]))`, `f` into
/// `("f", None)`, splitting arguments at top-level commas only.
fn split_signature(text: &str) -> (String, Option<Vec<String>>) {
    let Some((name, rest)) = text.split_once('(') else {
        return (text.trim().to_string(), None);
    };
    let inner = rest.strip_suffix(')').unwrap_or(rest);
    let mut args = Vec::new();
    let (mut depth, mut current) = (0usize, String::new());
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(std::mem::take(&mut current).trim().to_string());
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    (name.trim().to_string(), Some(args))
}

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl PgHandler {
    /// The ACL document of `(kind, name)`, when a GRANT / REVOKE made one.
    fn object_acl_doc(&self, kind: &str, name: &str) -> Option<Document> {
        self.storage
            .find_matching(
                self.db(),
                OBJECT_ACL_COLLECTION,
                &bson::doc! {"_id": format!("{kind}\0{name}")},
            )
            .ok()?
            .first()
            .and_then(|b| decode_doc(b).ok())
    }

    /// `(grantee, privileges, option_privileges)` of an object: its recorded
    /// ACL, else the kind's default.
    fn object_grants(&self, kind: &str, name: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
        if let Some(d) = self.object_acl_doc(kind, name) {
            return d
                .get_array("grants")
                .map(|a| {
                    a.iter()
                        .filter_map(|g| g.as_document())
                        .map(|g| {
                            (
                                g.get_str("grantee").unwrap_or_default().to_string(),
                                strings(g, "privileges"),
                                strings(g, "option_privileges"),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        match (kind, name) {
            ("schema", "public") => vec![("PUBLIC".into(), vec!["USAGE".into()], vec![])],
            ("function", _) => vec![("PUBLIC".into(), vec!["EXECUTE".into()], vec![])],
            _ => Vec::new(),
        }
    }

    /// The owner of `(kind, name)`: recorded on a schema / sequence /
    /// function at CREATE; the login role for one made before that was.
    fn object_owner(&self, kind: &str, name: &str) -> String {
        let recorded = match kind {
            "schema" => self
                .type_catalog_docs(Self::SCHEMA_COLLECTION)
                .ok()
                .and_then(|docs| {
                    docs.iter()
                        .find(|d| d.get_str("_id") == Ok(name))
                        .and_then(|d| d.get_str("owner").ok().map(str::to_string))
                }),
            "sequence" => self
                .sequence_doc(name)
                .ok()
                .flatten()
                .and_then(|d| d.get_str("owner").ok().map(str::to_string)),
            _ => self
                .type_catalog_docs(Self::FUNCTION_COLLECTION)
                .ok()
                .and_then(|docs| {
                    docs.iter()
                        .find(|d| d.get_str("_id") == Ok(name))
                        .and_then(|d| d.get_str("owner").ok().map(str::to_string))
                }),
        };
        recorded.unwrap_or_else(|| self.session_user_name())
    }

    /// Does `role` hold `privilege` on `(kind, name)`: a superuser, the
    /// owner (or a member of the owning role), or a grant to it, a role it
    /// is in, or PUBLIC.
    pub(crate) fn object_privilege_held(
        &self,
        role: &str,
        kind: &str,
        name: &str,
        privilege: &str,
        with_option: bool,
    ) -> bool {
        if self.is_superuser(role) {
            return true;
        }
        let owner = self.object_owner(kind, name);
        if self.role_is_member(role, &owner) {
            return true;
        }
        self.object_grants(kind, name)
            .iter()
            .any(|(grantee, privs, options)| {
                let list = if with_option { options } else { privs };
                (grantee.eq_ignore_ascii_case("PUBLIC") || self.role_is_member(role, grantee))
                    && list.iter().any(|p| p == privilege)
            })
    }

    /// `GRANT` / `REVOKE` on schemas, sequences and functions.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grant_object(
        &self,
        is_grant: bool,
        privileges: &[String],
        kind: &'static str,
        written_kind: &str,
        objects: &[String],
        grantees: &[String],
        grant_option: bool,
    ) -> PgWireResult<()> {
        let all = kind_privileges(kind);
        // A name that is no privilege at all is the grammar's error; one
        // that is, but not for this kind of object, is 0LP01.
        const KNOWN: [&str; 13] = [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "USAGE",
            "CREATE",
            "CONNECT",
            "TEMPORARY",
            "TEMP",
            "EXECUTE",
        ];
        if let Some(p) = privileges
            .iter()
            .find(|p| *p != "ALL" && !KNOWN.contains(&p.as_str()))
        {
            return Err(Self::user_error(
                "42601",
                format!("unrecognized privilege type \"{}\"", p.to_ascii_lowercase()),
            ));
        }
        let wanted: Vec<&'static str> =
            if privileges.is_empty() || privileges.iter().any(|p| p == "ALL") {
                all.to_vec()
            } else {
                let mut out = Vec::new();
                for p in privileges {
                    match all.iter().find(|a| **a == p.as_str()) {
                        Some(a) => out.push(*a),
                        None => {
                            return Err(Self::user_error(
                                "0LP01",
                                format!("invalid privilege type {p} for {written_kind}"),
                            ))
                        }
                    }
                }
                out
            };
        for object in objects {
            let name = match kind {
                "schema" => {
                    if !secantus_pgplan::schemas::schema_exists(object) {
                        return Err(Self::user_error(
                            "3F000",
                            format!("schema \"{object}\" does not exist"),
                        ));
                    }
                    object.clone()
                }
                // Already the catalog key: the planner resolved the RangeVar.
                "sequence" => object.clone(),
                _ => self.function_acl_key(object)?,
            };
            let mut grants = self.object_grants(kind, &name);
            for grantee in grantees {
                let at = match grants.iter().position(|(g, _, _)| g == grantee) {
                    Some(i) => i,
                    None => {
                        grants.push((grantee.clone(), Vec::new(), Vec::new()));
                        grants.len() - 1
                    }
                };
                let (_, held, options) = &mut grants[at];
                for p in &wanted {
                    let p = p.to_string();
                    if is_grant {
                        if !held.contains(&p) {
                            held.push(p.clone());
                        }
                        if grant_option && !options.contains(&p) {
                            options.push(p);
                        }
                    } else if grant_option {
                        // `REVOKE GRANT OPTION FOR` keeps the privilege.
                        options.retain(|o| *o != p);
                    } else {
                        held.retain(|h| *h != p);
                        options.retain(|o| *o != p);
                    }
                }
                let order = |v: &mut Vec<String>| {
                    v.sort_by_key(|p| all.iter().position(|a| a == p));
                };
                order(held);
                order(options);
            }
            grants.retain(|(_, held, _)| !held.is_empty());
            let grants: Vec<Bson> = grants
                .into_iter()
                .map(|(grantee, privs, options)| {
                    Bson::Document(bson::doc! {
                        "grantee": grantee,
                        "privileges": privs,
                        "option_privileges": options,
                    })
                })
                .collect();
            let id = format!("{kind}\0{name}");
            self.put_comment_doc(
                OBJECT_ACL_COLLECTION,
                &id,
                Some(bson::doc! {"_id": &id, "kind": kind, "name": &name, "grants": grants}),
            )?;
        }
        Ok(())
    }

    /// A signature as text (`'s.f(integer, text)'`) in the planner's
    /// normal form (`f(int4,text)`), read through GRANT's own parse.
    fn normalized_signature(text: &str) -> PgWireResult<String> {
        let sql = format!("GRANT EXECUTE ON FUNCTION {text} TO PUBLIC");
        match secantus_pgplan::plan(&sql, &|_| None) {
            Ok(secantus_pgplan::Statement::Grant { objects, .. }) if objects.len() == 1 => {
                Ok(objects[0].clone())
            }
            _ => Err(Self::user_error(
                "42601",
                format!("invalid function signature \"{text}\""),
            )),
        }
    }

    /// The ACL key of the routine a GRANT names (`f(int4)`, or bare `f`
    /// when it is the only one of that name): its catalog key. 42883 when no
    /// such signature exists, 42725 for an ambiguous bare name. A built-in
    /// has no catalog document and is keyed `name/nargs`.
    pub(crate) fn function_acl_key(&self, signature: &str) -> PgWireResult<String> {
        // Already a catalog key (`ON ALL FUNCTIONS IN SCHEMA` expanded it).
        if let Some(key) = signature.strip_prefix('\0') {
            return Ok(key.to_string());
        }
        let (name, args) = split_signature(signature);
        let functions = self.functions()?;
        let candidates: Vec<_> = functions.iter().filter(|f| f.name == name).collect();
        match args {
            Some(types) => {
                if let Some(f) = candidates.iter().find(|f| f.param_types == types) {
                    return Ok(f.key.clone());
                }
                if candidates.is_empty() && secantus_pgplan::is_known_function(&name) {
                    return Ok(format!("{name}/{}", types.len()));
                }
                let shown: Vec<String> = types
                    .iter()
                    .map(|t| secantus_pgplan::display_type(t))
                    .collect();
                Err(Self::user_error(
                    "42883",
                    format!("function {name}({}) does not exist", shown.join(", ")),
                ))
            }
            None => match candidates.as_slice() {
                [one] => Ok(one.key.clone()),
                [] if secantus_pgplan::is_known_function(&name) => Ok(name),
                [] => Err(Self::user_error(
                    "42883",
                    format!("could not find a function named \"{name}\""),
                )),
                _ => {
                    let mut info = pgwire::error::ErrorInfo::new(
                        "ERROR".into(),
                        "42725".into(),
                        format!("function name \"{name}\" is not unique"),
                    );
                    info.hint = Some(
                        "Specify the argument list to select the function unambiguously.".into(),
                    );
                    Err(pgwire::error::PgWireError::UserError(Box::new(info)))
                }
            },
        }
    }

    /// `EXECUTE` on a user function at its call: 42501 `permission denied
    /// for function f` otherwise.
    pub(crate) fn check_function_execute(&self, key: &str, name: &str) -> PgWireResult<()> {
        let role = self.current_role_name();
        if self.object_privilege_held(&role, "function", key, "EXECUTE", false) {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!("permission denied for function {name}"),
        ))
    }

    /// A sequence function's privilege: any of `privileges` on `name`
    /// (`nextval` takes USAGE or UPDATE, `currval` USAGE or SELECT, `setval`
    /// UPDATE), else 42501 `permission denied for sequence q`.
    pub(crate) fn check_sequence_privilege(
        &self,
        name: &str,
        privileges: &[&str],
    ) -> PgWireResult<()> {
        let role = self.current_role_name();
        if self.is_superuser(&role) || self.sequence_doc(name)?.is_none() {
            return Ok(());
        }
        if privileges
            .iter()
            .any(|p| self.object_privilege_held(&role, "sequence", name, p, false))
        {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!(
                "permission denied for sequence {}",
                secantus_pgplan::schemas::split_key(name).1
            ),
        ))
    }

    /// The schemas `role` holds no `USAGE` on, which PostgreSQL leaves off
    /// its active search path. None for a superuser.
    pub(crate) fn unusable_schemas(&self, role: &str) -> Vec<String> {
        if self.is_superuser(role) {
            return Vec::new();
        }
        self.namespaces()
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| !matches!(n.as_str(), "pg_catalog" | "information_schema" | "pg_toast"))
            .filter(|n| !n.starts_with("pg_temp"))
            .filter(|n| !self.object_privilege_held(role, "schema", n, "USAGE", false))
            .collect()
    }

    /// Schema `CREATE` for every schema a CREATE statement makes something
    /// in: 42501 `permission denied for schema s` otherwise.
    pub(crate) fn check_schema_create(&self, role: &str, sql: &str) -> PgWireResult<()> {
        if self.is_superuser(role) {
            return Ok(());
        }
        for schema in secantus_pgplan::privileges::sql_creation_schemas(sql) {
            if !secantus_pgplan::schemas::schema_exists(&schema) {
                continue;
            }
            if !self.object_privilege_held(role, "schema", &schema, "USAGE", false)
                || !self.object_privilege_held(role, "schema", &schema, "CREATE", false)
            {
                return Err(Self::user_error(
                    "42501",
                    format!("permission denied for schema {schema}"),
                ));
            }
        }
        Ok(())
    }

    /// A dropped object's ACL goes with it.
    pub(crate) fn drop_object_acl(&self, kind: &str, name: &str) -> PgWireResult<()> {
        if self.object_acl_doc(kind, name).is_some() {
            self.put_comment_doc(OBJECT_ACL_COLLECTION, &format!("{kind}\0{name}"), None)?;
        }
        Ok(())
    }

    /// Schema `USAGE` for every user schema a relation key names: 42501
    /// `permission denied for schema s` otherwise, as PostgreSQL answers
    /// while resolving the name -- before any table privilege is checked.
    pub(crate) fn check_schema_usage(&self, role: &str, relation: &str) -> PgWireResult<()> {
        let (schema, _) = secantus_pgplan::schemas::split_key(relation);
        if schema == "public" || schema.starts_with("pg_temp") {
            return Ok(());
        }
        if self.object_privilege_held(role, "schema", &schema, "USAGE", false) {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!("permission denied for schema {schema}"),
        ))
    }

    /// `has_schema_privilege` / `has_sequence_privilege` /
    /// `has_function_privilege` / `has_column_privilege`, reached through the
    /// planner's executor hook (a table already resolved to its catalog key).
    pub(crate) fn has_object_privilege_call(
        &self,
        name: &str,
        args: &[Bson],
    ) -> PgWireResult<Bson> {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let text = |v: &Bson| secantus_pgplan::value_text(v);
        let explicit_role = match name {
            "has_column_privilege" => args.len() == 4,
            _ => args.len() == 3,
        };
        let (role, rest) = if explicit_role {
            let r = text(&args[0]);
            if self.role(&r)?.is_none() && r != self.session_user_name() {
                return Err(Self::user_error(
                    "42704",
                    format!("role \"{r}\" does not exist"),
                ));
            }
            (r, &args[1..])
        } else {
            (self.current_role_name(), args)
        };
        let privileges = text(rest.last().expect("has_*_privilege takes arguments"));
        // `(privilege, with grant option, as written)`.
        let wanted: Vec<(String, bool, String)> = privileges
            .split(',')
            .map(|written| {
                let written = written.trim();
                let p = written.to_ascii_uppercase();
                match p.strip_suffix(" WITH GRANT OPTION") {
                    Some(base) => (base.trim().to_string(), true, written.to_string()),
                    None => (p, false, written.to_string()),
                }
            })
            .collect();
        let held = |kind: &str, object: &str| -> PgWireResult<bool> {
            let all = kind_privileges(kind);
            for (p, _, written) in &wanted {
                if !all.contains(&p.as_str()) {
                    return Err(Self::user_error(
                        "22023",
                        format!("unrecognized privilege type: \"{written}\""),
                    ));
                }
            }
            Ok(wanted
                .iter()
                .any(|(p, o, _)| self.object_privilege_held(&role, kind, object, p, *o)))
        };
        let answer = match name {
            "has_schema_privilege" => {
                let schema = text(&rest[0]);
                if !secantus_pgplan::schemas::schema_exists(&schema) {
                    return Err(Self::user_error(
                        "3F000",
                        format!("schema \"{schema}\" does not exist"),
                    ));
                }
                held("schema", &schema)?
            }
            "has_sequence_privilege" => {
                let key = Self::relation_text_key(&text(&rest[0]));
                if self.sequence_doc(&key)?.is_none()
                    && (self.lookup(&key).is_some() || self.view_doc(&key).is_some())
                {
                    return Err(Self::user_error(
                        "42809",
                        format!("\"{}\" is not a sequence", text(&rest[0])),
                    ));
                }
                if self.sequence_doc(&key)?.is_none() {
                    return Err(Self::user_error(
                        "42P01",
                        format!("relation \"{}\" does not exist", text(&rest[0])),
                    ));
                }
                held("sequence", &key)?
            }
            "has_function_privilege" => {
                let written = text(&rest[0]);
                if !written.contains('(') {
                    return Err(Self::user_error(
                        "22P02",
                        "expected a left parenthesis".into(),
                    ));
                }
                let key = self
                    .function_acl_key(&Self::normalized_signature(&written)?)
                    .map_err(|_| {
                        Self::user_error("42883", format!("function \"{written}\" does not exist"))
                    })?;
                held("function", &key)?
            }
            _ => {
                // has_column_privilege: the table privilege, or the
                // column's own grant.
                let table = text(&rest[0]);
                let column = match &rest[1] {
                    Bson::String(c) => c.clone(),
                    other => {
                        let n = secantus_pgplan::value_text(other)
                            .parse::<usize>()
                            .unwrap_or(0);
                        self.lookup(&table)
                            .and_then(|d| d.columns.get(n.wrapping_sub(1)).map(|c| c.name.clone()))
                            .unwrap_or_default()
                    }
                };
                if let Some(def) = self.lookup(&table) {
                    if def.column(&column).is_none() {
                        return Err(Self::user_error(
                            "42703",
                            format!("column \"{column}\" of relation \"{table}\" does not exist"),
                        ));
                    }
                }
                wanted.iter().any(|(p, o, _)| {
                    self.privilege_held(&role, &table, p, "table", *o)
                        || (!*o
                            && self.column_grant_docs(&table).iter().any(|g| {
                                let grantee = g.get_str("grantee").unwrap_or_default();
                                g.get_str("column") == Ok(column.as_str())
                                    && (grantee.eq_ignore_ascii_case("PUBLIC")
                                        || self.role_is_member(&role, grantee))
                                    && g.get_array("privileges").is_ok_and(|ps| {
                                        ps.iter().any(|x| x.as_str() == Some(p.as_str()))
                                    })
                            }))
                })
            }
        };
        Ok(Bson::Boolean(answer))
    }

    /// `information_schema.role_table_grants` (and, with `PUBLIC` grants
    /// kept, `table_privileges`): every table privilege, the owner's own
    /// included, whose grantor or grantee is a role the current role is in.
    pub(crate) fn table_grant_rows(&self, include_public: bool) -> Vec<[String; 7]> {
        let me = self.current_role_name();
        let mut out = Vec::new();
        let Ok(defs) = self.all_table_defs() else {
            return out;
        };
        for def in defs.iter().filter(|d| !d.temp) {
            let owner = self.table_owner(def);
            let (schema, table) = secantus_pgplan::schemas::split_key(&def.name);
            let owner_privs: Vec<String> = self
                .grant_docs_in(Self::RELATION_ACL_COLLECTION, &def.name)
                .first()
                .map(|d| strings(d, "owner_privs"))
                .unwrap_or_else(|| {
                    Self::TABLE_PRIVILEGES
                        .iter()
                        .map(|p| p.to_string())
                        .collect()
                });
            let mut rows: Vec<(String, String, bool)> = owner_privs
                .into_iter()
                .map(|p| (owner.clone(), p, true))
                .collect();
            for g in self.grant_docs_in(Self::GRANT_COLLECTION, &def.name) {
                let grantee = g.get_str("grantee").unwrap_or_default().to_string();
                let options = strings(&g, "option_privileges");
                for p in strings(&g, "privileges") {
                    let grantable = options.contains(&p);
                    rows.push((grantee.clone(), p, grantable));
                }
            }
            for (grantee, privilege, grantable) in rows {
                let visible = self.role_is_member(&me, &owner)
                    || self.role_is_member(&me, &grantee)
                    || (include_public && grantee.eq_ignore_ascii_case("PUBLIC"));
                if !visible {
                    continue;
                }
                let hierarchy = if privilege == "SELECT" { "YES" } else { "NO" };
                out.push([
                    owner.clone(),
                    grantee,
                    schema.clone(),
                    table.clone(),
                    privilege,
                    if grantable { "YES" } else { "NO" }.to_string(),
                    hierarchy.to_string(),
                ]);
            }
        }
        out
    }
}
