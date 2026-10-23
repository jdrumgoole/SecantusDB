//! `GRANT / REVOKE { SET | ALTER SYSTEM } ON PARAMETER`, kept as
//! PostgreSQL 15 keeps it: a shared `pg_parameter_acl` row per parameter
//! with a grant, its `paracl` the bootstrap superuser's `sA` and each
//! grant's privileges and grant options with its grantor. A role that holds
//! the grant option grants on in its own name, and REVOKE ... CASCADE takes
//! what it granted with it (2BP01 `dependent privileges exist` without).
//!
//! `SET` lets a role set a parameter of `superuser` context, which is 42501
//! for any other role (`set_config_option`'s `pg_parameter_aclcheck`), and
//! `ALTER SYSTEM` lets it run ALTER SYSTEM on the parameter.

use super::*;

/// The cluster's grants, one record per parameter, in the registry's
/// namespace as `pg_parameter_acl` is shared.
pub(crate) const PARAMETER_ACL_COLLECTION: &str = "__sql_parameter_acl__";

/// `pg_parameter_acl`'s oid, the class its rows depend on roles as.
pub(crate) const PG_PARAMETER_ACL: i64 = 6243;

/// The privileges a parameter has, in PostgreSQL's order, with their
/// `aclitem` letters.
const PRIVILEGES: [(&str, char); 2] = [("SET", 's'), ("ALTER SYSTEM", 'A')];

/// One grant: a grantee's privileges (and grant options) from one grantor.
#[derive(Debug, Clone, PartialEq)]
struct Grant {
    grantee: String,
    grantor: String,
    privileges: Vec<String>,
    options: Vec<String>,
}

/// A parameter's row.
#[derive(Debug, Clone)]
pub(crate) struct ParameterAcl {
    pub name: String,
    pub oid: i64,
    grants: Vec<Grant>,
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

fn order(privileges: &mut Vec<String>) {
    privileges.sort_by_key(|p| PRIVILEGES.iter().position(|(w, _)| w == p));
    privileges.dedup();
}

impl PgHandler {
    /// Every parameter's row.
    pub(crate) fn parameter_acls(&self) -> Vec<ParameterAcl> {
        let Ok(docs) = self.storage.find_matching(
            DatabaseRegistry::NAMESPACE,
            PARAMETER_ACL_COLLECTION,
            &Document::new(),
        ) else {
            return Vec::new();
        };
        let mut out: Vec<ParameterAcl> = docs
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .filter_map(|d| {
                Some(ParameterAcl {
                    name: d.get_str("_id").ok()?.to_string(),
                    oid: d.get("oid").and_then(bson_i64)?,
                    grants: d
                        .get_array("grants")
                        .ok()?
                        .iter()
                        .filter_map(|g| g.as_document())
                        .map(|g| Grant {
                            grantee: g.get_str("grantee").unwrap_or_default().to_string(),
                            grantor: g.get_str("grantor").unwrap_or_default().to_string(),
                            privileges: strings(g, "privileges"),
                            options: strings(g, "options"),
                        })
                        .collect(),
                })
            })
            .collect();
        out.sort_by_key(|a| a.oid);
        out
    }

    fn write_parameter_acl(&self, acl: &ParameterAcl) -> PgWireResult<()> {
        let err = |e| Self::storage_err("could not record the parameter's privileges", e);
        let ns = DatabaseRegistry::NAMESPACE;
        if !self
            .storage
            .collection_exists(ns, PARAMETER_ACL_COLLECTION)
            .map_err(err)?
        {
            self.storage
                .create_collection(ns, PARAMETER_ACL_COLLECTION)
                .map_err(err)?;
        }
        self.storage
            .delete_matching(
                ns,
                PARAMETER_ACL_COLLECTION,
                &bson::doc! {"_id": &acl.name},
                0,
                &Document::new(),
                None,
            )
            .map_err(err)?;
        // A parameter whose grants are all gone has no row.
        if acl.grants.is_empty() {
            return Ok(());
        }
        let grants: Vec<Bson> = acl
            .grants
            .iter()
            .map(|g| {
                Bson::Document(bson::doc! {
                    "grantee": &g.grantee,
                    "grantor": &g.grantor,
                    "privileges": g.privileges.clone(),
                    "options": g.options.clone(),
                })
            })
            .collect();
        let doc = bson::doc! {"_id": &acl.name, "oid": acl.oid, "grants": grants};
        let bytes = encode_doc(&doc)
            .map_err(|e| Self::storage_err("could not encode the parameter's privileges", e))?;
        self.storage
            .insert(ns, PARAMETER_ACL_COLLECTION, vec![bytes], true)
            .map_err(err)?;
        Ok(())
    }

    /// The name `pg_parameter_acl` keeps a parameter under: lower case, a
    /// known parameter or a custom (dotted) one; 42602 otherwise.
    fn parameter_acl_name(&self, name: &str) -> PgWireResult<String> {
        let lower = name.to_ascii_lowercase();
        if lower.contains('.') || self.check_setting_known(name).is_ok() {
            return Ok(lower);
        }
        Err(Self::user_error(
            "42602",
            format!("invalid parameter name \"{name}\""),
        ))
    }

    /// The bootstrap superuser, who owns every parameter.
    fn bootstrap_role_name() -> String {
        RoleInfo::bootstrap().name
    }

    /// Does `role` hold `privilege` on parameter `name` (lower case), or its
    /// grant option with `option`: a superuser, or a grant to it, a role it
    /// is a member of, or PUBLIC.
    pub(crate) fn parameter_privilege_held(
        &self,
        role: &str,
        name: &str,
        privilege: &str,
        option: bool,
    ) -> bool {
        if self.is_superuser(role) {
            return true;
        }
        self.parameter_acls()
            .iter()
            .filter(|a| a.name == name)
            .flat_map(|a| a.grants.iter())
            .any(|g| {
                let list = if option { &g.options } else { &g.privileges };
                (g.grantee.eq_ignore_ascii_case("PUBLIC") || self.role_is_member(role, &g.grantee))
                    && list.iter().any(|p| p == privilege)
            })
    }

    /// 42501 for a `SET` of a `superuser` parameter the role in effect may
    /// not set (`set_config_option`).
    pub(crate) fn check_set_privilege(&self, key: &str) -> PgWireResult<()> {
        if self
            .privileged_set
            .load(std::sync::atomic::Ordering::Relaxed)
            || pg15_settings::pg15_setting(key).map(|p| p.context) != Some("superuser")
        {
            return Ok(());
        }
        let me = self.current_role_name();
        if self.parameter_privilege_held(&me, &key.to_ascii_lowercase(), "SET", false) {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!("permission denied to set parameter \"{key}\""),
        ))
    }

    /// `GRANT` / `REVOKE` on parameters.
    pub(crate) fn grant_parameter(
        &self,
        is_grant: bool,
        privileges: &[String],
        names: &[String],
        grantees: &[String],
        grant_option: bool,
        cascade: bool,
    ) -> PgWireResult<()> {
        let all = privileges.is_empty() || privileges.iter().any(|p| p == "ALL");
        let wanted: Vec<String> = if all {
            PRIVILEGES.iter().map(|(w, _)| (*w).to_string()).collect()
        } else {
            let mut out = Vec::new();
            for p in privileges {
                if !PRIVILEGES.iter().any(|(w, _)| w == p) {
                    return Err(Self::user_error(
                        "0LP01",
                        format!("invalid privilege type {p} for parameter"),
                    ));
                }
                out.push(p.clone());
            }
            out
        };
        let me = self.current_role_name();
        let superuser = self.is_superuser(&me);
        let grantor = if superuser {
            Self::bootstrap_role_name()
        } else {
            me.clone()
        };
        let grantees: Vec<String> = grantees
            .iter()
            .map(|g| {
                if g.eq_ignore_ascii_case("public") {
                    return Ok("PUBLIC".to_string());
                }
                let g = self.grantee_name(g)?;
                if self.role(&g)?.is_none() && g != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{g}\" does not exist"),
                    ));
                }
                Ok(g)
            })
            .collect::<PgWireResult<_>>()?;
        for written in names {
            let name = self.parameter_acl_name(written)?;
            let mut acl = match self.parameter_acls().into_iter().find(|a| a.name == name) {
                Some(acl) => acl,
                None if is_grant => ParameterAcl {
                    name: name.clone(),
                    oid: self.mint_composite_oid()?,
                    grants: Vec::new(),
                },
                None => continue,
            };
            // What this grantor may give: everything for a superuser, what
            // it holds the grant option of otherwise.
            let can: Vec<String> = wanted
                .iter()
                .filter(|p| superuser || self.parameter_privilege_held(&me, &name, p, true))
                .cloned()
                .collect();
            // `restrict_and_check_grant`: no grant option to give is an
            // error only for a role holding no privilege on it at all.
            if can.is_empty() {
                let holds_any = PRIVILEGES
                    .iter()
                    .any(|(p, _)| self.parameter_privilege_held(&me, &name, p, false));
                if !holds_any {
                    return Err(Self::user_error(
                        "42501",
                        format!("permission denied for parameter {name}"),
                    ));
                }
                let (code, verb) = if is_grant {
                    ("01007", "no privileges were granted")
                } else {
                    ("01006", "no privileges could be revoked")
                };
                self.warning(code, format!("{verb} for \"{name}\""));
                continue;
            }
            if can.len() < wanted.len() && !all {
                let (code, verb) = if is_grant {
                    ("01007", "not all privileges were granted")
                } else {
                    ("01006", "not all privileges could be revoked")
                };
                self.warning(code, format!("{verb} for \"{name}\""));
            }
            for grantee in &grantees {
                if is_grant {
                    let at = match acl
                        .grants
                        .iter()
                        .position(|g| g.grantee == *grantee && g.grantor == grantor)
                    {
                        Some(i) => i,
                        None => {
                            acl.grants.push(Grant {
                                grantee: grantee.clone(),
                                grantor: grantor.clone(),
                                privileges: Vec::new(),
                                options: Vec::new(),
                            });
                            acl.grants.len() - 1
                        }
                    };
                    let g = &mut acl.grants[at];
                    g.privileges.extend(can.iter().cloned());
                    if grant_option {
                        g.options.extend(can.iter().cloned());
                    }
                    order(&mut g.privileges);
                    order(&mut g.options);
                } else {
                    revoke_from(&mut acl, grantee, &grantor, &can, grant_option, cascade)?;
                }
            }
            acl.grants
                .retain(|g| !g.privileges.is_empty() || !g.options.is_empty());
            self.write_parameter_acl(&acl)?;
        }
        Ok(())
    }

    /// `has_parameter_privilege([role,] parameter, privileges)`: NULL for a
    /// NULL argument, false for a parameter there is none of.
    pub(crate) fn has_parameter_privilege_call(&self, args: &[Bson]) -> PgWireResult<Bson> {
        if args.contains(&Bson::Null) {
            return Ok(Bson::Null);
        }
        let text = |v: &Bson| secantus_pgplan::value_text(v);
        let (role, param, privileges) = match args {
            [role, param, privileges] => {
                let r = text(role);
                if r != "public" && self.role(&r)?.is_none() && r != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{r}\" does not exist"),
                    ));
                }
                (r, text(param), text(privileges))
            }
            [param, privileges] => (self.current_role_name(), text(param), text(privileges)),
            _ => {
                return Err(Self::user_error(
                    "42883",
                    "function has_parameter_privilege does not exist".into(),
                ))
            }
        };
        let mut wanted = Vec::new();
        for written in privileges.split(',') {
            let written = written.trim();
            let upper = written.to_ascii_uppercase();
            let (p, option) = match upper.strip_suffix(" WITH GRANT OPTION") {
                Some(base) => (base.trim().to_string(), true),
                None => (upper.clone(), false),
            };
            if !PRIVILEGES.iter().any(|(w, _)| *w == p) {
                return Err(Self::user_error(
                    "22023",
                    format!("unrecognized privilege type: \"{written}\""),
                ));
            }
            wanted.push((p, option));
        }
        let Ok(name) = self.parameter_acl_name(&param) else {
            return Ok(Bson::Boolean(false));
        };
        Ok(Bson::Boolean(wanted.iter().any(|(p, o)| {
            self.parameter_privilege_held(&role, &name, p, *o)
        })))
    }

    /// `pg_parameter_acl`'s rows.
    pub(crate) fn parameter_acl_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |c: &str| def.field_of(c);
        let owner = Self::bootstrap_role_name();
        self.parameter_acls()
            .into_iter()
            .map(|a| {
                let mut items = vec![Bson::String(format!("{owner}=sA/{owner}"))];
                for g in &a.grants {
                    let mut letters = String::new();
                    for (word, letter) in PRIVILEGES {
                        if g.privileges.iter().any(|p| p == word) {
                            letters.push(letter);
                            if g.options.iter().any(|p| p == word) {
                                letters.push('*');
                            }
                        }
                    }
                    let grantee = if g.grantee == "PUBLIC" {
                        String::new()
                    } else {
                        secantus_pgplan::scalar::quote_identifier(&g.grantee)
                    };
                    items.push(Bson::String(format!(
                        "{grantee}={letters}/{}",
                        secantus_pgplan::scalar::quote_identifier(&g.grantor)
                    )));
                }
                let mut d = Document::new();
                for (column, value) in [
                    ("oid", Bson::Int64(a.oid)),
                    ("parname", Bson::String(a.name.clone())),
                    ("paracl", Bson::Array(items)),
                ] {
                    if let Some(field) = f(column) {
                        d.insert(field, value);
                    }
                }
                d
            })
            .collect()
    }

    /// `(parameter acl oid, role)` for every grant: `pg_shdepend`'s ACL rows
    /// on roles, grantors included.
    pub(crate) fn parameter_acl_roles(&self) -> Vec<(i64, String, String)> {
        self.parameter_acls()
            .into_iter()
            .flat_map(|a| {
                let oid = a.oid;
                let name = a.name.clone();
                a.grants.into_iter().flat_map(move |g| {
                    [
                        (oid, name.clone(), g.grantee),
                        (oid, name.clone(), g.grantor),
                    ]
                })
            })
            .collect()
    }
}

/// Take `privileges` (or with `option` only their grant options) granted by
/// `grantor` from `grantee`, and, with `cascade`, what `grantee` granted on
/// in its own name that relied on them; 2BP01 without it.
fn revoke_from(
    acl: &mut ParameterAcl,
    grantee: &str,
    grantor: &str,
    privileges: &[String],
    option: bool,
    cascade: bool,
) -> PgWireResult<()> {
    let Some(at) = acl
        .grants
        .iter()
        .position(|g| g.grantee == grantee && g.grantor == grantor)
    else {
        return Ok(());
    };
    // The grant options lost, which what the grantee granted relied on.
    let lost: Vec<String> = privileges
        .iter()
        .filter(|p| acl.grants[at].options.contains(p))
        .cloned()
        .collect();
    {
        let g = &mut acl.grants[at];
        g.options.retain(|p| !privileges.contains(p));
        if !option {
            g.privileges.retain(|p| !privileges.contains(p));
        }
    }
    let dependents: Vec<String> = acl
        .grants
        .iter()
        .filter(|g| g.grantor == grantee && g.privileges.iter().any(|p| lost.contains(p)))
        .map(|g| g.grantee.clone())
        .collect();
    if dependents.is_empty() {
        return Ok(());
    }
    if !cascade {
        let mut info = ErrorInfo::new(
            "ERROR".into(),
            "2BP01".into(),
            "dependent privileges exist".into(),
        );
        info.hint = Some("Use CASCADE to revoke them too.".into());
        return Err(PgWireError::UserError(Box::new(info)));
    }
    for d in dependents {
        revoke_from(acl, &d, grantee, &lost, false, true)?;
    }
    Ok(())
}
