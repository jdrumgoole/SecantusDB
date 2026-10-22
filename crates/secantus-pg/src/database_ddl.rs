//! `CREATE DATABASE`'s options and `ALTER DATABASE` (RP-064), checked as
//! PostgreSQL 15's `createdb` / `AlterDatabase` check them, in their order.
//!
//! A database is a storage namespace here, so `TEMPLATE t` copies `t`'s
//! namespace -- every collection with its options, rows and indexes -- as
//! `createdb` copies the template's files. The encoding, collation, ctype,
//! connection limit, owner and the template / allow-connections flags are
//! the database's record, which `pg_database` shows and the login enforces.

use super::*;

fn bool_option(name: &str, v: &str) -> PgWireResult<bool> {
    match v.to_ascii_lowercase().as_str() {
        "" | "true" | "on" | "1" | "yes" => Ok(true),
        "false" | "off" | "0" | "no" => Ok(false),
        _ => Err(PgHandler::user_error(
            "22023",
            format!("{name} requires a Boolean value"),
        )),
    }
}

/// The codeset a libc locale name requires, or `None` for one that takes any
/// encoding (`C`, `POSIX`).
fn locale_encoding(locale: &str) -> Option<&'static str> {
    if matches!(locale, "C" | "POSIX") {
        return None;
    }
    let codeset = locale.split_once('.').map(|(_, c)| c.to_ascii_lowercase());
    match codeset.as_deref() {
        Some("utf-8" | "utf8") => Some("UTF8"),
        Some("iso88591" | "iso8859-1") => Some("LATIN1"),
        Some("iso885915" | "iso8859-15") => Some("LATIN9"),
        _ => Some("UTF8"),
    }
}

/// The locales this server has: libc's `C` and `POSIX`, and `C.UTF-8`
/// (the libc-collations scope decision in the backlog).
fn locale_exists(locale: &str) -> bool {
    matches!(locale, "C" | "POSIX" | "C.UTF-8" | "C.utf8")
}

impl PgHandler {
    /// The other sessions connected to `db`.
    pub(crate) fn sessions_on(&self, db: &str) -> usize {
        let me = self.backend_pid.load(std::sync::atomic::Ordering::Relaxed);
        backend_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(pid, b)| {
                **pid != me && b.activity.lock().unwrap_or_else(|e| e.into_inner()).datname == db
            })
            .count()
    }

    pub(crate) fn create_database(
        &self,
        name: &str,
        options: &[(String, String)],
    ) -> PgWireResult<()> {
        let get = |k: &str| options.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        for (k, _) in options {
            if !matches!(
                k.as_str(),
                "owner"
                    | "template"
                    | "encoding"
                    | "locale"
                    | "lc_collate"
                    | "lc_ctype"
                    | "icu_locale"
                    | "locale_provider"
                    | "collation_version"
                    | "tablespace"
                    | "allow_connections"
                    | "connection_limit"
                    | "is_template"
                    | "oid"
                    | "strategy"
            ) {
                return Err(Self::user_error(
                    "42601",
                    format!("option \"{k}\" not recognized"),
                ));
            }
        }
        if get("locale").is_some() && (get("lc_collate").is_some() || get("lc_ctype").is_some()) {
            return Err(Self::user_error(
                "42601",
                "conflicting or redundant options".into(),
            ));
        }
        if let Some(p) = get("locale_provider").filter(|p| p.to_ascii_lowercase() != "libc") {
            return Err(Self::user_error(
                "0A000",
                format!("locale provider \"{p}\" is not supported"),
            ));
        }
        if let Some(t) = get("tablespace").filter(|t| t != "pg_default") {
            return Err(Self::user_error(
                "42704",
                format!("tablespace \"{t}\" does not exist"),
            ));
        }
        if let Some(st) = get("strategy") {
            if !matches!(st.to_ascii_lowercase().as_str(), "wal_log" | "file_copy") {
                return Err(Self::user_error(
                    "22023",
                    format!("invalid create database strategy \"{st}\""),
                ));
            }
        }
        let me = self.current_role_name();
        let owner = get("owner").unwrap_or_else(|| me.clone());
        if self.role(&owner)?.is_none() && owner != self.session_user_name() {
            return Err(Self::user_error(
                "42704",
                format!("role \"{owner}\" does not exist"),
            ));
        }
        let my_role = self.role(&me)?;
        let can_create =
            self.is_superuser(&me) || my_role.as_ref().is_some_and(|r| r.createdb || r.superuser);
        if !can_create {
            return Err(Self::user_error(
                "42501",
                "permission denied to create database".into(),
            ));
        }
        if owner != me && !self.role_is_member(&me, &owner) {
            return Err(Self::user_error(
                "42501",
                format!("must be member of role \"{owner}\""),
            ));
        }
        let encoding = match get("encoding") {
            None => None,
            Some(e) => Some(
                secantus_pgplan::scalar::call("pg_char_to_encoding", &[Bson::String(e.clone())])
                    .and_then(Result::ok)
                    .and_then(|v| match v {
                        Bson::Int32(n) if n >= 0 => {
                            secantus_pgplan::scalar::call("pg_encoding_to_char", &[Bson::Int32(n)])
                                .and_then(Result::ok)
                                .and_then(|c| c.as_str().map(str::to_string))
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        Self::user_error("42704", format!("{e} is not a valid encoding name"))
                    })?,
            ),
        };
        let template = get("template").unwrap_or_else(|| "template1".into());
        let Some(source) = self.databases.lookup(&self.storage, &template)? else {
            return Err(Self::user_error(
                "3D000",
                format!("template database \"{template}\" does not exist"),
            ));
        };
        if !source.is_template && !self.is_superuser(&me) && source.owner != me {
            return Err(Self::user_error(
                "42501",
                format!("permission denied to copy database \"{template}\""),
            ));
        }
        let connlimit: i32 = match get("connection_limit") {
            None => -1,
            Some(v) => v
                .trim()
                .parse()
                .map_err(|_| Self::user_error("22023", format!("invalid connection limit: {v}")))?,
        };
        if connlimit < -1 {
            return Err(Self::user_error(
                "22023",
                format!("invalid connection limit: {connlimit}"),
            ));
        }
        let encoding = encoding.unwrap_or_else(|| source.encoding.clone());
        let collate = get("lc_collate")
            .or_else(|| get("locale"))
            .unwrap_or_else(|| source.collate.clone());
        let ctype = get("lc_ctype")
            .or_else(|| get("locale"))
            .unwrap_or_else(|| source.ctype.clone());
        for (what, l) in [("LC_COLLATE", &collate), ("LC_CTYPE", &ctype)] {
            if !locale_exists(l) {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "42809".into(),
                    format!("invalid {what} locale name: \"{l}\""),
                );
                info.hint = Some("If the locale name is specific to ICU, use ICU_LOCALE.".into());
                return Err(PgWireError::UserError(Box::new(info)));
            }
        }
        for (what, l) in [("LC_CTYPE", &ctype), ("LC_COLLATE", &collate)] {
            if let Some(needs) = locale_encoding(l).filter(|n| *n != encoding) {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "22023".into(),
                    format!("encoding \"{encoding}\" does not match locale \"{l}\""),
                );
                info.detail = Some(format!(
                    "The chosen {what} setting requires encoding \"{needs}\"."
                ));
                return Err(PgWireError::UserError(Box::new(info)));
            }
        }
        if template != "template0" {
            if encoding != source.encoding {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "22023".into(),
                    format!(
                        "new encoding ({encoding}) is incompatible with the encoding of the template database ({})",
                        source.encoding
                    ),
                );
                info.hint = Some(
                    "Use the same encoding as in the template database, or use template0 as template."
                        .into(),
                );
                return Err(PgWireError::UserError(Box::new(info)));
            }
            for (what, new, old) in [
                ("collation", &collate, &source.collate),
                ("LC_CTYPE", &ctype, &source.ctype),
            ] {
                if new != old {
                    let mut info = ErrorInfo::new(
                        "ERROR".into(),
                        "22023".into(),
                        format!(
                            "new {what} ({new}) is incompatible with the {what} of the template database ({old})"
                        ),
                    );
                    info.hint = Some(format!(
                        "Use the same {what} as in the template database, or use template0 as template."
                    ));
                    return Err(PgWireError::UserError(Box::new(info)));
                }
            }
        }
        if self.databases.lookup(&self.storage, name)?.is_some() {
            return Err(Self::user_error(
                "42P04",
                format!("database \"{name}\" already exists"),
            ));
        }
        let others = self.sessions_on(&template);
        if others > 0 {
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "55006".into(),
                format!("source database \"{template}\" is being accessed by other users"),
            );
            info.detail = Some(if others == 1 {
                "There is 1 other session using the database.".into()
            } else {
                format!("There are {others} other sessions using the database.")
            });
            return Err(PgWireError::UserError(Box::new(info)));
        }
        let is_template = get("is_template")
            .map(|v| bool_option("is_template", &v))
            .transpose()?
            .unwrap_or(false);
        let allow = get("allow_connections")
            .map(|v| bool_option("allow_connections", &v))
            .transpose()?
            .unwrap_or(true);
        self.copy_database(&template, name)?;
        self.databases.create(
            &self.storage,
            name,
            bson::doc! {
                "owner": owner,
                "encoding": encoding,
                "collate": collate,
                "ctype": ctype,
                "connection_limit": connlimit,
                "is_template": is_template,
                "allow_connections": allow,
            },
        )
    }

    /// Copy every collection of `from` into `to`: options, rows, indexes.
    fn copy_database(&self, from: &str, to: &str) -> PgWireResult<()> {
        let err = |e| Self::storage_err("could not copy the template database", e);
        for coll in self.storage.list_collections(from).map_err(err)? {
            let options = self
                .storage
                .get_collection_options(from, &coll)
                .map_err(err)?;
            self.storage
                .create_collection_with_options(to, &coll, &options)
                .map_err(err)?;
            let rows = self
                .storage
                .find_matching(from, &coll, &Document::new())
                .map_err(err)?;
            if !rows.is_empty() {
                self.storage.insert(to, &coll, rows, true).map_err(err)?;
            }
            for ix in self.storage.list_indexes(from, &coll).unwrap_or_default() {
                let (Ok(ix_name), Ok(key)) = (ix.get_str("name"), ix.get_document("key")) else {
                    continue;
                };
                if ix_name == "_id_" {
                    continue;
                }
                let mut options = Document::new();
                for (k, v) in &ix {
                    if !matches!(k.as_str(), "name" | "key" | "v" | "ns") {
                        options.insert(k.clone(), v.clone());
                    }
                }
                self.storage
                    .create_index(to, &coll, ix_name, key, &options)
                    .map_err(err)?;
            }
        }
        Ok(())
    }

    /// `ALTER DATABASE d RENAME TO n` / `OWNER TO r` / `WITH CONNECTION LIMIT
    /// n | IS_TEMPLATE b | ALLOW_CONNECTIONS b`.
    pub(crate) fn alter_database(
        &self,
        name: &str,
        options: &[(String, String)],
        rename: Option<&str>,
        owner: Option<&str>,
    ) -> PgWireResult<()> {
        let Some(info) = self.databases.lookup(&self.storage, name)? else {
            return Err(Self::user_error(
                "3D000",
                format!("database \"{name}\" does not exist"),
            ));
        };
        let me = self.current_role_name();
        let is_owner = self.is_superuser(&me) || info.owner == me;
        if !is_owner {
            return Err(Self::user_error(
                "42501",
                format!("must be owner of database {name}"),
            ));
        }
        let mut fields = Document::new();
        if let Some(new) = rename {
            if name == self.db() {
                return Err(Self::user_error(
                    "0A000",
                    "current database cannot be renamed".into(),
                ));
            }
            if self.databases.lookup(&self.storage, new)?.is_some() {
                return Err(Self::user_error(
                    "42P04",
                    format!("database \"{new}\" already exists"),
                ));
            }
            let others = self.sessions_on(name);
            if others > 0 {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "55006".into(),
                    format!("database \"{name}\" is being accessed by other users"),
                );
                info.detail = Some(if others == 1 {
                    "There is 1 other session using the database.".into()
                } else {
                    format!("There are {others} other sessions using the database.")
                });
                return Err(PgWireError::UserError(Box::new(info)));
            }
            self.copy_database(name, new)?;
            self.storage
                .drop_database(name)
                .map_err(|e| Self::storage_err("could not rename the database", e))?;
        }
        if let Some(r) = owner {
            if self.role(r)?.is_none() && *r != self.session_user_name() {
                return Err(Self::user_error(
                    "42704",
                    format!("role \"{r}\" does not exist"),
                ));
            }
            fields.insert("owner", r);
        }
        for (k, v) in options {
            match k.as_str() {
                "connection_limit" => {
                    let n: i32 = v.trim().parse().map_err(|_| {
                        Self::user_error("22023", format!("invalid connection limit: {v}"))
                    })?;
                    if n < -1 {
                        return Err(Self::user_error(
                            "22023",
                            format!("invalid connection limit: {n}"),
                        ));
                    }
                    fields.insert("connection_limit", n);
                }
                "is_template" => {
                    fields.insert("is_template", bool_option(k, v)?);
                }
                "allow_connections" => {
                    let allow = bool_option(k, v)?;
                    if !allow && name == self.db() {
                        return Err(Self::user_error(
                            "22023",
                            "cannot disallow connections for current database".into(),
                        ));
                    }
                    fields.insert("allow_connections", allow);
                }
                "tablespace" => {
                    return Err(Self::user_error(
                        "0A000",
                        "ALTER DATABASE SET TABLESPACE is not supported".into(),
                    ))
                }
                other => {
                    return Err(Self::user_error(
                        "42601",
                        format!("option \"{other}\" not recognized"),
                    ))
                }
            }
        }
        self.databases.update(&self.storage, name, fields, rename)
    }
}
