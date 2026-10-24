//! `CREATE / ALTER / DROP SUBSCRIPTION` (RP-169), as PostgreSQL 15.19's
//! `subscriptioncmds.c` runs them: `parse_subscription_options` ported
//! option by option, then each statement's checks in its order. A
//! subscription is its `pg_subscription` row, the replication origin
//! (`pg_<oid>`) CREATE makes for it, and its row in the statistics views.
//!
//! There is no WAL receiver here, so nothing reaches a publisher. Where
//! PostgreSQL would connect -- CREATE with `connect = true` (its default),
//! `REFRESH PUBLICATION`, a refreshing `SET / ADD / DROP PUBLICATION`, and
//! DROP of a subscription that names a slot -- the statement is refused by
//! name, after every check PostgreSQL makes before it connects. `ENABLE`
//! is refused the same way: it would start an apply worker.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::subscriptions::{AlterKind, OptArg, RawOpt, SubscriptionOp};

use crate::{bson_i64, PgHandler};

pub(crate) const SUBSCRIPTION_COLLECTION: &str = "__sql_subscriptions__";
/// The open block's own writes to the shared subscriptions, in the
/// uncommitted-catalog overlay (`note_uncommitted_type`), keyed apart from
/// a database's own catalog of the same name.
const SHARED_SUBSCRIPTIONS: &str = "__shared_subscriptions__";
pub(crate) const PG_SUBSCRIPTION: i64 = 6100;

const SUBOPT_CONNECT: u32 = 0x0001;
const SUBOPT_ENABLED: u32 = 0x0002;
const SUBOPT_CREATE_SLOT: u32 = 0x0004;
const SUBOPT_SLOT_NAME: u32 = 0x0008;
const SUBOPT_COPY_DATA: u32 = 0x0010;
const SUBOPT_SYNCHRONOUS_COMMIT: u32 = 0x0020;
const SUBOPT_REFRESH: u32 = 0x0040;
const SUBOPT_BINARY: u32 = 0x0080;
const SUBOPT_STREAMING: u32 = 0x0100;
const SUBOPT_TWOPHASE_COMMIT: u32 = 0x0200;
const SUBOPT_DISABLE_ON_ERR: u32 = 0x0400;
const SUBOPT_LSN: u32 = 0x0800;

/// `SubOpts`.
#[derive(Debug, Clone, Default)]
struct SubOpts {
    specified: u32,
    slot_name: Option<String>,
    synchronous_commit: Option<String>,
    connect: bool,
    enabled: bool,
    create_slot: bool,
    copy_data: bool,
    refresh: bool,
    binary: bool,
    streaming: bool,
    twophase: bool,
    disableonerr: bool,
    lsn: u64,
}

impl SubOpts {
    fn given(&self, bit: u32) -> bool {
        self.specified & bit == bit
    }
}

fn error(code: &str, message: String, hint: Option<&str>) -> PgWireError {
    let mut info = ErrorInfo::new("ERROR".into(), code.into(), message);
    info.hint = hint.map(str::to_string);
    PgWireError::UserError(Box::new(info))
}

fn syntax(message: String) -> PgWireError {
    error("42601", message, None)
}

/// `defGetString`.
fn string_value(o: &RawOpt) -> PgWireResult<String> {
    match &o.arg {
        OptArg::None => Err(syntax(format!("{} requires a parameter", o.name))),
        OptArg::Integer(i) => Ok(i.to_string()),
        OptArg::Text(t) => Ok(t.clone()),
    }
}

/// `defGetBoolean`.
fn bool_value(o: &RawOpt) -> PgWireResult<bool> {
    let refused = || syntax(format!("{} requires a Boolean value", o.name));
    match &o.arg {
        OptArg::None => Ok(true),
        OptArg::Integer(0) => Ok(false),
        OptArg::Integer(1) => Ok(true),
        OptArg::Integer(_) => Err(refused()),
        OptArg::Text(t) => match t.to_ascii_lowercase().as_str() {
            "true" | "on" => Ok(true),
            "false" | "off" => Ok(false),
            _ => Err(refused()),
        },
    }
}

/// `ReplicationSlotValidateName`.
fn validate_slot_name(name: &str) -> PgWireResult<()> {
    if name.is_empty() {
        return Err(error(
            "42602",
            format!("replication slot name \"{name}\" is too short"),
            None,
        ));
    }
    if name.len() >= 64 {
        return Err(error(
            "42622",
            format!("replication slot name \"{name}\" is too long"),
            None,
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(error(
            "42602",
            format!("replication slot name \"{name}\" contains invalid character"),
            Some(
                "Replication slot names may only contain lower case letters, numbers, and the \
                 underscore character.",
            ),
        ));
    }
    Ok(())
}

/// `synchronous_commit`'s values, as `set_config_option` checks them.
fn check_synchronous_commit(v: &str) -> PgWireResult<()> {
    let known = [
        "local",
        "remote_write",
        "remote_apply",
        "on",
        "off",
        "true",
        "false",
        "yes",
        "no",
        "1",
        "0",
    ];
    if known.contains(&v.to_ascii_lowercase().as_str()) {
        return Ok(());
    }
    Err(error(
        "22023",
        format!("invalid value for parameter \"synchronous_commit\": \"{v}\""),
        Some("Available values: local, remote_write, remote_apply, on, off."),
    ))
}

/// `pg_lsn_in`.
fn parse_lsn(s: &str) -> PgWireResult<u64> {
    let bad = || {
        error(
            "22P02",
            format!("invalid input syntax for type pg_lsn: \"{s}\""),
            None,
        )
    };
    let (hi, lo) = s.split_once('/').ok_or_else(bad)?;
    let hex = |p: &str| {
        (!p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(p, 16).ok())
            .flatten()
    };
    Ok((hex(hi).ok_or_else(bad)? << 32) | hex(lo).ok_or_else(bad)?)
}

fn exclusive(a: &str, b: &str) -> PgWireError {
    syntax(format!("{a} and {b} are mutually exclusive options"))
}

fn must_also(a: &str, b: &str) -> PgWireError {
    syntax(format!("subscription with {a} must also set {b}"))
}

/// `parse_subscription_options`.
fn parse_options(opts: &[RawOpt], supported: u32) -> PgWireResult<SubOpts> {
    let is = |bit: u32| supported & bit == bit;
    let mut o = SubOpts {
        connect: is(SUBOPT_CONNECT),
        enabled: is(SUBOPT_ENABLED),
        create_slot: is(SUBOPT_CREATE_SLOT),
        copy_data: is(SUBOPT_COPY_DATA),
        refresh: is(SUBOPT_REFRESH),
        ..SubOpts::default()
    };
    for d in opts {
        let name = d.name.as_str();
        let flag = |bit: u32, o: &mut SubOpts| -> PgWireResult<()> {
            if o.given(bit) {
                return Err(syntax("conflicting or redundant options".into()));
            }
            o.specified |= bit;
            Ok(())
        };
        match name {
            "connect" if is(SUBOPT_CONNECT) => {
                flag(SUBOPT_CONNECT, &mut o)?;
                o.connect = bool_value(d)?;
            }
            "enabled" if is(SUBOPT_ENABLED) => {
                flag(SUBOPT_ENABLED, &mut o)?;
                o.enabled = bool_value(d)?;
            }
            "create_slot" if is(SUBOPT_CREATE_SLOT) => {
                flag(SUBOPT_CREATE_SLOT, &mut o)?;
                o.create_slot = bool_value(d)?;
            }
            "slot_name" if is(SUBOPT_SLOT_NAME) => {
                flag(SUBOPT_SLOT_NAME, &mut o)?;
                let v = string_value(d)?;
                if v == "none" {
                    o.slot_name = None;
                } else {
                    validate_slot_name(&v)?;
                    o.slot_name = Some(v);
                }
            }
            "copy_data" if is(SUBOPT_COPY_DATA) => {
                flag(SUBOPT_COPY_DATA, &mut o)?;
                o.copy_data = bool_value(d)?;
            }
            "synchronous_commit" if is(SUBOPT_SYNCHRONOUS_COMMIT) => {
                flag(SUBOPT_SYNCHRONOUS_COMMIT, &mut o)?;
                let v = string_value(d)?;
                check_synchronous_commit(&v)?;
                o.synchronous_commit = Some(v);
            }
            "refresh" if is(SUBOPT_REFRESH) => {
                flag(SUBOPT_REFRESH, &mut o)?;
                o.refresh = bool_value(d)?;
            }
            "binary" if is(SUBOPT_BINARY) => {
                flag(SUBOPT_BINARY, &mut o)?;
                o.binary = bool_value(d)?;
            }
            "streaming" if is(SUBOPT_STREAMING) => {
                flag(SUBOPT_STREAMING, &mut o)?;
                o.streaming = bool_value(d)?;
            }
            // Toggling two_phase is refused where it is not an option:
            // ALTER answers it as unrecognised.
            "two_phase" => {
                if !is(SUBOPT_TWOPHASE_COMMIT) {
                    return Err(syntax(format!(
                        "unrecognized subscription parameter: \"{name}\""
                    )));
                }
                flag(SUBOPT_TWOPHASE_COMMIT, &mut o)?;
                o.twophase = bool_value(d)?;
            }
            "disable_on_error" if is(SUBOPT_DISABLE_ON_ERR) => {
                flag(SUBOPT_DISABLE_ON_ERR, &mut o)?;
                o.disableonerr = bool_value(d)?;
            }
            "lsn" if is(SUBOPT_LSN) => {
                let v = string_value(d)?;
                if o.given(SUBOPT_LSN) {
                    return Err(syntax("conflicting or redundant options".into()));
                }
                o.lsn = if v == "none" {
                    0
                } else {
                    let lsn = parse_lsn(&v)?;
                    if lsn == 0 {
                        return Err(error(
                            "22023",
                            format!("invalid WAL location (LSN): {v}"),
                            None,
                        ));
                    }
                    lsn
                };
                o.specified |= SUBOPT_LSN;
            }
            _ => {
                return Err(syntax(format!(
                    "unrecognized subscription parameter: \"{name}\""
                )))
            }
        }
    }
    if !o.connect && is(SUBOPT_CONNECT) {
        if o.enabled && o.given(SUBOPT_ENABLED) {
            return Err(exclusive("connect = false", "enabled = true"));
        }
        if o.create_slot && o.given(SUBOPT_CREATE_SLOT) {
            return Err(exclusive("connect = false", "create_slot = true"));
        }
        if o.copy_data && o.given(SUBOPT_COPY_DATA) {
            return Err(exclusive("connect = false", "copy_data = true"));
        }
        o.enabled = false;
        o.create_slot = false;
        o.copy_data = false;
    }
    if o.slot_name.is_none() && o.given(SUBOPT_SLOT_NAME) {
        if o.enabled {
            return Err(if o.given(SUBOPT_ENABLED) {
                exclusive("slot_name = NONE", "enabled = true")
            } else {
                must_also("slot_name = NONE", "enabled = false")
            });
        }
        if o.create_slot {
            return Err(if o.given(SUBOPT_CREATE_SLOT) {
                exclusive("slot_name = NONE", "create_slot = true")
            } else {
                must_also("slot_name = NONE", "create_slot = false")
            });
        }
    }
    Ok(o)
}

/// libpq's connection options (PostgreSQL 15's `PQconninfoOptions`).
const CONNINFO_KEYWORDS: &[&str] = &[
    "service",
    "user",
    "password",
    "passfile",
    "channel_binding",
    "connect_timeout",
    "dbname",
    "host",
    "hostaddr",
    "port",
    "client_encoding",
    "options",
    "application_name",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "tcp_user_timeout",
    "sslmode",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslpassword",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "gssencmode",
    "krbsrvname",
    "gsslib",
    "replication",
    "target_session_attrs",
];

/// `walrcv_check_conninfo`: libpq's `conninfo_parse` of a `key=value`
/// string (a `postgresql://` URI is taken as written).
fn check_conninfo(conninfo: &str) -> PgWireResult<()> {
    let refuse = |m: String| syntax(format!("invalid connection string syntax: {m}\n"));
    if conninfo.starts_with("postgresql://") || conninfo.starts_with("postgres://") {
        return Ok(());
    }
    let chars: Vec<char> = conninfo.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let keyword: String = chars[start..i].iter().collect();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            return Err(refuse(format!(
                "missing \"=\" after \"{keyword}\" in connection info string"
            )));
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == '\'' {
            i += 1;
            loop {
                match chars.get(i) {
                    None => {
                        return Err(refuse(
                            "unterminated quoted string in connection info string".into(),
                        ))
                    }
                    Some('\\') => i += 2,
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
        }
        if !CONNINFO_KEYWORDS.contains(&keyword.as_str()) {
            return Err(refuse(format!("invalid connection option \"{keyword}\"")));
        }
    }
    Ok(())
}

/// `check_duplicates_in_publist`.
fn check_duplicates(publications: &[String]) -> PgWireResult<()> {
    for (i, p) in publications.iter().enumerate() {
        if publications[..i].contains(p) {
            return Err(error(
                "42710",
                format!("publication name \"{p}\" used more than once"),
                None,
            ));
        }
    }
    Ok(())
}

/// What PostgreSQL would connect to a publisher for: refused by name.
fn needs_publisher(what: &str) -> PgWireError {
    error(
        "0A000",
        format!("{what} is not supported: this server does not connect to a publisher"),
        None,
    )
}

impl PgHandler {
    /// Every database's subscriptions: `pg_subscription` is a shared
    /// catalog, each row naming its database (`subdbid`). Each is kept
    /// once, beside the databases, keyed by its database's oid and its
    /// name; one an earlier build kept in its database is read as that
    /// database's (RP-169). Each document's `_id` is the name and `db` its
    /// database's oid.
    fn all_subscription_docs(&self) -> Vec<Document> {
        let ns = crate::DatabaseRegistry::NAMESPACE;
        let mut shared: std::collections::BTreeMap<String, Document> =
            std::collections::BTreeMap::new();
        if let Ok(true) = self.storage.collection_exists(ns, SUBSCRIPTION_COLLECTION) {
            for b in self
                .storage
                .find_matching(ns, SUBSCRIPTION_COLLECTION, &Document::new())
                .unwrap_or_default()
            {
                if let Ok(d) = crate::decode_doc(&b) {
                    shared.insert(d.get_str("_id").unwrap_or_default().to_string(), d);
                }
            }
        }
        // What the open block wrote, which a read outside its transaction
        // does not see.
        for ((coll, id), doc) in self.pending.types().iter() {
            if *coll == SHARED_SUBSCRIPTIONS {
                match doc {
                    Some(d) => shared.insert(id.clone(), d.clone()),
                    None => shared.remove(id),
                };
            }
        }
        let mut docs: Vec<Document> = shared
            .into_values()
            .map(|mut d| {
                let name = d.get_str("name").unwrap_or_default().to_string();
                d.insert("_id", name);
                d.remove("name");
                d
            })
            .collect();
        let here = self.current_database_oid().unwrap_or(0);
        for mut d in self.legacy_subscription_docs() {
            let name = d.get_str("_id").unwrap_or_default().to_string();
            let shadowed = docs.iter().any(|s| {
                s.get("db").and_then(bson_i64) == Some(here) && s.get_str("_id") == Ok(&name)
            });
            if !shadowed {
                d.insert("db", here);
                docs.push(d);
            }
        }
        docs.sort_by_key(|d| d.get("oid").and_then(bson_i64).unwrap_or(0));
        docs
    }

    /// The subscriptions an earlier build kept in this database's catalog.
    fn legacy_subscription_docs(&self) -> Vec<Document> {
        if !matches!(
            self.storage
                .collection_exists(self.db(), SUBSCRIPTION_COLLECTION),
            Ok(true)
        ) {
            return Vec::new();
        }
        self.type_catalog_docs(SUBSCRIPTION_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// This database's subscriptions, which a name resolves among
    /// (`get_subscription_oid` looks in `MyDatabaseId`'s).
    fn subscription_docs(&self) -> Vec<Document> {
        let here = self.current_database_oid().unwrap_or(0);
        self.all_subscription_docs()
            .into_iter()
            .filter(|d| d.get("db").and_then(bson_i64) == Some(here))
            .collect()
    }

    /// How many subscriptions database `oid` has (`CountDBSubscriptions`).
    pub(crate) fn database_subscription_count(&self, oid: i64) -> usize {
        self.all_subscription_docs()
            .iter()
            .filter(|d| d.get("db").and_then(bson_i64) == Some(oid))
            .count()
    }

    fn shared_subscription_key(db: i64, name: &str) -> String {
        format!("{db}:{name}")
    }

    /// Write this database's subscription `name` as `doc` (whose `_id` is
    /// its name).
    fn put_subscription(&self, name: &str, mut doc: Document) -> PgWireResult<()> {
        self.delete_subscription(name)?;
        let ns = crate::DatabaseRegistry::NAMESPACE;
        let err = |e| Self::storage_err("could not record the subscription", e);
        if !self
            .storage
            .collection_exists(ns, SUBSCRIPTION_COLLECTION)
            .map_err(err)?
        {
            self.storage
                .create_collection(ns, SUBSCRIPTION_COLLECTION)
                .map_err(err)?;
        }
        let db = self.current_database_oid()?;
        let key = Self::shared_subscription_key(db, name);
        doc.insert("_id", &key);
        doc.insert("name", name);
        doc.insert("db", db);
        let bytes = crate::encode_doc(&doc)
            .map_err(|e| Self::storage_err("could not encode the subscription", e))?;
        self.storage
            .insert(ns, SUBSCRIPTION_COLLECTION, vec![bytes], true)
            .map_err(err)?;
        self.note_uncommitted_type(SHARED_SUBSCRIPTIONS, &key, Some(doc));
        Ok(())
    }

    /// Remove this database's subscription `name`, wherever it is kept.
    fn delete_subscription(&self, name: &str) -> PgWireResult<()> {
        let ns = crate::DatabaseRegistry::NAMESPACE;
        let err = |e| Self::storage_err("could not drop the subscription", e);
        if self
            .storage
            .collection_exists(ns, SUBSCRIPTION_COLLECTION)
            .map_err(err)?
        {
            let key = Self::shared_subscription_key(self.current_database_oid()?, name);
            self.storage
                .delete_matching(
                    ns,
                    SUBSCRIPTION_COLLECTION,
                    &bson::doc! {"_id": &key},
                    0,
                    &Document::new(),
                    None,
                )
                .map_err(err)?;
            self.note_uncommitted_type(SHARED_SUBSCRIPTIONS, &key, None);
        }
        if self
            .legacy_subscription_docs()
            .iter()
            .any(|d| d.get_str("_id") == Ok(name))
        {
            self.delete_type_doc(SUBSCRIPTION_COLLECTION, name)?;
        }
        Ok(())
    }

    fn subscription_doc(&self, name: &str) -> Option<Document> {
        self.subscription_docs()
            .into_iter()
            .find(|d| d.get_str("_id") == Ok(name))
    }

    fn found_subscription(&self, name: &str) -> PgWireResult<Document> {
        self.subscription_doc(name).ok_or_else(|| {
            error(
                "42704",
                format!("subscription \"{name}\" does not exist"),
                None,
            )
        })
    }

    /// `pg_subscription_ownercheck`.
    fn subscription_owner_check(&self, d: &Document, name: &str) -> PgWireResult<()> {
        let role = self.current_role_name();
        if d.get_str("owner") == Ok(role.as_str()) || self.is_superuser(&role) {
            return Ok(());
        }
        Err(error(
            "42501",
            format!("must be owner of subscription {name}"),
            None,
        ))
    }

    pub(crate) fn subscription_statement(&self, op: SubscriptionOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        match op {
            SubscriptionOp::Create {
                name,
                conninfo,
                publications,
                options,
            } => {
                let supported = SUBOPT_CONNECT
                    | SUBOPT_ENABLED
                    | SUBOPT_CREATE_SLOT
                    | SUBOPT_SLOT_NAME
                    | SUBOPT_COPY_DATA
                    | SUBOPT_SYNCHRONOUS_COMMIT
                    | SUBOPT_BINARY
                    | SUBOPT_STREAMING
                    | SUBOPT_TWOPHASE_COMMIT
                    | SUBOPT_DISABLE_ON_ERR;
                let opts = parse_options(&options, supported)?;
                if opts.create_slot {
                    self.refuse_in_transaction_block(
                        "CREATE SUBSCRIPTION ... WITH (create_slot = true)",
                    )?;
                }
                if !self.is_superuser(&self.current_role_name()) {
                    return Err(error(
                        "42501",
                        "must be superuser to create subscriptions".into(),
                        None,
                    ));
                }
                if self.subscription_doc(&name).is_some() {
                    return Err(error(
                        "42710",
                        format!("subscription \"{name}\" already exists"),
                        None,
                    ));
                }
                let slot = if opts.given(SUBOPT_SLOT_NAME) {
                    opts.slot_name.clone()
                } else {
                    Some(name.clone())
                };
                check_conninfo(&conninfo)?;
                check_duplicates(&publications)?;
                if opts.connect {
                    return Err(needs_publisher("CREATE SUBSCRIPTION with connect = true"));
                }
                // `replorigin_create`: the lowest free origin id.
                let used: Vec<i64> = self
                    .all_subscription_docs()
                    .iter()
                    .filter_map(|d| d.get("roident").and_then(bson_i64))
                    .collect();
                let roident = (1..).find(|i| !used.contains(i)).unwrap_or(1);
                let mut doc = bson::doc! {
                    "_id": &name,
                    "oid": self.mint_global_oids(1)?,
                    "owner": self.current_role_name(),
                    "enabled": opts.enabled,
                    "binary": opts.binary,
                    "streaming": opts.streaming,
                    "twophase": if opts.twophase { "p" } else { "d" },
                    "disableonerr": opts.disableonerr,
                    "conninfo": &conninfo,
                    "synccommit": opts.synchronous_commit.clone().unwrap_or_else(|| "off".into()),
                    "publications": publications.clone(),
                    "skiplsn": 0_i64,
                    "roident": roident,
                };
                if let Some(s) = &slot {
                    doc.insert("slot", s);
                }
                self.put_subscription(&name, doc)?;
                self.warning(
                    "01000",
                    "tables were not subscribed, you will have to run ALTER SUBSCRIPTION ... \
                     REFRESH PUBLICATION to subscribe the tables"
                        .into(),
                );
            }
            SubscriptionOp::Alter {
                kind,
                name,
                conninfo,
                publications,
                options,
            } => {
                let mut d = self.found_subscription(&name)?;
                self.subscription_owner_check(&d, &name)?;
                let enabled = d.get_bool("enabled").unwrap_or(false);
                let has_slot = d.get_str("slot").is_ok();
                let current: Vec<String> = d
                    .get_array("publications")
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                match kind {
                    AlterKind::Options => {
                        let o = parse_options(
                            &options,
                            SUBOPT_SLOT_NAME
                                | SUBOPT_SYNCHRONOUS_COMMIT
                                | SUBOPT_BINARY
                                | SUBOPT_STREAMING
                                | SUBOPT_DISABLE_ON_ERR,
                        )?;
                        if o.given(SUBOPT_SLOT_NAME) {
                            if enabled && o.slot_name.is_none() {
                                return Err(error(
                                    "55000",
                                    "cannot set slot_name = NONE for enabled subscription".into(),
                                    None,
                                ));
                            }
                            match &o.slot_name {
                                Some(s) => d.insert("slot", s),
                                None => d.remove("slot"),
                            };
                        }
                        if let Some(s) = &o.synchronous_commit {
                            d.insert("synccommit", s);
                        }
                        if o.given(SUBOPT_BINARY) {
                            d.insert("binary", o.binary);
                        }
                        if o.given(SUBOPT_STREAMING) {
                            d.insert("streaming", o.streaming);
                        }
                        if o.given(SUBOPT_DISABLE_ON_ERR) {
                            d.insert("disableonerr", o.disableonerr);
                        }
                    }
                    AlterKind::Enabled => {
                        let o = parse_options(&options, SUBOPT_ENABLED)?;
                        if !has_slot && o.enabled {
                            return Err(error(
                                "55000",
                                "cannot enable subscription that does not have a slot name".into(),
                                None,
                            ));
                        }
                        if o.enabled {
                            return Err(needs_publisher("ALTER SUBSCRIPTION ... ENABLE"));
                        }
                        d.insert("enabled", false);
                    }
                    AlterKind::Connection => {
                        check_conninfo(&conninfo)?;
                        d.insert("conninfo", &conninfo);
                    }
                    AlterKind::SetPublication => {
                        let o = parse_options(&options, SUBOPT_COPY_DATA | SUBOPT_REFRESH)?;
                        check_duplicates(&publications)?;
                        if o.refresh {
                            if !enabled {
                                return Err(error(
                                    "55000",
                                    "ALTER SUBSCRIPTION with refresh is not allowed for disabled \
                                     subscriptions"
                                        .into(),
                                    Some(
                                        "Use ALTER SUBSCRIPTION ... SET PUBLICATION ... WITH \
                                         (refresh = false).",
                                    ),
                                ));
                            }
                            return Err(needs_publisher("ALTER SUBSCRIPTION with refresh"));
                        }
                        d.insert("publications", publications);
                    }
                    AlterKind::AddPublication | AlterKind::DropPublication => {
                        let add = kind == AlterKind::AddPublication;
                        let o = parse_options(&options, SUBOPT_REFRESH | SUBOPT_COPY_DATA)?;
                        check_duplicates(&publications)?;
                        let mut list = current.clone();
                        for p in &publications {
                            match list.iter().position(|x| x == p) {
                                Some(_) if add => {
                                    return Err(error(
                                        "42710",
                                        format!(
                                            "publication \"{p}\" is already in subscription \
                                             \"{name}\""
                                        ),
                                        None,
                                    ))
                                }
                                Some(i) => {
                                    list.remove(i);
                                }
                                None if add => list.push(p.clone()),
                                None => {
                                    return Err(error(
                                        "42P17",
                                        format!(
                                            "publication \"{p}\" is not in subscription \
                                             \"{name}\""
                                        ),
                                        None,
                                    ))
                                }
                            }
                        }
                        if list.is_empty() {
                            return Err(error(
                                "42P17",
                                "cannot drop all the publications from a subscription".into(),
                                None,
                            ));
                        }
                        if o.refresh {
                            if !enabled {
                                let hint = if add {
                                    "Use ALTER SUBSCRIPTION ... ADD PUBLICATION ... WITH \
                                     (refresh = false) instead."
                                } else {
                                    "Use ALTER SUBSCRIPTION ... DROP PUBLICATION ... WITH \
                                     (refresh = false) instead."
                                };
                                return Err(error(
                                    "55000",
                                    "ALTER SUBSCRIPTION with refresh is not allowed for disabled \
                                     subscriptions"
                                        .into(),
                                    Some(hint),
                                ));
                            }
                            return Err(needs_publisher("ALTER SUBSCRIPTION with refresh"));
                        }
                        d.insert("publications", list);
                    }
                    AlterKind::Refresh => {
                        if !enabled {
                            return Err(error(
                                "55000",
                                "ALTER SUBSCRIPTION ... REFRESH is not allowed for disabled \
                                 subscriptions"
                                    .into(),
                                None,
                            ));
                        }
                        return Err(needs_publisher("ALTER SUBSCRIPTION ... REFRESH"));
                    }
                    AlterKind::Skip => {
                        let o = parse_options(&options, SUBOPT_LSN)?;
                        if !self.is_superuser(&self.current_role_name()) {
                            return Err(error(
                                "42501",
                                "must be superuser to skip transaction".into(),
                                None,
                            ));
                        }
                        // No origin has progressed: nothing applies here.
                        d.insert("skiplsn", o.lsn as i64);
                    }
                }
                self.put_subscription(&name, d)?;
            }
            SubscriptionOp::Drop { name, if_exists } => {
                let Some(d) = self.subscription_doc(&name) else {
                    if if_exists {
                        self.notice(
                            "00000",
                            format!("subscription \"{name}\" does not exist, skipping"),
                            None,
                        );
                        return Ok(vec![Response::Execution(Tag::new(tag))]);
                    }
                    return Err(error(
                        "42704",
                        format!("subscription \"{name}\" does not exist"),
                        None,
                    ));
                };
                self.subscription_owner_check(&d, &name)?;
                if d.get_str("slot").is_ok() {
                    self.refuse_in_transaction_block("DROP SUBSCRIPTION")?;
                    return Err(needs_publisher(
                        "DROP SUBSCRIPTION of a subscription with a replication slot",
                    ));
                }
                self.delete_subscription(&name)?;
            }
            SubscriptionOp::Rename { name, to } => {
                let mut d = self.found_subscription(&name)?;
                if self.subscription_doc(&to).is_some() {
                    return Err(error(
                        "42710",
                        format!("subscription \"{to}\" already exists"),
                        None,
                    ));
                }
                self.subscription_owner_check(&d, &name)?;
                d.insert("_id", &to);
                self.delete_subscription(&name)?;
                self.put_subscription(&to, d)?;
            }
            SubscriptionOp::Owner { name, owner } => {
                let mut d = self.found_subscription(&name)?;
                let owner = self.grantee_name(&owner)?;
                if self.role(&owner)?.is_none() && owner != self.session_user_name() {
                    return Err(error(
                        "42704",
                        format!("role \"{owner}\" does not exist"),
                        None,
                    ));
                }
                if d.get_str("owner") != Ok(owner.as_str()) {
                    self.subscription_owner_check(&d, &name)?;
                    if !self.is_superuser(&owner) {
                        return Err(error(
                            "42501",
                            format!("permission denied to change owner of subscription \"{name}\""),
                            Some("The owner of a subscription must be a superuser."),
                        ));
                    }
                    d.insert("owner", owner);
                    self.put_subscription(&name, d)?;
                }
            }
        }
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// `COMMENT ON SUBSCRIPTION name`: kept in `pg_description`, as
    /// PostgreSQL keeps it (not `pg_shdescription`).
    pub(crate) fn comment_on_subscription(
        &self,
        name: &str,
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let mut d = self.found_subscription(name)?;
        match comment {
            Some(c) => d.insert("comment", c),
            None => d.remove("comment"),
        };
        self.put_subscription(name, d)
    }

    /// `(oid, comment)` of each subscription with one.
    pub(crate) fn subscription_comments(&self) -> Vec<(i64, String)> {
        self.subscription_docs()
            .iter()
            .filter_map(|d| {
                Some((
                    d.get("oid").and_then(bson_i64)?,
                    d.get_str("comment").ok()?.to_string(),
                ))
            })
            .collect()
    }

    /// `pg_subscription`, `pg_replication_origin`, `pg_stat_subscription`
    /// or `pg_stat_subscription_stats`.
    pub(crate) fn subscription_rows(&self, name: &str, def: &TableDef) -> Vec<Document> {
        self.all_subscription_docs()
            .iter()
            .map(|s| {
                let mut d = Document::new();
                let mut put = |col: &str, v: Bson| {
                    if let Some(f) = def.field_of(col) {
                        d.insert(f, v);
                    }
                };
                let oid = s.get("oid").and_then(bson_i64).unwrap_or(0);
                let sname = Bson::String(s.get_str("_id").unwrap_or_default().into());
                let flag = |k: &str| Bson::Boolean(s.get_bool(k).unwrap_or(false));
                match name {
                    "pg_subscription" => {
                        put("oid", Bson::Int64(oid));
                        put(
                            "subdbid",
                            Bson::Int64(s.get("db").and_then(bson_i64).unwrap_or(0)),
                        );
                        put(
                            "subskiplsn",
                            Bson::String(secantus_pgplan::systypes::render_lsn(
                                s.get("skiplsn").and_then(bson_i64).unwrap_or(0) as u64,
                            )),
                        );
                        put("subname", sname);
                        put(
                            "subowner",
                            Bson::Int64(self.role_oid_of(s.get_str("owner").unwrap_or_default())),
                        );
                        put("subenabled", flag("enabled"));
                        put("subbinary", flag("binary"));
                        put("substream", flag("streaming"));
                        put(
                            "subtwophasestate",
                            Bson::String(s.get_str("twophase").unwrap_or("d").into()),
                        );
                        put("subdisableonerr", flag("disableonerr"));
                        put(
                            "subconninfo",
                            Bson::String(s.get_str("conninfo").unwrap_or_default().into()),
                        );
                        put(
                            "subslotname",
                            s.get_str("slot")
                                .map_or(Bson::Null, |v| Bson::String(v.into())),
                        );
                        put(
                            "subsynccommit",
                            Bson::String(s.get_str("synccommit").unwrap_or("off").into()),
                        );
                        put(
                            "subpublications",
                            s.get("publications")
                                .cloned()
                                .unwrap_or(Bson::Array(Vec::new())),
                        );
                    }
                    "pg_replication_origin" => {
                        put(
                            "roident",
                            Bson::Int64(s.get("roident").and_then(bson_i64).unwrap_or(0)),
                        );
                        put("roname", Bson::String(format!("pg_{oid}")));
                    }
                    "pg_stat_subscription" => {
                        put("subid", Bson::Int64(oid));
                        put("subname", sname);
                        for c in [
                            "pid",
                            "relid",
                            "received_lsn",
                            "last_msg_send_time",
                            "last_msg_receipt_time",
                            "latest_end_lsn",
                            "latest_end_time",
                        ] {
                            put(c, Bson::Null);
                        }
                    }
                    _ => {
                        put("subid", Bson::Int64(oid));
                        put("subname", sname);
                        put("apply_error_count", Bson::Int64(0));
                        put("sync_error_count", Bson::Int64(0));
                        put("stats_reset", Bson::Null);
                    }
                }
                d
            })
            .collect()
    }
}
