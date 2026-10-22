//! `ALTER DEFAULT PRIVILEGES` (RP-067): the privileges a role's NEW objects
//! start with, PostgreSQL's `pg_default_acl`.
//!
//! Each statement's GRANT or REVOKE is kept, in order, under the roles and
//! schemas it names (`__sql_default_acl__`). When one of those roles creates
//! a table, sequence, function, type or schema -- in one of those schemas,
//! or anywhere for an entry without one -- the kept actions are replayed on
//! the new object, which leaves it with the ACL `get_user_default_acl`
//! computes. `pg_default_acl` shows the ACL each (role, schema, kind) has
//! come to: from the kind's hard-wired default (`acldefault`) for a global
//! entry, from nothing for a schema's.

use super::*;

pub(crate) const DEFAULT_ACL_COLLECTION: &str = "__sql_default_acl__";

/// `pg_default_acl.defaclobjtype` for a GRANT's object kind.
fn objtype(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "table" => "r",
        "sequence" => "S",
        "function" | "procedure" | "routine" => "f",
        "type" | "domain" => "T",
        "schema" => "n",
        _ => return None,
    })
}

/// The privilege letters a kind's objects can hold, in `aclitem` order.
fn all_letters(kind: &str) -> &'static str {
    match kind {
        "r" => "arwdDxt",
        "S" => "rwU",
        "f" => "X",
        "T" => "U",
        _ => "UC",
    }
}

fn letter(privilege: &str) -> Option<char> {
    Some(match privilege {
        "SELECT" => 'r',
        "INSERT" => 'a',
        "UPDATE" => 'w',
        "DELETE" => 'd',
        "TRUNCATE" => 'D',
        "REFERENCES" => 'x',
        "TRIGGER" => 't',
        "EXECUTE" => 'X',
        "USAGE" => 'U',
        "CREATE" => 'C',
        _ => return None,
    })
}

impl PgHandler {
    /// `ALTER DEFAULT PRIVILEGES`: check its roles and schemas, then keep the
    /// action under each (role, schema).
    pub(crate) fn alter_default_privileges(
        &self,
        roles: &[String],
        schemas: &[String],
        action: &Statement,
    ) -> PgWireResult<()> {
        let Statement::Grant {
            is_grant,
            privileges,
            kind,
            grantees,
            grant_option,
            ..
        } = action
        else {
            return Err(Self::user_error(
                "0A000",
                "this ALTER DEFAULT PRIVILEGES action is not supported".into(),
            ));
        };
        let Some(objtype) = objtype(kind) else {
            return Err(Self::user_error(
                "0A000",
                format!("ALTER DEFAULT PRIVILEGES on {kind} objects is not supported"),
            ));
        };
        let me = self.current_role_name();
        let roles: Vec<String> = if roles.is_empty() {
            vec![me.clone()]
        } else {
            roles.to_vec()
        };
        for r in &roles {
            if self.role(r)?.is_none() && *r != self.session_user_name() {
                return Err(Self::user_error(
                    "42704",
                    format!("role \"{r}\" does not exist"),
                ));
            }
            // A member of the role may set its defaults (`has_privs_of_role`).
            if *r != me && !self.role_is_member(&me, r) {
                return Err(Self::user_error("42501", "permission denied".into()));
            }
        }
        for s in schemas {
            if !secantus_pgplan::schemas::schema_exists(s) {
                return Err(Self::user_error(
                    "3F000",
                    format!("schema \"{s}\" does not exist"),
                ));
            }
        }
        for g in grantees {
            if g != "PUBLIC" && g.to_ascii_lowercase() != "public" && self.role(g)?.is_none() {
                return Err(Self::user_error(
                    "42704",
                    format!("role \"{g}\" does not exist"),
                ));
            }
        }
        self.ensure_collection(DEFAULT_ACL_COLLECTION)?;
        let seq = self
            .default_acl_docs()
            .iter()
            .filter_map(|d| d.get_i64("seq").ok())
            .max()
            .unwrap_or(0);
        let schema_list: Vec<String> = if schemas.is_empty() {
            vec![String::new()]
        } else {
            schemas.to_vec()
        };
        let mut docs = Vec::new();
        let mut n = seq;
        for r in &roles {
            for s in &schema_list {
                n += 1;
                let d = bson::doc! {
                    "_id": n,
                    "seq": n,
                    "role": r,
                    "schema": s,
                    "objtype": objtype,
                    "kind": kind,
                    "is_grant": *is_grant,
                    "privileges": privileges.clone(),
                    "grantees": grantees.clone(),
                    "grant_option": *grant_option,
                };
                docs.push(encode_doc(&d).map_err(|e| Self::storage_err("could not encode", e))?);
            }
        }
        self.storage
            .insert(self.db(), DEFAULT_ACL_COLLECTION, docs, true)
            .map_err(|e| Self::storage_err("could not record the default privileges", e))?;
        Ok(())
    }

    fn default_acl_docs(&self) -> Vec<Document> {
        let Ok(true) = self
            .storage
            .collection_exists(self.db(), DEFAULT_ACL_COLLECTION)
        else {
            return Vec::new();
        };
        let mut docs: Vec<Document> = self
            .storage
            .find_matching(self.db(), DEFAULT_ACL_COLLECTION, &Document::new())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .collect();
        docs.sort_by_key(|d| d.get_i64("seq").unwrap_or(0));
        docs
    }

    /// A new object's defaults: replay the kept actions for the creating
    /// role, of this kind, global or in the object's schema, in order.
    pub(crate) fn apply_default_privileges(
        &self,
        kind: &str,
        object: &str,
        schema: &str,
    ) -> PgWireResult<()> {
        let Some(objtype) = objtype(kind) else {
            return Ok(());
        };
        let me = self.current_role_name();
        let docs = self.default_acl_docs();
        let actions: Vec<&Document> = docs
            .iter()
            .filter(|d| {
                d.get_str("role") == Ok(me.as_str())
                    && d.get_str("objtype") == Ok(objtype)
                    && matches!(d.get_str("schema"), Ok(s) if s.is_empty() || s == schema)
            })
            .collect();
        let strings = |d: &Document, k: &str| -> Vec<String> {
            d.get_array(k)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        for d in actions {
            self.grant(
                d.get_bool("is_grant").unwrap_or(true),
                &strings(d, "privileges"),
                &[],
                kind,
                &[object.to_string()],
                false,
                &strings(d, "grantees"),
                d.get_bool("grant_option").unwrap_or(false),
            )?;
        }
        Ok(())
    }

    /// `pg_default_acl`: each (role, schema, kind) whose ACL is not the
    /// default, as the replayed actions leave it.
    pub(crate) fn pg_default_acl_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let docs = self.default_acl_docs();
        let mut keys: Vec<(String, String, String)> = Vec::new();
        for d in &docs {
            let key = (
                d.get_str("role").unwrap_or_default().to_string(),
                d.get_str("schema").unwrap_or_default().to_string(),
                d.get_str("objtype").unwrap_or_default().to_string(),
            );
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let namespaces = self.namespaces();
        let mut out = Vec::new();
        for (role, schema, objtype) in keys {
            // grantee -> (letters, with grant option)
            let mut acl: Vec<(String, Vec<(char, bool)>)> = Vec::new();
            let full: Vec<(char, bool)> =
                all_letters(&objtype).chars().map(|c| (c, false)).collect();
            if schema.is_empty() {
                acl.push((role.clone(), full.clone()));
                if matches!(objtype.as_str(), "f" | "T") {
                    acl.push((String::new(), full.clone()));
                }
            }
            for d in docs.iter().filter(|d| {
                d.get_str("role") == Ok(role.as_str())
                    && d.get_str("schema") == Ok(schema.as_str())
                    && d.get_str("objtype") == Ok(objtype.as_str())
            }) {
                let grant = d.get_bool("is_grant").unwrap_or(true);
                let option = d.get_bool("grant_option").unwrap_or(false);
                let privs: Vec<char> = match d.get_array("privileges") {
                    Ok(a) if !a.is_empty() => a
                        .iter()
                        .filter_map(|v| v.as_str().and_then(letter))
                        .collect(),
                    _ => all_letters(&objtype).chars().collect(),
                };
                for g in d.get_array("grantees").into_iter().flatten() {
                    let g = g.as_str().unwrap_or_default();
                    let g = if g.eq_ignore_ascii_case("public") {
                        ""
                    } else {
                        g
                    };
                    let pos = match acl.iter().position(|(n, _)| n == g) {
                        Some(p) => p,
                        None => {
                            acl.push((g.to_string(), Vec::new()));
                            acl.len() - 1
                        }
                    };
                    let entry = &mut acl[pos].1;
                    for c in &privs {
                        if grant {
                            match entry.iter_mut().find(|(x, _)| x == c) {
                                Some(e) => e.1 |= option,
                                None => entry.push((*c, option)),
                            }
                        } else if option {
                            // REVOKE GRANT OPTION FOR: the privilege stays.
                            if let Some(e) = entry.iter_mut().find(|(x, _)| x == c) {
                                e.1 = false;
                            }
                        } else {
                            entry.retain(|(x, _)| x != c);
                        }
                    }
                }
            }
            acl.retain(|(_, e)| !e.is_empty());
            // An entry back at the default is no entry: a global one at the
            // kind's hard-wired ACL, a schema's at nothing (`SetDefaultACL`
            // removes the row).
            let mut default: Vec<(String, Vec<(char, bool)>)> = Vec::new();
            if schema.is_empty() {
                default.push((role.clone(), full.clone()));
                if matches!(objtype.as_str(), "f" | "T") {
                    default.push((String::new(), full.clone()));
                }
            }
            let same = |a: &[(String, Vec<(char, bool)>)], b: &[(String, Vec<(char, bool)>)]| {
                a.len() == b.len()
                    && a.iter().all(|(g, e)| {
                        b.iter().any(|(h, f)| {
                            g == h && e.len() == f.len() && e.iter().all(|x| f.contains(x))
                        })
                    })
            };
            if same(&acl, &default) {
                continue;
            }
            let order = all_letters(&objtype);
            let items: Vec<Bson> = acl
                .iter()
                .map(|(grantee, e)| {
                    let mut e = e.clone();
                    e.sort_by_key(|(c, _)| order.find(*c).unwrap_or(99));
                    let letters: String = e
                        .iter()
                        .map(|(c, o)| if *o { format!("{c}*") } else { c.to_string() })
                        .collect();
                    Bson::String(format!("{grantee}={letters}/{role}"))
                })
                .collect();
            let nsp = if schema.is_empty() {
                0
            } else {
                namespaces
                    .iter()
                    .find(|(n, _)| *n == schema)
                    .map_or(0, |(_, o)| *o)
            };
            let mut r = Document::new();
            r.insert(
                f("oid"),
                Bson::Int64(Self::index_oid(&format!(
                    "defacl:{role}:{schema}:{objtype}"
                ))),
            );
            r.insert(
                f("defaclrole"),
                Bson::Int64(self.role(&role).ok().flatten().map_or(10, |r| r.oid)),
            );
            r.insert(f("defaclnamespace"), Bson::Int64(nsp));
            r.insert(f("defaclobjtype"), objtype.as_str());
            r.insert(f("defaclacl"), Bson::Array(items));
            out.push(r);
        }
        out
    }
}

/// `pg_default_acl`.
pub(crate) fn pg_default_acl_def() -> TableDef {
    TableDef::new(
        "pg_default_acl",
        vec![
            Column::new("oid", "oid", false),
            Column::new("defaclrole", "oid", false),
            Column::new("defaclnamespace", "oid", false),
            Column::new("defaclobjtype", secantus_pgplan::QUOTED_CHAR, false),
            Column::new("defaclacl", "aclitem[]", false),
        ],
    )
}
