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

    /// The stored defaults of the session's database, by GUC name.
    pub(crate) fn db_setting_defaults(&self) -> Vec<(String, String)> {
        let Ok(docs) =
            self.storage
                .find_matching(self.db(), Self::DB_SETTINGS_COLLECTION, &Document::new())
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
        let db_defaults = self.db_setting_defaults();
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
                let source = if db_defaults.iter().any(|(k, _)| k == name) {
                    "database"
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
                        _ => v.to_string(),
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
