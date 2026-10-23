//! `CREATE / DROP / ALTER CONVERSION`: a database's own conversions, kept
//! in `__sql_conversions__` and listed in `pg_conversion` after the
//! built-in rows. A conversion's function is one of PostgreSQL's built-in
//! conversion procedures (the only ones this server has: a C function
//! cannot be created here), so a user conversion converts as the built-in
//! one of that pair does. Messages measured on PostgreSQL 15.19.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::TableDef;
use secantus_pgplan::misc_alter::{encoding, encoding_name, ConversionOp};

use crate::PgHandler;

pub(crate) const CONVERSION_COLLECTION: &str = "__sql_conversions__";

/// The signature PostgreSQL looks a conversion function up by.
const CONVERSION_ARGS: &str = "integer, integer, cstring, internal, integer, boolean";

impl PgHandler {
    fn conversion_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(CONVERSION_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// `[schema.]name` to `(schema, name)`: unqualified, the first schema
    /// on the search path holding it (`create` false) or the creation
    /// schema (`create` true).
    fn conversion_name(&self, parts: &[String], create: bool) -> PgWireResult<(String, String)> {
        match parts {
            [schema, name] => {
                self.require_schema(schema)?;
                Ok((schema.clone(), name.clone()))
            }
            [name] => {
                if !create {
                    if let Some(s) = secantus_pgplan::schemas::search_path()
                        .into_iter()
                        .find(|s| {
                            self.conversion_docs().iter().any(|d| {
                                d.get_str("schema") == Ok(s.as_str())
                                    && d.get_str("name") == Ok(name)
                            })
                        })
                    {
                        return Ok((s, name.clone()));
                    }
                }
                Ok((
                    secantus_pgplan::schemas::creation_schema().map_err(|e| Self::err(&e))?,
                    name.clone(),
                ))
            }
            _ => Err(Self::user_error("42601", "improper conversion name".into())),
        }
    }

    fn conversion_doc(&self, schema: &str, name: &str) -> Option<Document> {
        self.conversion_docs()
            .into_iter()
            .find(|d| d.get_str("schema") == Ok(schema) && d.get_str("name") == Ok(name))
    }

    fn found_conversion(&self, parts: &[String]) -> PgWireResult<Document> {
        let (schema, name) = self.conversion_name(parts, false)?;
        self.conversion_doc(&schema, &name).ok_or_else(|| {
            Self::user_error(
                "42704",
                format!("conversion \"{}\" does not exist", parts.join(".")),
            )
        })
    }

    fn write_conversion(&self, doc: Document) -> PgWireResult<()> {
        let id = doc.get_str("_id").unwrap_or_default().to_string();
        self.put(CONVERSION_COLLECTION, &id, doc)
    }

    pub(crate) fn execute_conversion(&self, op: ConversionOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        match op {
            ConversionOp::Create {
                name,
                for_encoding,
                to_encoding,
                function,
                default,
            } => {
                let (schema, name) = self.conversion_name(&name, true)?;
                let (for_id, for_name) = encoding(&for_encoding).ok_or_else(|| {
                    Self::user_error(
                        "42704",
                        format!("source encoding \"{for_encoding}\" does not exist"),
                    )
                })?;
                let (to_id, to_name) = encoding(&to_encoding).ok_or_else(|| {
                    Self::user_error(
                        "42704",
                        format!("destination encoding \"{to_encoding}\" does not exist"),
                    )
                })?;
                // The function: a built-in conversion procedure of that name.
                let func = function.last().cloned().unwrap_or_default();
                let pairs = crate::system_catalogs::builtin_conversion_pairs();
                let oids = secantus_pgplan::regobj::builtin_proc_oids(&func);
                let Some(proc_oid) = oids
                    .iter()
                    .copied()
                    .find(|o| pairs.iter().any(|(_, _, p)| p == o))
                else {
                    return Err(Self::user_error(
                        "42883",
                        format!(
                            "function {}({CONVERSION_ARGS}) does not exist",
                            function.join(".")
                        ),
                    ));
                };
                // PostgreSQL calls it on an empty string, and it checks the
                // pair it was given.
                let served: Vec<(i64, i64)> = pairs
                    .iter()
                    .filter(|(_, _, p)| *p == proc_oid)
                    .map(|(f, t, _)| (*f, *t))
                    .collect();
                let (f, t) = (i64::from(for_id), i64::from(to_id));
                if !served.contains(&(f, t)) {
                    let message = match served.iter().find(|(sf, _)| *sf == f) {
                        Some((_, st)) => format!(
                            "expected destination encoding \"{}\", but got \"{to_name}\"",
                            encoding_name(*st).unwrap_or_default()
                        ),
                        None => format!(
                            "expected source encoding \"{}\", but got \"{for_name}\"",
                            served
                                .first()
                                .and_then(|(sf, _)| encoding_name(*sf))
                                .unwrap_or_default()
                        ),
                    };
                    return Err(Self::user_error("XX000", message));
                }
                let docs = self.conversion_docs();
                if docs.iter().any(|d| {
                    d.get_str("schema") == Ok(schema.as_str()) && d.get_str("name") == Ok(&name)
                }) {
                    return Err(Self::user_error(
                        "42710",
                        format!("conversion \"{name}\" already exists"),
                    ));
                }
                if default
                    && docs.iter().any(|d| {
                        d.get_str("schema") == Ok(schema.as_str())
                            && d.get_bool("default").unwrap_or(false)
                            && d.get_i64("for").ok() == Some(f)
                            && d.get_i64("to").ok() == Some(t)
                    })
                {
                    return Err(Self::user_error(
                        "42710",
                        format!("default conversion for {for_name} to {to_name} already exists"),
                    ));
                }
                let id = format!("{schema}.{name}");
                self.ensure_collection(CONVERSION_COLLECTION)?;
                self.write_conversion(bson::doc! {
                    "_id": &id,
                    "schema": &schema,
                    "name": &name,
                    "owner": self.current_role_name(),
                    "for": f,
                    "to": t,
                    "proc": proc_oid,
                    "default": default,
                    "oid": Self::index_oid(&format!("conv:{id}")),
                })?;
            }
            ConversionOp::Drop { names, if_exists } => {
                let mut doomed = Vec::new();
                for parts in &names {
                    match self.found_conversion(parts) {
                        Ok(d) => doomed.push(d),
                        Err(_) if if_exists => self.notice(
                            "00000",
                            format!(
                                "conversion \"{}\" does not exist, skipping",
                                parts.join(".")
                            ),
                            None,
                        ),
                        Err(e) => return Err(e),
                    }
                }
                for d in doomed {
                    self.delete_type_doc(CONVERSION_COLLECTION, d.get_str("_id").unwrap_or(""))?;
                }
            }
            ConversionOp::Rename { name, to } => {
                let mut d = self.found_conversion(&name)?;
                let schema = d.get_str("schema").unwrap_or("public").to_string();
                if self.conversion_doc(&schema, &to).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!("conversion \"{to}\" already exists in schema \"{schema}\""),
                    ));
                }
                let old = d.get_str("_id").unwrap_or_default().to_string();
                let id = format!("{schema}.{to}");
                d.insert("_id", &id);
                d.insert("name", &to);
                self.delete_type_doc(CONVERSION_COLLECTION, &old)?;
                self.write_conversion(d)?;
            }
            ConversionOp::Owner { name, owner } => {
                let mut d = self.found_conversion(&name)?;
                let owner = self.grantee_name(&owner)?;
                if self.role(&owner)?.is_none() && owner != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{owner}\" does not exist"),
                    ));
                }
                d.insert("owner", owner);
                self.write_conversion(d)?;
            }
            ConversionOp::SetSchema { name, schema } => {
                let mut d = self.found_conversion(&name)?;
                self.require_schema(&schema)?;
                let bare = d.get_str("name").unwrap_or_default().to_string();
                if d.get_str("schema") == Ok(schema.as_str()) {
                    return Ok(vec![Response::Execution(Tag::new(tag))]);
                }
                if self.conversion_doc(&schema, &bare).is_some() {
                    return Err(Self::user_error(
                        "42710",
                        format!("conversion \"{bare}\" already exists in schema \"{schema}\""),
                    ));
                }
                let old = d.get_str("_id").unwrap_or_default().to_string();
                d.insert("_id", format!("{schema}.{bare}"));
                d.insert("schema", &schema);
                self.delete_type_doc(CONVERSION_COLLECTION, &old)?;
                self.write_conversion(d)?;
            }
        }
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// `COMMENT ON CONVERSION name IS ...`, kept on its row.
    pub(crate) fn comment_on_conversion(
        &self,
        names: &[String],
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let mut d = self.found_conversion(names)?;
        match comment {
            Some(c) => d.insert("comment", c),
            None => d.remove("comment"),
        };
        self.write_conversion(d)
    }

    /// `(oid, comment)` of each conversion with one.
    pub(crate) fn conversion_comments(&self) -> Vec<(i64, String)> {
        self.conversion_docs()
            .iter()
            .filter_map(|d| {
                Some((
                    d.get_i64("oid").ok()?,
                    d.get_str("comment").ok()?.to_string(),
                ))
            })
            .collect()
    }

    /// The database's conversions as `pg_conversion` rows.
    pub(crate) fn user_conversion_rows(&self, def: &TableDef) -> Vec<Document> {
        let field = |n: &str| def.field_of(n);
        self.conversion_docs()
            .iter()
            .map(|c| {
                let mut d = Document::new();
                let mut put = |k: &str, v: Bson| {
                    if let Some(f) = field(k) {
                        d.insert(f, v);
                    }
                };
                put("oid", Bson::Int64(c.get_i64("oid").unwrap_or(0)));
                put(
                    "conname",
                    Bson::String(c.get_str("name").unwrap_or_default().into()),
                );
                put(
                    "connamespace",
                    Bson::Int64(self.namespace_oid(c.get_str("schema").unwrap_or("public"))),
                );
                put(
                    "conowner",
                    Bson::Int64(self.role_oid_of(c.get_str("owner").unwrap_or_default())),
                );
                put(
                    "conforencoding",
                    Bson::Int32(c.get_i64("for").unwrap_or(0) as i32),
                );
                put(
                    "contoencoding",
                    Bson::Int32(c.get_i64("to").unwrap_or(0) as i32),
                );
                put(
                    "conproc",
                    secantus_pgplan::regobj::value("regproc", c.get_i64("proc").unwrap_or(0)),
                );
                put(
                    "condefault",
                    Bson::Boolean(c.get_bool("default").unwrap_or(false)),
                );
                d
            })
            .collect()
    }
}
