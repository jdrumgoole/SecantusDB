//! `ALTER DATABASE d SET name = value`: a GUC default that every NEW session
//! of `d` starts with (PostgreSQL's `pg_db_role_setting`), never applied to
//! a session already open. Stored where the Python server keeps it --
//! `__sql_db_settings__`, one `{_id: <guc>, value: <text>}` per setting --
//! so either server reads what the other wrote.
//!
//! Also `pg_settings`, the view a client (pgjdbc's
//! `getDefaultTransactionIsolation`) reads a setting through.

use super::*;

impl PgHandler {
    pub(crate) const DB_SETTINGS_COLLECTION: &'static str = "__sql_db_settings__";

    /// `ALTER ROLE ... SET` defaults: one `{_id, role, database, name,
    /// value}` per setting, `role` / `database` empty for ALL. Kept in the
    /// cluster's registry, as `pg_db_role_setting` is shared.
    pub(crate) const ROLE_SETTINGS_COLLECTION: &'static str = "__sql_role_settings__";

    /// The defaults a NEW session of this database and login role starts
    /// with, by GUC name: `process_settings`' order, each overriding the one
    /// before -- every role's, the database's, the role's, the role's in
    /// this database.
    pub(crate) fn db_setting_defaults(&self) -> Vec<(String, String)> {
        self.db_setting_layers()
            .into_iter()
            .map(|(name, value, _)| (name, value))
            .collect()
    }

    /// [`Self::db_setting_defaults`], each with the `pg_settings.source`
    /// of the layer it came from.
    pub(crate) fn db_setting_layers(&self) -> Vec<(String, String, &'static str)> {
        let role = self.session_user_name();
        let db = self.db().to_string();
        let roles = self.role_setting_docs();
        let of = |r: &str, d: &str| -> Vec<(String, String)> {
            roles
                .iter()
                .filter(|(dr, dd, _, _)| dr == r && dd == d)
                .map(|(_, _, n, v)| (n.clone(), v.clone()))
                .collect()
        };
        let mut out: std::collections::BTreeMap<String, (String, &'static str)> =
            std::collections::BTreeMap::new();
        for (layer, source) in [
            (of("", ""), "global"),
            (self.db_setting_defaults_of(&db), "database"),
            (of(&role, ""), "user"),
            (of(&role, &db), "database user"),
        ] {
            for (name, value) in layer {
                out.insert(name, (value, source));
            }
        }
        out.into_iter().map(|(n, (v, s))| (n, v, s)).collect()
    }

    /// Every `ALTER ROLE ... SET` default: `(role, database, name, value)`.
    pub(crate) fn role_setting_docs(&self) -> Vec<(String, String, String, String)> {
        let Ok(docs) = self.storage.find_matching(
            DatabaseRegistry::NAMESPACE,
            Self::ROLE_SETTINGS_COLLECTION,
            &Document::new(),
        ) else {
            return Vec::new();
        };
        let mut out: Vec<_> = docs
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .filter_map(|d| {
                Some((
                    d.get_str("role").ok()?.to_string(),
                    d.get_str("database").ok()?.to_string(),
                    d.get_str("name").ok()?.to_string(),
                    d.get_str("value").ok()?.to_string(),
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Forget the `ALTER ROLE ... SET` defaults `filter` matches.
    pub(crate) fn drop_role_settings(&self, filter: Document) -> PgWireResult<()> {
        let err = |e| Self::storage_err("could not drop the role settings", e);
        let ns = DatabaseRegistry::NAMESPACE;
        if self
            .storage
            .collection_exists(ns, Self::ROLE_SETTINGS_COLLECTION)
            .map_err(err)?
        {
            self.storage
                .delete_matching(
                    ns,
                    Self::ROLE_SETTINGS_COLLECTION,
                    &filter,
                    0,
                    &Document::new(),
                    None,
                )
                .map_err(err)?;
        }
        Ok(())
    }

    /// `ALTER ROLE r [IN DATABASE d] SET / RESET`.
    pub(crate) fn alter_role_set(
        &self,
        role: Option<&str>,
        database: Option<&str>,
        name: Option<String>,
        value: Option<String>,
    ) -> PgWireResult<Vec<Response>> {
        let role = match role {
            None => String::new(),
            Some(r) => {
                let r = self.grantee_name(r)?;
                if self.role(&r)?.is_none() && r != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{r}\" does not exist"),
                    ));
                }
                r
            }
        };
        let database = match database {
            None => String::new(),
            Some(d) => {
                if d != self.db() && self.databases.lookup(&self.storage, d)?.is_none() {
                    return Err(Self::user_error(
                        "3D000",
                        format!("database \"{d}\" does not exist"),
                    ));
                }
                d.to_string()
            }
        };
        if let (Some(n), Some(_)) = (name.as_deref(), value.as_ref()) {
            self.check_setting_known(n)?;
        }
        let key = name.as_deref().map(canonical_setting);
        if let Some(msg) = key.as_deref().and_then(super::setting_change_refusal) {
            return Err(Self::user_error("55P02", msg));
        }
        let err = |e| Self::storage_err("could not record the role setting", e);
        let ns = DatabaseRegistry::NAMESPACE;
        if !self
            .storage
            .collection_exists(ns, Self::ROLE_SETTINGS_COLLECTION)
            .map_err(err)?
        {
            self.storage
                .create_collection(ns, Self::ROLE_SETTINGS_COLLECTION)
                .map_err(err)?;
        }
        let mut filter = bson::doc! {"role": &role, "database": &database};
        if let Some(k) = &key {
            filter.insert("name", k.as_str());
        }
        self.storage
            .delete_matching(
                ns,
                Self::ROLE_SETTINGS_COLLECTION,
                &filter,
                0,
                &Document::new(),
                None,
            )
            .map_err(err)?;
        if let (Some(key), Some(value)) = (key, value) {
            let doc = bson::doc! {
                "_id": format!("{role}\u{1f}{database}\u{1f}{key}"),
                "role": &role,
                "database": &database,
                "name": &key,
                "value": &value,
            };
            let bytes = encode_doc(&doc)
                .map_err(|e| Self::storage_err("could not encode the role setting", e))?;
            self.storage
                .insert(ns, Self::ROLE_SETTINGS_COLLECTION, vec![bytes], true)
                .map_err(err)?;
        }
        Ok(vec![Response::Execution(Tag::new("ALTER ROLE"))])
    }

    /// Database `db`'s `ALTER DATABASE ... SET` defaults, sorted by name.
    pub(crate) fn db_setting_defaults_of(&self, db: &str) -> Vec<(String, String)> {
        let Ok(docs) =
            self.storage
                .find_matching(db, Self::DB_SETTINGS_COLLECTION, &Document::new())
        else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = docs
            .iter()
            .filter_map(|b| decode_doc(b).ok())
            .filter_map(|d| {
                Some((
                    d.get_str("_id").ok()?.to_string(),
                    d.get_str("value").ok()?.to_string(),
                ))
            })
            .collect();
        out.sort();
        out
    }

    fn remove_db_setting(&self, name: Option<&str>) -> PgWireResult<()> {
        let filter = match name {
            Some(n) => bson::doc! {"_id": n},
            None => Document::new(),
        };
        let exists = self
            .storage
            .collection_exists(self.db(), Self::DB_SETTINGS_COLLECTION)
            .map_err(|e| Self::storage_err("could not change the database setting", e))?;
        if !exists {
            return Ok(());
        }
        self.storage
            .delete_matching(
                self.db(),
                Self::DB_SETTINGS_COLLECTION,
                &filter,
                0,
                &Document::new(),
                None,
            )
            .map_err(|e| Self::storage_err("could not change the database setting", e))?;
        Ok(())
    }

    pub(crate) fn alter_database_set(
        &self,
        database: &str,
        name: Option<String>,
        value: Option<String>,
    ) -> PgWireResult<Vec<Response>> {
        if database != self.db() && self.databases.lookup(&self.storage, database)?.is_none() {
            return Err(Self::user_error(
                "3D000",
                format!("database \"{database}\" does not exist"),
            ));
        }
        if database != self.db() {
            // Another database's store is not this session's to write.
            return Err(Self::user_error(
                "0A000",
                "ALTER DATABASE SET for a database other than the current one is not supported"
                    .into(),
            ));
        }
        // A value names a parameter PostgreSQL must know (42704 otherwise);
        // RESET of an unknown name is accepted (measured on 15.19).
        if let (Some(n), Some(_)) = (name.as_deref(), value.as_ref()) {
            self.check_setting_known(n)?;
        }
        let key = name.as_deref().map(canonical_setting);
        if let Some(msg) = key.as_deref().and_then(super::setting_change_refusal) {
            return Err(Self::user_error("55P02", msg));
        }
        self.remove_db_setting(key.as_deref())?;
        if let (Some(key), Some(value)) = (key, value) {
            let bytes = encode_doc(&bson::doc! {"_id": &key, "value": &value})
                .map_err(|e| Self::storage_err("could not encode the database setting", e))?;
            self.insert_checked(
                Self::DB_SETTINGS_COLLECTION,
                vec![bytes],
                "could not record the database setting",
            )?;
        }
        Ok(vec![Response::Execution(Tag::new("ALTER DATABASE"))])
    }

    /// The value `RESET name` returns to: the database's default when it
    /// has one, else the server's.
    pub(crate) fn reset_value(&self, key: &str) -> Option<String> {
        self.db_setting_defaults()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .or_else(|| self.server_defaults().get(key).cloned())
    }

    /// `pg_settings`: one row per setting this session knows.
    pub(crate) fn pg_settings_rows(&self, def: &TableDef) -> Vec<Document> {
        let field = |c: &str| def.field_of(c).expect("column");
        let defaults = self.server_defaults();
        let db_defaults = self.db_setting_layers();
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut names: Vec<&String> = defaults.keys().chain(settings.keys()).collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names.dedup();
        names
            .into_iter()
            .filter(|n| !n.contains('.') && n.as_str() != "role")
            .map(|name| {
                let setting = settings
                    .get(name)
                    .or_else(|| defaults.get(name))
                    .cloned()
                    .unwrap_or_default();
                let boot = defaults.get(name).cloned();
                let layer = db_defaults.iter().find(|(k, _, _)| k == name);
                let source = if let Some((_, _, source)) = layer.filter(|(_, v, _)| *v == setting) {
                    *source
                } else if layer.is_some() {
                    "session"
                } else if boot.as_deref() == Some(setting.as_str()) {
                    "default"
                } else {
                    "session"
                };
                // PostgreSQL 15's row, where it has one: a value still at
                // its default reads in the row's own unit (`work_mem` is
                // `4096` kB, where SHOW says `4MB`).
                let pg = pg15_settings::pg15_setting(name);
                let raw = |v: &str| -> String {
                    match pg {
                        Some(p) if p.show == v => p.setting.to_string(),
                        Some(p) => in_unit(v, p.unit, p.vartype).unwrap_or_else(|| v.to_string()),
                        None => v.to_string(),
                    }
                };
                let mut d = Document::new();
                d.insert(field("name"), name.as_str());
                d.insert(field("setting"), raw(&setting));
                d.insert(field("source"), source);
                let internal = is_internal_setting(name);
                match pg {
                    Some(p) => {
                        let opt = |v: &str| {
                            if v.is_empty() {
                                Bson::Null
                            } else {
                                Bson::String(v.to_string())
                            }
                        };
                        d.insert(field("unit"), opt(p.unit));
                        d.insert(field("category"), p.category);
                        d.insert(field("short_desc"), p.short_desc);
                        d.insert(field("context"), p.context);
                        d.insert(field("vartype"), p.vartype);
                        d.insert(field("min_val"), opt(p.min_val));
                        d.insert(field("max_val"), opt(p.max_val));
                        if p.vartype == "enum" {
                            let vals: Vec<Bson> = p
                                .enumvals
                                .split(',')
                                .map(|v| Bson::String(v.into()))
                                .collect();
                            d.insert(field("enumvals"), vals);
                        }
                    }
                    None => {
                        d.insert(field("context"), if internal { "internal" } else { "user" });
                        d.insert(
                            field("vartype"),
                            if setting == "on" || setting == "off" {
                                "bool"
                            } else if internal && setting.parse::<i64>().is_ok() {
                                "integer"
                            } else {
                                "string"
                            },
                        );
                    }
                }
                if let Some(b) = boot {
                    let b = match pg {
                        Some(p) if p.show == b => p.boot_val.to_string(),
                        _ => b,
                    };
                    d.insert(field("boot_val"), b);
                }
                if let Some(r) = self.reset_value(name) {
                    d.insert(field("reset_val"), raw(&r));
                }
                d.insert(field("pending_restart"), Bson::Boolean(false));
                d
            })
            .collect()
    }
}

/// A unit's size in its family's base (bytes, or microseconds), and the
/// family: `parse_int` / `parse_real`'s tables.
fn unit_scale(unit: &str) -> Option<(f64, bool)> {
    Some(match unit {
        "B" => (1.0, true),
        "kB" => (1024.0, true),
        "8kB" => (8192.0, true),
        "MB" => (1024.0 * 1024.0, true),
        "GB" => (1024.0 * 1024.0 * 1024.0, true),
        "TB" => (1024.0 * 1024.0 * 1024.0 * 1024.0, true),
        "us" => (1.0, false),
        "ms" => (1_000.0, false),
        "s" => (1_000_000.0, false),
        "min" => (60_000_000.0, false),
        "h" => (3_600_000_000.0, false),
        "d" => (86_400_000_000.0, false),
        _ => return None,
    })
}

/// A setting's value as `pg_settings.setting` shows it: a number in the
/// parameter's own unit (`work_mem = '8MB'` reads `8192`, its unit being
/// kB), rounded as `parse_int` rounds for an integer.
fn in_unit(value: &str, unit: &str, vartype: &str) -> Option<String> {
    if vartype != "integer" && vartype != "real" {
        return None;
    }
    let (base, memory) = unit_scale(unit)?;
    let v = value.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(v.len());
    // An exponent's `e` may also open no unit at all; a unit never starts
    // with `e`.
    let (number, suffix) = v.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let suffix = suffix.trim();
    let n = if suffix.is_empty() {
        number
    } else {
        let (scale, is_memory) = unit_scale(suffix)?;
        if is_memory != memory {
            return None;
        }
        number * scale / base
    };
    Some(if vartype == "integer" {
        format!("{}", n.round() as i64)
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    })
}

#[cfg(test)]
mod in_unit_tests {
    use super::in_unit;

    /// PostgreSQL 15.19's `pg_settings.setting` for each value.
    #[test]
    fn values_read_in_the_parameter_unit() {
        assert_eq!(in_unit("8MB", "kB", "integer").as_deref(), Some("8192"));
        assert_eq!(in_unit("70MB", "kB", "integer").as_deref(), Some("71680"));
        assert_eq!(in_unit("9MB", "8kB", "integer").as_deref(), Some("1152"));
        assert_eq!(in_unit("5s", "ms", "integer").as_deref(), Some("5000"));
        assert_eq!(in_unit("1min", "s", "integer").as_deref(), Some("60"));
        assert_eq!(in_unit("4096", "kB", "integer").as_deref(), Some("4096"));
        assert_eq!(in_unit("2ms", "ms", "real").as_deref(), Some("2"));
        assert_eq!(in_unit("on", "", "bool"), None);
    }
}
