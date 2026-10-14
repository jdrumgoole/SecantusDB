//! What an authenticated user may do, decided as mongod decides it: the
//! union of the privileges of every role the user holds and every role those
//! inherit, matched against the resource a command works on.
//!
//! Every rule here was measured on mongod 8.2.11 with
//! `tools/probes/auth_enforcement.py` (2026-10-14): 62 users holding one
//! role each -- every built-in role and stored roles naming one collection,
//! one database, a collection name on every database, every database, the
//! cluster and any resource -- each running 150 commands.
//!
//! A privilege's resource is a PATTERN, and a check asks whether the user's
//! privileges on the patterns that cover the target carry the actions the
//! command needs. The patterns that cover a target (mongod's
//! `buildResourceSearchList`):
//!
//! - a namespace `db.coll`: its own exact pattern; `{db: "", collection:
//!   "coll"}`; `anyResource`; and, when `coll` is not a system collection,
//!   `{db, collection: ""}` and -- unless `db` is `local` or `config` --
//!   `{db: "", collection: ""}`. So a `read` on `ae` finds `ae.c1` and not
//!   `ae.system.profile`, and `readAnyDatabase` finds `ae.c1` and not
//!   `local.oplog.rs`;
//! - a database: its own pattern, `anyResource`, and `{db: "", collection:
//!   ""}` unless it is `local` or `config`;
//! - the cluster: `cluster` and `anyResource`.
//!
//! The set is computed from the user records and the role graph, and cached
//! on the connection until a user or role command changes anything
//! ([`crate::params::ServerParams::invalidate_user_cache`]).

use std::sync::Arc;

use crate::privileges::{PrivilegeSet, Resource};
use crate::{CommandContext, CommandError};

/// The effective privileges of a connection's authenticated users.
#[derive(Debug, Default)]
pub(crate) struct Authz {
    /// The `(db, user)` principals authenticated on the connection.
    pub(crate) principals: Vec<(String, String)>,
    /// The roles those users hold directly, as `(role, db)`.
    pub(crate) roles: Vec<(String, String)>,
    pub(crate) privileges: PrivilegeSet,
}

/// A computed [`Authz`], good until the server's user cache changes.
#[derive(Debug)]
pub(crate) struct CachedAuthz {
    pub(crate) generation: u64,
    pub(crate) authz: Arc<Authz>,
}

fn is_system(collection: &str) -> bool {
    collection.starts_with("system.")
}

fn is_special_db(db: &str) -> bool {
    db == "local" || db == "config"
}

/// The patterns a privilege may carry to cover `target`.
pub(crate) fn search_list(target: &Resource) -> Vec<Resource> {
    let mut out = Vec::with_capacity(5);
    match target {
        Resource::Ns { db, collection } if !collection.is_empty() => {
            if !is_system(collection) {
                if !is_special_db(db) {
                    out.push(Resource::Ns {
                        db: String::new(),
                        collection: String::new(),
                    });
                }
                out.push(Resource::Ns {
                    db: db.clone(),
                    collection: String::new(),
                });
            }
            out.push(Resource::Ns {
                db: String::new(),
                collection: collection.clone(),
            });
        }
        Resource::Ns { db, .. } => {
            if !is_special_db(db) {
                out.push(Resource::Ns {
                    db: String::new(),
                    collection: String::new(),
                });
            }
        }
        Resource::SystemBuckets { db, bucket } => {
            if !bucket.is_empty() {
                out.push(Resource::SystemBuckets {
                    db: db.clone(),
                    bucket: String::new(),
                });
            }
            if !db.is_empty() {
                out.push(Resource::SystemBuckets {
                    db: String::new(),
                    bucket: String::new(),
                });
            }
        }
        Resource::Cluster | Resource::Any => {}
    }
    out.push(Resource::Any);
    out.push(target.clone());
    out
}

impl Authz {
    /// Whether the privileges carry every one of `actions` on `target`.
    pub(crate) fn is_authorized(&self, target: &Resource, actions: &[&str]) -> bool {
        let mut unmet: Vec<&str> = actions.to_vec();
        for pattern in search_list(target) {
            if let Some(held) = self.privileges.actions_on(&pattern) {
                unmet.retain(|a| !held.iter().any(|h| h == a));
            }
            if unmet.is_empty() {
                return true;
            }
        }
        unmet.is_empty()
    }

    /// Whether the privileges carry any action at all on `target`
    /// (mongod's `isAuthorizedForAnyActionOnResource`), which is what
    /// `listCollections` with `authorizedCollections` lists by.
    pub(crate) fn any_action_on(&self, target: &Resource) -> bool {
        search_list(target)
            .iter()
            .any(|pattern| self.privileges.actions_on(pattern).is_some())
    }

    /// Whether the privileges carry any action on any resource of `db`
    /// (mongod's `isAuthorizedForAnyActionOnAnyResourceInDB`): what
    /// `listDatabases` lists by, and what lets `listCollections` with
    /// `nameOnly` and `authorizedCollections` run at all.
    pub(crate) fn any_action_in_db(&self, db: &str) -> bool {
        self.privileges.resources().any(|r| match r {
            Resource::Any => true,
            Resource::Cluster => false,
            Resource::Ns {
                db: pattern_db,
                collection,
            } => {
                if pattern_db.is_empty() {
                    // Every collection name on every database; every
                    // normal collection of every normal database.
                    !collection.is_empty() || !is_special_db(db)
                } else {
                    pattern_db == db
                }
            }
            Resource::SystemBuckets { db: pattern_db, .. } => {
                pattern_db.is_empty() || pattern_db == db
            }
        })
    }

    /// Whether `(db, user)` is one of the connection's authenticated users:
    /// mongod lets a user view and change its own record, and read and kill
    /// its own cursors.
    pub(crate) fn is_self(&self, db: &str, user: &str) -> bool {
        self.principals.iter().any(|(d, u)| d == db && u == user)
    }

    /// Whether the connection's users hold `(role, db)` directly, which is
    /// what lets `rolesInfo` describe a role the user has.
    pub(crate) fn holds_role(&self, role: &str, db: &str) -> bool {
        self.roles.iter().any(|(r, d)| r == role && d == db)
    }
}

/// Whether the cursor opened by `opened_by` may be read on or killed by a
/// session whose users are `now` (mongod's `isCoauthorizedWith`): the same
/// users, or none on either side.
pub(crate) fn coauthorized(opened_by: &[(String, String)], now: &[(String, String)]) -> bool {
    let mut a = opened_by.to_vec();
    let mut b = now.to_vec();
    a.sort();
    b.sort();
    a == b
}

/// The effective privileges of the calling connection, computed from its
/// authenticated users' records and the role graph, and cached until a user
/// or role changes. A connection with no authenticated user has none.
pub(crate) fn effective(ctx: &CommandContext) -> Result<Arc<Authz>, CommandError> {
    let Some(conn) = ctx.conn_auth.as_ref() else {
        return Ok(Arc::new(Authz::default()));
    };
    let generation = ctx
        .server_params
        .as_ref()
        .map(|p| p.user_cache_generation());
    let principals = {
        let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(generation), Some(cached)) = (generation, &guard.privileges) {
            if cached.generation == generation && cached.authz.principals == guard.authenticated {
                return Ok(cached.authz.clone());
            }
        }
        guard.authenticated.clone()
    };
    let mut roles: Vec<(String, String)> = Vec::new();
    for (db, user) in &principals {
        for pair in crate::auth::lookup_roles(ctx, db, user)
            .map(|r| crate::umc::role_pairs(&r))
            .unwrap_or_default()
        {
            if !roles.contains(&pair) {
                roles.push(pair);
            }
        }
    }
    let inherited = crate::umc::inherit(ctx, &roles)?;
    let authz = Arc::new(Authz {
        principals,
        roles: roles.clone(),
        privileges: inherited.privileges,
    });
    let mut guard = conn.lock().unwrap_or_else(|e| e.into_inner());
    guard.effective_roles = roles;
    guard.privileges = generation.map(|generation| CachedAuthz {
        generation,
        authz: authz.clone(),
    });
    Ok(authz)
}

/// The calling connection's privileges when access control is on and a
/// user is authenticated; `None` when every command is allowed.
pub(crate) fn enforced(ctx: &CommandContext) -> Result<Option<Arc<Authz>>, CommandError> {
    if !ctx.require_auth {
        return Ok(None);
    }
    let authenticated = ctx.conn_auth.as_ref().is_some_and(|a| {
        a.lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_authenticated()
    });
    if !authenticated {
        return Ok(None);
    }
    effective(ctx).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileges::Privilege;

    fn ns(db: &str, collection: &str) -> Resource {
        Resource::Ns {
            db: db.into(),
            collection: collection.into(),
        }
    }

    fn with(privileges: &[(Resource, &[&str])]) -> Authz {
        let mut set = PrivilegeSet::default();
        for (resource, actions) in privileges {
            set.add(&Privilege {
                resource: resource.clone(),
                actions: actions.iter().map(|a| a.to_string()).collect(),
            });
        }
        Authz {
            principals: vec![("admin".into(), "u".into())],
            roles: Vec::new(),
            privileges: set,
        }
    }

    /// `{db: "ae", collection: ""}` covers the normal collections of `ae`
    /// and none of its system collections (user `c_dbFind`).
    #[test]
    fn database_pattern_skips_system_collections() {
        let az = with(&[(ns("ae", ""), &["find"])]);
        assert!(az.is_authorized(&ns("ae", "c1"), &["find"]));
        assert!(az.is_authorized(&ns("ae", "nope"), &["find"]));
        assert!(!az.is_authorized(&ns("ae", "system.js"), &["find"]));
        assert!(!az.is_authorized(&ns("ae2", "c1"), &["find"]));
        assert!(!az.is_authorized(&ns("ae", "c1"), &["find", "insert"]));
    }

    /// `{db: "", collection: ""}` covers every normal collection of every
    /// database but `local` and `config` (user `c_everyFind`).
    #[test]
    fn any_normal_pattern_skips_local_and_config() {
        let az = with(&[(ns("", ""), &["find"])]);
        assert!(az.is_authorized(&ns("ae", "c1"), &["find"]));
        assert!(az.is_authorized(&ns("admin", "c1"), &["find"]));
        assert!(!az.is_authorized(&ns("local", "oplog.rs"), &["find"]));
        assert!(!az.is_authorized(&ns("config", "settings"), &["find"]));
        assert!(!az.is_authorized(&ns("ae", "system.profile"), &["find"]));
        assert!(az.is_authorized(&ns("ae", ""), &["find"]));
        assert!(!az.is_authorized(&ns("local", ""), &["find"]));
    }

    /// `{db: "", collection: "c1"}` covers that name on every database,
    /// `local` and `config` included, and a system collection's name too
    /// (users `c_nameFind`, `c_nameSysJs`).
    #[test]
    fn collection_name_pattern_covers_every_database() {
        let az = with(&[(ns("", "c1"), &["find"]), (ns("", "system.js"), &["find"])]);
        assert!(az.is_authorized(&ns("ae", "c1"), &["find"]));
        assert!(az.is_authorized(&ns("local", "c1"), &["find"]));
        assert!(!az.is_authorized(&ns("ae", "c2"), &["find"]));
        assert!(az.is_authorized(&ns("ae", "system.js"), &["find"]));
        assert!(!az.is_authorized(&ns("ae", ""), &["find"]));
    }

    /// Actions on one target may come from several patterns together.
    #[test]
    fn actions_join_across_patterns() {
        let az = with(&[(ns("ae", "c1"), &["find"]), (ns("ae", ""), &["insert"])]);
        assert!(az.is_authorized(&ns("ae", "c1"), &["find", "insert"]));
        assert!(!az.is_authorized(&ns("ae", "c2"), &["find", "insert"]));
    }

    /// `anyResource` covers the cluster and `cluster` covers only it.
    #[test]
    fn cluster_is_covered_by_cluster_and_any() {
        let any = with(&[(Resource::Any, &["find", "serverStatus"])]);
        assert!(any.is_authorized(&Resource::Cluster, &["serverStatus"]));
        assert!(any.is_authorized(&ns("local", "system.replset"), &["find"]));
        let cluster = with(&[(Resource::Cluster, &["serverStatus"])]);
        assert!(cluster.is_authorized(&Resource::Cluster, &["serverStatus"]));
        assert!(!cluster.is_authorized(&ns("ae", "c1"), &["serverStatus"]));
        assert!(!cluster.any_action_in_db("ae"));
    }

    /// What `listDatabases` lists for a user without `listDatabases` on the
    /// cluster: the databases the user has any action in.
    #[test]
    fn any_action_in_db_follows_the_patterns() {
        assert!(with(&[(ns("", ""), &["find"])]).any_action_in_db("ae"));
        assert!(!with(&[(ns("", ""), &["find"])]).any_action_in_db("local"));
        assert!(with(&[(ns("", "c1"), &["find"])]).any_action_in_db("config"));
        assert!(with(&[(ns("ae", "c1"), &["insert"])]).any_action_in_db("ae"));
        assert!(!with(&[(ns("ae", "c1"), &["insert"])]).any_action_in_db("ae2"));
        assert!(with(&[(ns("local", ""), &["find"])]).any_action_in_db("local"));
        assert!(with(&[(Resource::Any, &["find"])]).any_action_in_db("local"));
    }

    /// What `listCollections` with `authorizedCollections` lists: a
    /// collection the user has any action on, which for a system collection
    /// takes `anyResource` or its name.
    #[test]
    fn any_action_on_a_namespace() {
        let az = with(&[(ns("ae", ""), &["listCollections"])]);
        assert!(az.any_action_on(&ns("ae", "c1")));
        assert!(!az.any_action_on(&ns("ae", "system.views")));
        assert!(
            with(&[(Resource::Any, &["listCollections"])]).any_action_on(&ns("ae", "system.views"))
        );
        assert!(with(&[(ns("ae", "c1"), &["insert"])]).any_action_on(&ns("ae", "c1")));
        assert!(!with(&[(ns("ae", "c1"), &["insert"])]).any_action_on(&ns("ae", "c2")));
    }

    #[test]
    fn coauthorized_is_the_same_users() {
        let a = vec![("admin".to_string(), "u".to_string())];
        let b = vec![("admin".to_string(), "v".to_string())];
        assert!(coauthorized(&a, &a));
        assert!(!coauthorized(&a, &b));
        assert!(!coauthorized(&a, &[]));
        assert!(coauthorized(&[], &[]));
    }
}
