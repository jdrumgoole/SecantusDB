//! Schema-qualified relations.
//!
//! The catalog this server shares with the Python one stores a relation in
//! `public` under its bare name and one in any other schema under
//! `schema.name` -- the catalog key, the backing collection and the
//! sequence / view keys alike (the Python server's `qualified_table_name`).
//! So the planner never has to carry a schema: every `RangeVar` (and every
//! name list a DROP / COMMENT carries) is REWRITTEN to that key before the
//! statement is planned, the same way the Python server rewrites its tree in
//! `qualify_from_search_path`.
//!
//! An unqualified name is resolved through `search_path` the way PostgreSQL
//! does: the first schema on the path holding the relation wins, and a
//! relation in no schema on the path is not found. A CREATE target lands in
//! the first schema on the path that exists (3F000 when none does).

use super::*;
use pg_query::protobuf::{Alias, RangeVar};

thread_local! {
    /// The user schemas (`CREATE SCHEMA`), installed per statement.
    static USER_SCHEMAS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Every relation's catalog key (tables, views, sequences), installed
    /// per statement: a table the Python server created carries no row-type
    /// entry, so the regclass list alone does not see it.
    static RELATION_KEYS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// The session's own temporary schema (`pg_temp_N`), installed per
    /// statement. A TEMP relation is stored under `pg_temp_N.name`, so two
    /// sessions' same-named temp tables are distinct relations -- as they
    /// are in PostgreSQL, where every backend has its own temp namespace.
    static TEMP_SCHEMA: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// The schemas the current role holds no `USAGE` on, installed per
    /// statement: PostgreSQL drops them from the active search path, so an
    /// unqualified name never resolves into one and nothing is created there.
    static UNUSABLE_SCHEMAS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the schemas the current role cannot use (no `USAGE`).
pub fn set_unusable_schemas(schemas: Vec<String>) {
    UNUSABLE_SCHEMAS.with(|s| *s.borrow_mut() = schemas);
}

fn schema_usable(schema: &str) -> bool {
    UNUSABLE_SCHEMAS.with(|s| !s.borrow().iter().any(|n| n == schema))
}

/// Install the session's temporary schema for the statements that follow.
pub fn set_temp_schema(schema: Option<String>) {
    TEMP_SCHEMA.with(|t| *t.borrow_mut() = schema);
}

/// The session's temporary schema, when one is installed.
pub fn temp_schema() -> Option<String> {
    TEMP_SCHEMA.with(|t| t.borrow().clone())
}

/// Does `schema` name the session's temp schema (`pg_temp`, its alias, or
/// `pg_temp_N` itself)?
fn names_temp_schema(schema: &str) -> Option<String> {
    let t = temp_schema()?;
    (schema == "pg_temp" || schema == t).then_some(t)
}

/// Does the session have a temporary relation?
fn session_has_temp() -> bool {
    let Some(t) = temp_schema() else {
        return false;
    };
    let prefix = format!("{t}.");
    RELATION_KEYS.with(|k| k.borrow().iter().any(|n| n.starts_with(&prefix)))
        || PLAN_USER_RELATIONS.with(|r| r.borrow().iter().any(|(n, _, _)| n.starts_with(&prefix)))
}

/// Install every relation's catalog key for the statements that follow.
pub fn set_relation_keys(keys: std::collections::HashSet<String>) {
    RELATION_KEYS.with(|k| *k.borrow_mut() = keys);
}

/// Add one relation key to the installed set: a TEMP table a statement
/// created part-way through a larger one (a trigger's transition table, a
/// function body's own temp table), which the next statement inside it must
/// already resolve to.
pub fn note_relation_key(key: &str) {
    RELATION_KEYS.with(|k| {
        k.borrow_mut().insert(key.to_string());
    });
}

/// Install the database's user schemas for the statements that follow.
pub fn set_user_schemas(schemas: Vec<String>) {
    USER_SCHEMAS.with(|s| *s.borrow_mut() = schemas);
}

/// Schemas every database has, which hold no user relation of their own
/// (`public` holds them under their bare name).
fn is_builtin(schema: &str) -> bool {
    matches!(
        schema,
        "public" | "pg_catalog" | "information_schema" | "pg_toast"
    ) || schema == "pg_temp"
        || schema.starts_with("pg_temp_")
}

/// Does `schema` exist?
pub fn schema_exists(schema: &str) -> bool {
    is_builtin(schema) || USER_SCHEMAS.with(|s| s.borrow().iter().any(|n| n == schema))
}

fn is_user_schema(schema: &str) -> bool {
    !is_builtin(schema) && USER_SCHEMAS.with(|s| s.borrow().iter().any(|n| n == schema))
}

/// The catalog key of `name` in `schema`: bare in `public`, else
/// `schema.name`.
pub fn relation_key(schema: &str, name: &str) -> String {
    if schema == "public" {
        name.to_string()
    } else {
        format!("{schema}.{name}")
    }
}

/// Split a catalog key into `(schema, name)`: `schema.name` when the prefix
/// is a user schema, else `public`.
pub fn split_key(key: &str) -> (String, String) {
    if let Some((s, n)) = key.split_once('.') {
        if is_user_schema(s) || s.starts_with("pg_temp_") {
            return (s.to_string(), n.to_string());
        }
    }
    ("public".to_string(), key.to_string())
}

/// The session's `search_path`, as the schema names it lists (`"$user"`
/// expanded to the session user when a schema of that name exists, dropped
/// otherwise; quotes removed).
pub fn search_path() -> Vec<String> {
    let raw = session_setting("search_path").unwrap_or_else(|| "\"$user\", public".into());
    let user = session_user();
    raw.split(',')
        .filter_map(|p| {
            let p = p.trim();
            let p = if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
                p[1..p.len() - 1].replace("\"\"", "\"")
            } else {
                p.to_ascii_lowercase()
            };
            if p == "$user" {
                return user
                    .clone()
                    .filter(|u| is_user_schema(u) && schema_usable(u));
            }
            (!p.is_empty() && schema_usable(&p)).then_some(p)
        })
        .collect()
}

/// Is the path one that can only ever resolve to `public` -- nothing to
/// rewrite?
fn path_is_trivial(path: &[String]) -> bool {
    path.iter().all(|s| !is_user_schema(s)) && !session_has_temp()
}

fn relation_exists(key: &str) -> bool {
    RELATION_KEYS.with(|k| k.borrow().contains(key))
        || PLAN_USER_RELATIONS.with(|t| t.borrow().iter().any(|(n, _, _)| n == key))
}

/// The key an UNQUALIFIED reference resolves to: the first schema on the path
/// holding it, else the bare name (whose lookup then fails as PostgreSQL's
/// does, naming what the user wrote).
pub fn resolve_unqualified(name: &str) -> String {
    // The session's temp schema is searched first for relations, as
    // PostgreSQL's implicit `pg_temp` is.
    if let Some(t) = temp_schema() {
        let key = relation_key(&t, name);
        if relation_exists(&key) {
            return key;
        }
    }
    for schema in search_path() {
        if !schema_exists(schema.as_str()) || matches!(schema.as_str(), "pg_catalog") {
            continue;
        }
        let key = relation_key(&schema, name);
        if relation_exists(&key) {
            return key;
        }
    }
    name.to_string()
}

/// The schema an unqualified CREATE lands in.
pub fn creation_schema() -> Result<String> {
    search_path()
        .into_iter()
        .find(|s| schema_exists(s) && !matches!(s.as_str(), "pg_catalog" | "information_schema"))
        .ok_or_else(|| Error::Sqlstate("3F000", "no schema has been selected to create in".into()))
}

fn qualify_target(r: &mut RangeVar) -> Result<()> {
    // `CREATE TABLE pg_temp.x` makes a temporary table, as TEMP does.
    if r.relpersistence != "t" && names_temp_schema(&r.schemaname).is_some() {
        r.relpersistence = "t".into();
    }
    if r.relpersistence == "t" {
        if let Some(t) = temp_schema() {
            if r.schemaname.is_empty() || names_temp_schema(&r.schemaname).is_some() {
                r.relname = relation_key(&t, &r.relname);
                r.schemaname.clear();
            } else {
                // At the relation's name, as PostgreSQL points it.
                crate::set_error_location(r.location);
                return Err(Error::Sqlstate(
                    "42P16",
                    "cannot create temporary relation in non-temporary schema".into(),
                ));
            }
        }
        return Ok(());
    }
    if r.schemaname.is_empty() {
        let schema = creation_schema()?;
        if schema != "public" {
            r.relname = relation_key(&schema, &r.relname);
        }
        return Ok(());
    }
    if is_builtin(&r.schemaname) {
        return Ok(());
    }
    if !schema_exists(&r.schemaname) {
        return Err(Error::Sqlstate(
            "3F000",
            format!("schema \"{}\" does not exist", r.schemaname),
        ));
    }
    r.relname = relation_key(&r.schemaname, &r.relname);
    r.schemaname.clear();
    Ok(())
}

/// Rewrite one reference. Returns the schema it named explicitly, if any.
fn qualify_reference(r: &mut RangeVar, ctes: &[String], trivial: bool) -> Option<String> {
    if r.schemaname.is_empty() {
        if trivial || r.relname.contains('.') || ctes.contains(&r.relname) {
            return None;
        }
        let key = resolve_unqualified(&r.relname);
        if key != r.relname {
            if r.alias.is_none() {
                r.alias = Some(Alias {
                    aliasname: r.relname.clone(),
                    colnames: Vec::new(),
                });
            }
            r.relname = key;
        }
        return None;
    }
    if let Some(t) = names_temp_schema(&r.schemaname) {
        r.schemaname.clear();
        r.catalogname.clear();
        if r.alias.is_none() {
            r.alias = Some(Alias {
                aliasname: r.relname.clone(),
                colnames: Vec::new(),
            });
        }
        r.relname = relation_key(&t, &r.relname);
        return None;
    }
    if is_builtin(&r.schemaname) {
        return None;
    }
    let schema = std::mem::take(&mut r.schemaname);
    r.catalogname.clear();
    if r.alias.is_none() {
        r.alias = Some(Alias {
            aliasname: r.relname.clone(),
            colnames: Vec::new(),
        });
    }
    r.relname = relation_key(&schema, &r.relname);
    Some(schema)
}

fn strings(l: &pg_query::protobuf::List) -> Option<Vec<String>> {
    l.items
        .iter()
        .map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn string_node(s: String) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String { sval: s })),
    }
}

/// A `[schema.]name[.rest]` name list (DROP / COMMENT), with the relation
/// part folded to its key. `rel_parts` is how many trailing items follow the
/// relation (a column for COMMENT ON COLUMN).
fn qualify_name_list(l: &mut pg_query::protobuf::List, rest: usize, trivial: bool) {
    let Some(parts) = strings(l) else {
        return;
    };
    if parts.len() < 1 + rest {
        return;
    }
    let (rel, tail) = parts.split_at(parts.len() - rest);
    let key = match rel {
        [name] => {
            if trivial || name.contains('.') {
                return;
            }
            resolve_unqualified(name)
        }
        [schema, name] | [_, schema, name] => {
            if let Some(t) = names_temp_schema(schema) {
                relation_key(&t, name)
            } else if is_builtin(schema) {
                return;
            } else {
                relation_key(schema, name)
            }
        }
        _ => return,
    };
    l.items = std::iter::once(key)
        .chain(tail.iter().cloned())
        .map(string_node)
        .collect();
}

fn qualify_constraint(c: &mut pg_query::protobuf::Constraint, ctes: &[String], trivial: bool) {
    if let Some(pk) = c.pktable.as_mut() {
        qualify_reference(pk, ctes, trivial);
        // A REFERENCES target is not a FROM item; no alias belongs on it.
        if pk.alias.as_ref().is_some_and(|a| {
            pk.relname.ends_with(&format!(".{}", a.aliasname)) && a.colnames.is_empty()
        }) {
            pk.alias = None;
        }
    }
}

/// Point every REFERENCES to the unqualified `bare` at `key`.
fn self_references(elts: &mut [pg_query::protobuf::Node], bare: &str, key: &str) {
    let fix = |c: &mut pg_query::protobuf::Constraint| {
        if let Some(pk) = c.pktable.as_mut() {
            if pk.schemaname.is_empty() && pk.relname == bare {
                pk.relname = key.to_string();
            }
        }
    };
    for e in elts {
        match e.node.as_mut() {
            Some(N::Constraint(c)) => fix(c),
            Some(N::ColumnDef(cd)) => {
                for c in &mut cd.constraints {
                    if let Some(N::Constraint(c)) = c.node.as_mut() {
                        fix(c);
                    }
                }
            }
            _ => {}
        }
    }
}

fn qualify_elts(elts: &mut [pg_query::protobuf::Node], ctes: &[String], trivial: bool) {
    for e in elts {
        match e.node.as_mut() {
            Some(N::Constraint(c)) => qualify_constraint(c, ctes, trivial),
            Some(N::ColumnDef(cd)) => {
                for c in &mut cd.constraints {
                    if let Some(N::Constraint(c)) = c.node.as_mut() {
                        qualify_constraint(c, ctes, trivial);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Strip an explicit no-alias marker we added on a target that takes none.
fn drop_added_alias(r: &mut RangeVar) {
    if r.alias
        .as_ref()
        .is_some_and(|a| a.colnames.is_empty() && r.relname.ends_with(&format!(".{}", a.aliasname)))
    {
        r.alias = None;
    }
}

/// Rewrite every relation reference in `node` to its catalog key.
pub fn qualify(node: &mut N) -> Result<()> {
    let path = search_path();
    let trivial = path_is_trivial(&path);
    let has_qualified = node_mentions_schema(node);
    if trivial && !has_qualified {
        return Ok(());
    }
    // The CREATE target, handled first (and skipped by the general walk).
    let mut target: Option<*const RangeVar> = None;
    match node {
        N::CreateStmt(c) => {
            if let Some(r) = c.relation.as_mut() {
                let bare = r.relname.clone();
                qualify_target(r)?;
                target = Some(r as *const _);
                // A REFERENCES back to the table being created names the
                // table itself: a temp table's own key, which no lookup can
                // find before it exists (`create temp table t (... references
                // t)` was 42P01).
                if r.relname != bare && r.schemaname.is_empty() {
                    let key = r.relname.clone();
                    self_references(&mut c.table_elts, &bare, &key);
                }
            }
        }
        N::CreateTableAsStmt(c) => {
            if let Some(r) = c.into.as_mut().and_then(|i| i.rel.as_mut()) {
                qualify_target(r)?;
                target = Some(r as *const _);
            }
        }
        N::ViewStmt(v) => {
            if let Some(r) = v.view.as_mut() {
                qualify_target(r)?;
                target = Some(r as *const _);
            }
        }
        N::CreateSeqStmt(s) => {
            if let Some(r) = s.sequence.as_mut() {
                qualify_target(r)?;
            }
        }
        _ => {}
    }
    let mut ctes: Vec<String> = Vec::new();
    let mut schemas: Vec<String> = Vec::new();
    // SAFETY: the pointers `nodes_mut` hands out point into `node`, which
    // outlives both loops and is not otherwise touched while they are used.
    unsafe {
        for (n, _, _) in node.nodes_mut() {
            if let pg_query::NodeMut::CommonTableExpr(c) = n {
                ctes.push((*c).ctename.clone());
            }
        }
        for (n, _, _) in node.nodes_mut() {
            if let pg_query::NodeMut::RangeVar(r) = n {
                if target.is_some_and(|t| std::ptr::eq(t, r)) {
                    continue;
                }
                if let Some(s) = qualify_reference(&mut *r, &ctes, trivial) {
                    if !schemas.contains(&s) {
                        schemas.push(s);
                    }
                }
            }
        }
        if !schemas.is_empty() {
            for (n, _, _) in node.nodes_mut() {
                if let pg_query::NodeMut::ColumnRef(c) = n {
                    let c = &mut *c;
                    if c.fields.len() >= 3 {
                        if let Some(N::String(s)) = c.fields[0].node.as_ref() {
                            if schemas.contains(&s.sval) {
                                c.fields.remove(0);
                            }
                        }
                    }
                }
            }
        }
    }
    // What the general walk does not reach.
    match node {
        N::CreateStmt(c) => {
            qualify_elts(&mut c.table_elts, &ctes, trivial);
            for p in &mut c.inh_relations {
                if let Some(N::RangeVar(r)) = p.node.as_mut() {
                    qualify_reference(r, &ctes, trivial);
                    drop_added_alias(r);
                }
            }
        }
        N::AlterTableStmt(a) => {
            if let Some(r) = a.relation.as_mut() {
                drop_added_alias(r);
            }
            for cmd in &mut a.cmds {
                if let Some(N::AlterTableCmd(cmd)) = cmd.node.as_mut() {
                    match cmd.def.as_mut().and_then(|d| d.node.as_mut()) {
                        Some(N::Constraint(c)) => qualify_constraint(c, &ctes, trivial),
                        Some(N::ColumnDef(cd)) => {
                            for c in &mut cd.constraints {
                                if let Some(N::Constraint(c)) = c.node.as_mut() {
                                    qualify_constraint(c, &ctes, trivial);
                                }
                            }
                        }
                        Some(N::PartitionCmd(p)) => {
                            if let Some(r) = p.name.as_mut() {
                                qualify_reference(r, &ctes, trivial);
                                drop_added_alias(r);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        N::IndexStmt(i) => {
            if let Some(r) = i.relation.as_mut() {
                drop_added_alias(r);
            }
        }
        N::TruncateStmt(t) => {
            for r in &mut t.relations {
                if let Some(N::RangeVar(r)) = r.node.as_mut() {
                    drop_added_alias(r);
                }
            }
        }
        N::CopyStmt(c) => {
            if let Some(r) = c.relation.as_mut() {
                drop_added_alias(r);
            }
        }
        N::AlterSeqStmt(s) => {
            if let Some(r) = s.sequence.as_mut() {
                qualify_reference(r, &ctes, trivial);
                drop_added_alias(r);
            }
        }
        N::RenameStmt(rn) => {
            if let Some(r) = rn.relation.as_mut() {
                qualify_reference(r, &ctes, trivial);
                drop_added_alias(r);
                // A relation renamed stays in its schema.
                let renames_relation = matches!(
                    ObjectType::try_from(rn.rename_type),
                    Ok(ObjectType::ObjectTable
                        | ObjectType::ObjectView
                        | ObjectType::ObjectSequence
                        | ObjectType::ObjectMatview
                        | ObjectType::ObjectForeignTable)
                );
                if renames_relation {
                    let (schema, _) = split_key(&r.relname);
                    if schema != "public" {
                        rn.newname = relation_key(&schema, &rn.newname);
                    }
                }
            }
        }
        N::DropStmt(d) => {
            let relation_kind = matches!(
                ObjectType::try_from(d.remove_type),
                Ok(ObjectType::ObjectTable
                    | ObjectType::ObjectView
                    | ObjectType::ObjectSequence
                    | ObjectType::ObjectMatview
                    | ObjectType::ObjectForeignTable)
            );
            let index = ObjectType::try_from(d.remove_type) == Ok(ObjectType::ObjectIndex);
            for o in &mut d.objects {
                if let Some(N::List(l)) = o.node.as_mut() {
                    if relation_kind {
                        qualify_name_list(l, 0, trivial);
                    } else if index {
                        // Index names are per table in storage, so the
                        // schema is carried as the `schema.name` key and
                        // the server finds the index on a table of it.
                        if let Some(parts) = strings(l) {
                            if parts.len() >= 2 && !is_builtin(&parts[parts.len() - 2]) {
                                let schema = &parts[parts.len() - 2];
                                let last = &parts[parts.len() - 1];
                                l.items = vec![string_node(relation_key(schema, last))];
                            }
                        }
                    }
                }
            }
        }
        N::CommentStmt(c) => {
            let rest = match ObjectType::try_from(c.objtype) {
                Ok(
                    ObjectType::ObjectTable
                    | ObjectType::ObjectView
                    | ObjectType::ObjectSequence
                    | ObjectType::ObjectMatview
                    | ObjectType::ObjectForeignTable,
                ) => Some(0),
                Ok(ObjectType::ObjectColumn) => Some(1),
                _ => None,
            };
            if let (Some(rest), Some(N::List(l))) =
                (rest, c.object.as_mut().and_then(|o| o.node.as_mut()))
            {
                qualify_name_list(l, rest, trivial);
            }
        }
        N::GrantStmt(g) => {
            for o in &mut g.objects {
                if let Some(N::RangeVar(r)) = o.node.as_mut() {
                    drop_added_alias(r);
                }
            }
        }
        N::LockStmt(l) => {
            for o in &mut l.relations {
                if let Some(N::RangeVar(r)) = o.node.as_mut() {
                    drop_added_alias(r);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Does the statement name a user schema anywhere -- a `RangeVar`, or the
/// name list of a DROP / COMMENT?
fn node_mentions_schema(node: &N) -> bool {
    node.nodes().iter().any(|(n, _, _, _)| match n {
        pg_query::NodeRef::RangeVar(r) => {
            !r.schemaname.is_empty()
                && (!is_builtin(&r.schemaname) || names_temp_schema(&r.schemaname).is_some())
        }
        _ => false,
    }) || match node {
        N::DropStmt(d) => d.objects.iter().any(|o| match o.node.as_ref() {
            Some(N::List(l)) => l.items.len() >= 2,
            _ => false,
        }),
        N::CommentStmt(_)
        | N::RenameStmt(_)
        | N::AlterSeqStmt(_)
        | N::CreateStmt(_)
        | N::CreateSeqStmt(_)
        | N::ViewStmt(_)
        | N::CreateTableAsStmt(_) => true,
        N::AlterTableStmt(_) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<R>(path: &str, schemas: &[&str], rels: &[&str], f: impl FnOnce() -> R) -> R {
        let mut settings = std::collections::HashMap::new();
        settings.insert("search_path".to_string(), path.to_string());
        set_session_context("db", settings);
        set_user_schemas(schemas.iter().map(|s| s.to_string()).collect());
        set_user_relations(rels.iter().map(|r| (r.to_string(), 1, false)).collect());
        let out = f();
        set_user_schemas(Vec::new());
        set_user_relations(Vec::new());
        set_session_context("db", std::collections::HashMap::new());
        out
    }

    fn deparsed(sql: &str) -> String {
        let mut tree = pg_query::parse(sql).unwrap().protobuf;
        let n = tree.stmts[0].stmt.as_mut().unwrap().node.as_mut().unwrap();
        qualify(n).unwrap();
        tree.deparse().unwrap()
    }

    #[test]
    fn keys() {
        assert_eq!(relation_key("public", "t"), "t");
        assert_eq!(relation_key("s", "t"), "s.t");
        with_env("public", &["s"], &[], || {
            assert_eq!(split_key("s.t"), ("s".into(), "t".into()));
            assert_eq!(split_key("x.t"), ("public".into(), "x.t".into()));
            assert_eq!(split_key("t"), ("public".into(), "t".into()));
        });
    }

    #[test]
    fn qualified_reference_becomes_key() {
        with_env("\"$user\", public", &["s"], &["s.t", "t"], || {
            assert_eq!(
                deparsed("select s.t.a, t.b from s.t"),
                "SELECT t.a, t.b FROM \"s.t\" t"
            );
            assert_eq!(deparsed("select * from t"), "SELECT * FROM t");
            assert_eq!(deparsed("select * from public.t"), "SELECT * FROM public.t");
        });
    }

    #[test]
    fn search_path_resolution_order() {
        with_env("s, public", &["s"], &["s.t", "t", "u"], || {
            assert_eq!(deparsed("select * from t"), "SELECT * FROM \"s.t\" t");
            assert_eq!(deparsed("select * from u"), "SELECT * FROM u");
            // A CTE shadows a relation.
            assert_eq!(
                deparsed("with t as (select 1) select * from t"),
                "WITH t AS (SELECT 1) SELECT * FROM t"
            );
        });
        with_env("public, s", &["s"], &["s.t", "t"], || {
            assert_eq!(deparsed("select * from t"), "SELECT * FROM t");
        });
    }

    #[test]
    fn create_lands_in_first_existing_schema() {
        with_env("nope, s, public", &["s"], &[], || {
            assert_eq!(
                deparsed("create table t (a int)"),
                "CREATE TABLE \"s.t\" (a int)"
            );
        });
        with_env("s", &["s"], &[], || {
            assert_eq!(
                deparsed("create table public.t (a int)"),
                "CREATE TABLE public.t (a int)"
            );
        });
        with_env("nope", &[], &[], || {
            let mut node = pg_query::parse("create table t (a int)")
                .unwrap()
                .protobuf
                .stmts[0]
                .stmt
                .as_ref()
                .unwrap()
                .node
                .clone()
                .unwrap();
            let err = qualify(&mut node).unwrap_err();
            assert!(matches!(err, Error::Sqlstate("3F000", _)));
        });
        with_env("public", &[], &[], || {
            let mut node = pg_query::parse("create table nope.t (a int)")
                .unwrap()
                .protobuf
                .stmts[0]
                .stmt
                .as_ref()
                .unwrap()
                .node
                .clone()
                .unwrap();
            let err = qualify(&mut node).unwrap_err();
            assert!(
                matches!(err, Error::Sqlstate("3F000", m) if m == "schema \"nope\" does not exist")
            );
        });
    }

    #[test]
    fn drop_names_fold_to_keys() {
        with_env("public", &["s"], &["s.t"], || {
            assert_eq!(deparsed("drop table s.t, u"), "DROP TABLE \"s.t\", u");
        });
    }

    #[test]
    fn foreign_key_target() {
        with_env("public", &["s"], &["s.p"], || {
            assert_eq!(
                deparsed("create table s.c (a int references s.p (id))"),
                "CREATE TABLE \"s.c\" (a int REFERENCES \"s.p\" (id))"
            );
        });
    }
}
