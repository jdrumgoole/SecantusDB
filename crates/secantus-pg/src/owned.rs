//! What holds a role in place, read from the `pg_shdepend` rows this server
//! serves: DROP ROLE's `checkSharedDependencies`, `REASSIGN OWNED`'s
//! `shdepReassignOwned` and `DROP OWNED`'s `shdepDropOwned` (RP-072,
//! RP-134).

use super::*;

const PG_CLASS: i64 = 1259;
const PG_PROC: i64 = 1255;
const PG_TYPE: i64 = 1247;
const PG_NAMESPACE: i64 = 2615;
const PG_DATABASE: i64 = 1262;

/// `MAX_REPORTED_DEPS`: DROP ROLE's DETAIL names this many objects.
const MAX_REPORTED: usize = 100;

/// One user object of this database (or a shared one), as the statements
/// and messages about it name it.
struct ObjectName {
    /// `getObjectDescription`'s text: `table t`, `function f(integer)`.
    description: String,
    /// The word after ALTER / DROP / ON: `TABLE`, `MATERIALIZED VIEW`.
    keyword: &'static str,
    /// The object as a statement names it, qualified and quoted.
    sql: String,
    /// The order DROP OWNED removes it in: what may depend on an object
    /// goes before it.
    drop_rank: u8,
}

/// `(dbid, classid, objid, deptype)`: one `pg_shdepend` row.
type SharedDependency = (i64, i64, i64, String);

impl PgHandler {
    /// Every `pg_shdepend` row that names role `role`.
    fn shared_dependencies_on(&self, role: i64) -> Vec<SharedDependency> {
        let Some(def) = Self::virtual_table("pg_shdepend") else {
            return Vec::new();
        };
        let field = |n: &str| def.field_of(n).unwrap_or_default();
        let (dbid, classid, objid, refobjid, deptype) = (
            field("dbid"),
            field("classid"),
            field("objid"),
            field("refobjid"),
            field("deptype"),
        );
        self.virtual_rows("pg_shdepend", &Document::new())
            .unwrap_or_default()
            .iter()
            .filter(|r| r.get(&refobjid).and_then(bson_i64) == Some(role))
            .filter_map(|r| {
                Some((
                    r.get(&dbid).and_then(bson_i64)?,
                    r.get(&classid).and_then(bson_i64)?,
                    r.get(&objid).and_then(bson_i64)?,
                    r.get_str(&deptype).ok()?.to_string(),
                ))
            })
            .collect()
    }

    /// This database's user objects and the cluster's databases, by
    /// `(classid, oid)`.
    fn object_names(&self) -> PgWireResult<HashMap<(i64, i64), ObjectName>> {
        let text = |v: &Option<Bson>| {
            v.as_ref()
                .map(secantus_pgplan::value_text)
                .unwrap_or_default()
        };
        let quote = secantus_pgplan::scalar::quote_identifier;
        let mut out = HashMap::new();
        let (_, rows) = self.internal_query(
            "SELECT c.oid, c.relkind, c.relname, n.nspname, \
             pg_catalog.pg_table_is_visible(c.oid) FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.oid >= 16384",
        )?;
        for r in &rows {
            let [oid, kind, name, schema, visible] = r.as_slice() else {
                continue;
            };
            let Ok(oid) = text(oid).parse::<i64>() else {
                continue;
            };
            let (word, keyword, drop_rank) = match text(kind).as_str() {
                "r" | "p" => ("table", "TABLE", 2),
                "v" => ("view", "VIEW", 0),
                "m" => ("materialized view", "MATERIALIZED VIEW", 0),
                "S" => ("sequence", "SEQUENCE", 3),
                "f" => ("foreign table", "FOREIGN TABLE", 2),
                "c" => ("composite type", "TYPE", 4),
                _ => ("index", "INDEX", 2),
            };
            let qualified = format!("{}.{}", quote(&text(schema)), quote(&text(name)));
            let shown = if text(visible) == "true" || text(visible) == "t" {
                quote(&text(name))
            } else {
                qualified.clone()
            };
            out.insert(
                (PG_CLASS, oid),
                ObjectName {
                    description: format!("{word} {shown}"),
                    keyword,
                    sql: qualified,
                    drop_rank,
                },
            );
        }
        let (_, rows) = self.internal_query(
            "SELECT p.oid, p.prokind, p.oid::pg_catalog.regprocedure::text \
             FROM pg_catalog.pg_proc p WHERE p.oid >= 16384",
        )?;
        for r in &rows {
            let [oid, kind, signature] = r.as_slice() else {
                continue;
            };
            let Ok(oid) = text(oid).parse::<i64>() else {
                continue;
            };
            let keyword = match text(kind).as_str() {
                "p" => "PROCEDURE",
                "a" => "AGGREGATE",
                _ => "FUNCTION",
            };
            out.insert(
                (PG_PROC, oid),
                ObjectName {
                    description: format!("function {}", text(signature)),
                    keyword,
                    sql: text(signature),
                    drop_rank: 1,
                },
            );
        }
        let (_, rows) = self.internal_query(
            "SELECT t.oid, t.typtype, pg_catalog.format_type(t.oid, NULL) \
             FROM pg_catalog.pg_type t WHERE t.oid >= 16384",
        )?;
        for r in &rows {
            let [oid, kind, name] = r.as_slice() else {
                continue;
            };
            let Ok(oid) = text(oid).parse::<i64>() else {
                continue;
            };
            out.insert(
                (PG_TYPE, oid),
                ObjectName {
                    description: format!("type {}", text(name)),
                    keyword: if text(kind) == "d" { "DOMAIN" } else { "TYPE" },
                    sql: text(name),
                    drop_rank: 4,
                },
            );
        }
        let (_, rows) = self.internal_query(
            "SELECT oid, nspname FROM pg_catalog.pg_namespace WHERE oid >= 16384",
        )?;
        for r in &rows {
            let [oid, name] = r.as_slice() else {
                continue;
            };
            let Ok(oid) = text(oid).parse::<i64>() else {
                continue;
            };
            out.insert(
                (PG_NAMESPACE, oid),
                ObjectName {
                    description: format!("schema {}", text(name)),
                    keyword: "SCHEMA",
                    sql: quote(&text(name)),
                    drop_rank: 5,
                },
            );
        }
        for acl in self.parameter_acls() {
            out.insert(
                (parameter_acl::PG_PARAMETER_ACL, acl.oid),
                ObjectName {
                    description: format!("parameter {}", acl.name),
                    keyword: "PARAMETER",
                    sql: acl.name.clone(),
                    drop_rank: 0,
                },
            );
        }
        for db in self.databases.all(&self.storage)? {
            out.insert(
                (PG_DATABASE, db.oid),
                ObjectName {
                    description: format!("database {}", db.name),
                    keyword: "DATABASE",
                    sql: quote(&db.name),
                    drop_rank: 6,
                },
            );
        }
        Ok(out)
    }

    /// The oid of this connection's database.
    fn current_database_oid(&self) -> PgWireResult<i64> {
        Ok(self
            .databases
            .all(&self.storage)?
            .into_iter()
            .find(|d| d.name == self.db())
            .map_or(0, |d| d.oid))
    }

    fn existing_role_oid(&self, name: &str) -> PgWireResult<i64> {
        if let Some(r) = self.role(name)? {
            return Ok(r.oid);
        }
        if name == self.session_user_name() {
            return Ok(RoleInfo::BOOTSTRAP_OID);
        }
        Err(Self::user_error(
            "42704",
            format!("role \"{name}\" does not exist"),
        ))
    }

    /// DROP ROLE's `checkSharedDependencies`: 2BP01 while any object is
    /// owned by `name` or names it in an ACL. This database's objects and
    /// the shared ones are listed in oid order; another database's are
    /// counted.
    pub(crate) fn check_role_unreferenced(&self, name: &str) -> PgWireResult<()> {
        let role = self.existing_role_oid(name)?;
        let deps = self.shared_dependencies_on(role);
        if deps.is_empty() {
            return Ok(());
        }
        let here = self.current_database_oid()?;
        let names = self.object_names()?;
        let mut local: Vec<&SharedDependency> = deps
            .iter()
            .filter(|(db, ..)| *db == here || *db == 0)
            .collect();
        // `shared_dependency_comparator`: oid, then catalog, then deptype.
        local.sort_by(|a, b| (a.2, a.1, &a.3).cmp(&(b.2, b.1, &b.3)));
        let mut lines: Vec<String> = Vec::new();
        for (_, classid, objid, deptype) in local.iter().take(MAX_REPORTED) {
            let what = names
                .get(&(*classid, *objid))
                .map_or_else(|| format!("object {objid}"), |n| n.description.clone());
            lines.push(match deptype.as_str() {
                "o" => format!("owner of {what}"),
                "a" => format!("privileges for {what}"),
                _ => format!("target of {what}"),
            });
        }
        if local.len() > MAX_REPORTED {
            lines.push(format!(
                "and {} other objects (see server log for list)",
                local.len() - MAX_REPORTED
            ));
        }
        let mut remote: Vec<(i64, usize)> = Vec::new();
        for (db, ..) in deps.iter().filter(|(db, ..)| *db != here && *db != 0) {
            match remote.iter_mut().find(|(d, _)| d == db) {
                Some((_, n)) => *n += 1,
                None => remote.push((*db, 1)),
            }
        }
        let databases = self.databases.all(&self.storage)?;
        for (db, n) in remote {
            let name = databases
                .iter()
                .find(|d| d.oid == db)
                .map_or_else(|| db.to_string(), |d| d.name.clone());
            lines.push(if n == 1 {
                format!("{n} object in database {name}")
            } else {
                format!("{n} objects in database {name}")
            });
        }
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "2BP01".into(), // dependent_objects_still_exist
            format!("role \"{name}\" cannot be dropped because some objects depend on it"),
        );
        info.detail = Some(lines.join("\n"));
        Err(PgWireError::UserError(Box::new(info)))
    }

    /// `has_privs_of_role(GetUserId(), role)`, or `denied` (42501).
    fn require_privileges_of(&self, role: &str, denied: &str) -> PgWireResult<()> {
        let me = self.current_role_name();
        let held = self.has_role_call(&[
            Bson::String(me),
            Bson::String(role.to_string()),
            Bson::String("USAGE".into()),
        ])?;
        if held == Bson::Boolean(true) {
            Ok(())
        } else {
            Err(Self::user_error("42501", denied.to_string()))
        }
    }

    /// `REASSIGN OWNED BY roles TO to`: every object of this database, and
    /// every database, that one of `roles` owns becomes `to`'s.
    pub(crate) fn reassign_owned(&self, roles: &[String], to: &str) -> PgWireResult<Vec<Response>> {
        let mut olds = Vec::new();
        for r in roles {
            let name = self.grantee_name(r)?;
            olds.push((self.existing_role_oid(&name)?, name.clone()));
            self.require_privileges_of(&name, "permission denied to reassign objects")?;
        }
        let to = self.grantee_name(to)?;
        self.existing_role_oid(&to)?;
        self.require_privileges_of(&to, "permission denied to reassign objects")?;
        let here = self.current_database_oid()?;
        self.atomically(|| {
            for (oid, name) in &olds {
                if *oid == RoleInfo::BOOTSTRAP_OID {
                    return Err(Self::user_error(
                        "2BP01",
                        format!(
                            "cannot reassign ownership of objects owned by role {name} because they are required by the database system"
                        ),
                    ));
                }
                let names = self.object_names()?;
                let mut owned: Vec<(i64, i64)> = self
                    .shared_dependencies_on(*oid)
                    .into_iter()
                    .filter(|(db, _, _, kind)| (*db == here || *db == 0) && kind == "o")
                    .map(|(_, classid, objid, _)| (classid, objid))
                    .collect();
                owned.sort();
                owned.dedup();
                for key in owned {
                    let Some(object) = names.get(&key) else {
                        continue;
                    };
                    // `ATExecChangeOwner(..., recursing = true)`: a
                    // sequence a column owns is reassigned too.
                    if object.keyword == "SEQUENCE" {
                        let bare = object
                            .sql
                            .rsplit_once('.')
                            .map_or(object.sql.as_str(), |(_, b)| b)
                            .trim_matches('"')
                            .replace("\"\"", "\"");
                        self.set_sequence_owner(&bare, &to)?;
                        continue;
                    }
                    self.internal_sql(&format!(
                        "ALTER {} {} OWNER TO {}",
                        object.keyword,
                        object.sql,
                        secantus_pgplan::scalar::quote_identifier(&to)
                    ))?;
                }
            }
            Ok(())
        })?;
        bump_catalog_version();
        self.marks.note_catalog_changed();
        Ok(vec![Response::Execution(Tag::new("REASSIGN OWNED"))])
    }

    /// `DROP OWNED BY roles [CASCADE]`: every privilege granted to one of
    /// `roles` here (and on shared objects) is revoked, and every object of
    /// this database one of them owns is dropped -- all as one deletion,
    /// so an owned object that depends on another owned one is no
    /// obstacle.
    pub(crate) fn drop_owned(
        &self,
        roles: &[String],
        cascade: bool,
    ) -> PgWireResult<Vec<Response>> {
        let mut targets = Vec::new();
        for r in roles {
            let name = self.grantee_name(r)?;
            targets.push((self.existing_role_oid(&name)?, name.clone()));
            self.require_privileges_of(&name, "permission denied to drop objects")?;
        }
        let here = self.current_database_oid()?;
        let behaviour = if cascade { " CASCADE" } else { "" };
        self.atomically(|| {
            for (oid, name) in &targets {
                if *oid == RoleInfo::BOOTSTRAP_OID {
                    return Err(Self::user_error(
                        "2BP01",
                        format!(
                            "cannot drop objects owned by role {name} because they are required by the database system"
                        ),
                    ));
                }
                let names = self.object_names()?;
                let deps = self.shared_dependencies_on(*oid);
                // Privileges first: an object about to be dropped loses its
                // ACL with it.
                let mut acl: Vec<(i64, i64)> = deps
                    .iter()
                    .filter(|(db, _, _, kind)| (*db == here || *db == 0) && kind == "a")
                    .map(|(_, c, o, _)| (*c, *o))
                    .collect();
                acl.sort();
                acl.dedup();
                for key in acl {
                    let Some(object) = names.get(&key) else {
                        continue;
                    };
                    let on = match object.keyword {
                        "VIEW" | "MATERIALIZED VIEW" | "FOREIGN TABLE" => "TABLE",
                        k => k,
                    };
                    self.internal_sql(&format!(
                        "REVOKE ALL ON {on} {} FROM {} CASCADE",
                        object.sql,
                        secantus_pgplan::scalar::quote_identifier(name)
                    ))?;
                }
                let mut owned: Vec<(u8, i64, i64)> = deps
                    .iter()
                    .filter(|(db, _, _, kind)| *db == here && kind == "o")
                    .filter_map(|(_, c, o, _)| names.get(&(*c, *o)).map(|n| (n.drop_rank, *c, *o)))
                    .collect();
                owned.sort();
                owned.dedup();
                // Dropped one by one in an order that removes dependents
                // first; an object another owned object still needs waits
                // for the next pass, so only a dependent outside the set
                // stops the drop.
                let mut pending: Vec<&ObjectName> =
                    owned.iter().filter_map(|(_, c, o)| names.get(&(*c, *o))).collect();
                while !pending.is_empty() {
                    let mut blocked = Vec::new();
                    let mut first_error = None;
                    for object in &pending {
                        let sql = format!(
                            "DROP {} IF EXISTS {}{behaviour}",
                            object.keyword, object.sql
                        );
                        if let Err(e) = self.internal_sql(&sql) {
                            let dependents = matches!(
                                &e,
                                PgWireError::UserError(info) if info.code == "2BP01"
                            );
                            if !dependents {
                                return Err(e);
                            }
                            first_error.get_or_insert(e);
                            blocked.push(*object);
                        }
                    }
                    if blocked.len() == pending.len() {
                        return Err(first_error.expect("a blocked drop has its error"));
                    }
                    pending = blocked;
                }
            }
            Ok(())
        })?;
        bump_catalog_version();
        self.marks.note_catalog_changed();
        Ok(vec![Response::Execution(Tag::new("DROP OWNED"))])
    }
}
