//! `CREATE / DROP TRANSFORM` (RP-169), as PostgreSQL 15.19's
//! `CreateTransform` checks them: a transform is its `pg_transform` row,
//! which depends on its type (DROP TYPE refuses while one names it). No
//! language here calls a transform's functions: SQL and PL/pgSQL never do.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::transforms::{TransformFunction, TransformOp};

use crate::{bson_i64, PgHandler};

pub(crate) const TRANSFORM_COLLECTION: &str = "__sql_transforms__";
pub(crate) const PG_TRANSFORM: i64 = 3576;

const INTERNAL: i64 = 2281;

/// The languages this server has, as `pg_language` lists them.
pub(crate) fn language_oid(name: &str) -> Option<i64> {
    Some(match name {
        "internal" => 12,
        "c" => 13,
        "sql" => 14,
        "plpgsql" => 14035,
        _ => return None,
    })
}

fn language_name(oid: i64) -> &'static str {
    match oid {
        12 => "internal",
        13 => "c",
        14 => "sql",
        14035 => "plpgsql",
        _ => "",
    }
}

fn invalid(m: &str) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "42P17".into(),
        m.into(),
    )))
}

fn transform_key(type_oid: i64, lang: i64) -> String {
    format!("{type_oid}/{lang}")
}

impl PgHandler {
    fn transform_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(TRANSFORM_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// `getObjectDescription` of a transform.
    pub(crate) fn transform_description(&self, type_oid: i64, lang: i64) -> String {
        format!(
            "transform for {} language {}",
            secantus_pgplan::regtype_text(type_oid),
            language_name(lang)
        )
    }

    /// The type a transform names, and its language: 42704 for either
    /// missing.
    fn transform_target(&self, type_name: &str, written: &str) -> PgWireResult<i64> {
        self.any_type_oid(type_name)
            .ok_or_else(|| Self::user_error("42704", format!("type \"{written}\" does not exist")))
    }

    fn transform_language(lang: &str) -> PgWireResult<i64> {
        language_oid(lang)
            .ok_or_else(|| Self::user_error("42704", format!("language \"{lang}\" does not exist")))
    }

    /// `check_transform_function` and the return-type check before it.
    fn transform_function(
        &self,
        f: &TransformFunction,
        from_sql: bool,
        type_oid: i64,
    ) -> PgWireResult<i64> {
        let args: Vec<i64> =
            f.1.iter()
                .map(|t| self.any_type_oid(t).unwrap_or(0))
                .collect();
        let p = self.lookup_proc(&f.0, Some(&args))?;
        if from_sql && p.ret != INTERNAL {
            return Err(invalid(
                "return data type of FROM SQL function must be internal",
            ));
        }
        if !from_sql && p.ret != type_oid {
            return Err(invalid(
                "return data type of TO SQL function must be the transform data type",
            ));
        }
        if p.volatile == 'v' {
            return Err(invalid("transform function must not be volatile"));
        }
        if p.kind != 'f' {
            return Err(invalid("transform function must be a normal function"));
        }
        if p.retset {
            return Err(invalid("transform function must not return a set"));
        }
        if p.args.len() != 1 {
            return Err(invalid("transform function must take one argument"));
        }
        if p.args[0] != INTERNAL {
            return Err(invalid(
                "first argument of transform function must be type internal",
            ));
        }
        Ok(p.oid)
    }

    pub(crate) fn transform_statement(&self, op: TransformOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        match op {
            TransformOp::Create {
                replace,
                type_name,
                type_written,
                language,
                from_sql,
                to_sql,
            } => {
                let type_oid = self.transform_target(&type_name, &type_written)?;
                let shown = secantus_pgplan::regtype_text(type_oid);
                if secantus_pgplan::pgtypes::PSEUDO_TYPES
                    .iter()
                    .any(|(o, _)| *o == type_oid)
                {
                    return Err(Self::user_error(
                        "42809",
                        format!("data type {shown} is a pseudo-type"),
                    ));
                }
                if secantus_pgplan::domains::user_domain(&type_name).is_some() {
                    return Err(Self::user_error(
                        "42809",
                        format!("data type {shown} is a domain"),
                    ));
                }
                let role = self.current_role_name();
                if !self.is_superuser(&role) {
                    let owner = self
                        .type_owner_oids()
                        .into_iter()
                        .find(|(n, _)| *n == type_name)
                        .map_or(10, |(_, o)| o);
                    if owner != self.role_oid_of(&role) {
                        return Err(Self::user_error(
                            "42501",
                            format!("must be owner of type {shown}"),
                        ));
                    }
                }
                let lang = Self::transform_language(&language)?;
                let from = match &from_sql {
                    Some(f) => self.transform_function(f, true, type_oid)?,
                    None => 0,
                };
                let to = match &to_sql {
                    Some(f) => self.transform_function(f, false, type_oid)?,
                    None => 0,
                };
                let key = transform_key(type_oid, lang);
                let existing = self
                    .transform_docs()
                    .into_iter()
                    .find(|d| d.get_str("_id") == Ok(key.as_str()));
                let (oid, comment) = match &existing {
                    Some(_) if !replace => {
                        return Err(Self::user_error(
                            "42710",
                            format!(
                                "transform for type {shown} language \"{language}\" already exists"
                            ),
                        ))
                    }
                    Some(d) => (
                        d.get("oid").and_then(bson_i64).unwrap_or(0),
                        d.get_str("comment").ok().map(str::to_string),
                    ),
                    None => (Self::index_oid(&format!("trf:{key}")), None),
                };
                let mut doc = bson::doc! {
                    "_id": &key,
                    "oid": oid,
                    "type": type_oid,
                    "type_name": &type_name,
                    "lang": lang,
                    "fromsql": from,
                    "tosql": to,
                };
                if let Some(c) = comment {
                    doc.insert("comment", c);
                }
                self.put(TRANSFORM_COLLECTION, &key, doc)?;
            }
            TransformOp::Drop {
                type_name,
                type_written,
                language,
                if_exists,
            } => {
                let found = self
                    .any_type_oid(&type_name)
                    .zip(language_oid(&language))
                    .and_then(|(t, l)| {
                        let key = transform_key(t, l);
                        self.transform_docs()
                            .into_iter()
                            .find(|d| d.get_str("_id") == Ok(key.as_str()))
                            .map(|_| key)
                    });
                let Some(key) = found else {
                    if if_exists {
                        self.notice(
                            "00000",
                            format!(
                                "transform for type {type_written} language \"{language}\" does \
                                 not exist, skipping"
                            ),
                            None,
                        );
                        return Ok(vec![Response::Execution(Tag::new(tag))]);
                    }
                    let type_oid = self.transform_target(&type_name, &type_written)?;
                    Self::transform_language(&language)?;
                    return Err(Self::user_error(
                        "42704",
                        format!(
                            "transform for type {} language \"{language}\" does not exist",
                            secantus_pgplan::regtype_text(type_oid)
                        ),
                    ));
                };
                self.delete_type_doc(TRANSFORM_COLLECTION, &key)?;
            }
        }
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// `COMMENT ON TRANSFORM FOR type LANGUAGE lang`: `names` is the type's
    /// parts, then the language.
    pub(crate) fn comment_on_transform(
        &self,
        names: &[String],
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let Some((lang, type_parts)) = names.split_last() else {
            return Ok(());
        };
        let type_name = type_parts
            .last()
            .map(|t| {
                if type_parts.len() > 1 && type_parts[0] == "pg_catalog" {
                    t.clone()
                } else {
                    type_parts.join(".")
                }
            })
            .unwrap_or_default();
        let type_oid = self.transform_target(&type_name, &type_parts.join("."))?;
        let l = Self::transform_language(lang)?;
        let key = transform_key(type_oid, l);
        let Some(mut d) = self
            .transform_docs()
            .into_iter()
            .find(|d| d.get_str("_id") == Ok(key.as_str()))
        else {
            return Err(Self::user_error(
                "42704",
                format!(
                    "transform for type {} language \"{lang}\" does not exist",
                    secantus_pgplan::regtype_text(type_oid)
                ),
            ));
        };
        match comment {
            Some(c) => d.insert("comment", c),
            None => d.remove("comment"),
        };
        self.put(TRANSFORM_COLLECTION, &key, d)
    }

    /// `(oid, comment)` of each transform with one.
    pub(crate) fn transform_comments(&self) -> Vec<(i64, String)> {
        self.transform_docs()
            .iter()
            .filter_map(|d| {
                Some((
                    d.get("oid").and_then(bson_i64)?,
                    d.get_str("comment").ok()?.to_string(),
                ))
            })
            .collect()
    }

    /// The transforms over a type: `(key, description)`, for DROP TYPE.
    pub(crate) fn transforms_on_type(&self, type_oid: i64) -> Vec<(String, String)> {
        self.transform_docs()
            .iter()
            .filter(|d| d.get("type").and_then(bson_i64) == Some(type_oid))
            .map(|d| {
                (
                    d.get_str("_id").unwrap_or_default().to_string(),
                    self.transform_description(
                        type_oid,
                        d.get("lang").and_then(bson_i64).unwrap_or(0),
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn drop_transform_key(&self, key: &str) -> PgWireResult<()> {
        self.delete_type_doc(TRANSFORM_COLLECTION, key)
    }

    pub(crate) fn transform_rows(&self, def: &TableDef) -> Vec<Document> {
        self.transform_docs()
            .iter()
            .map(|t| {
                let mut d = Document::new();
                let mut put = |col: &str, v: Bson| {
                    if let Some(f) = def.field_of(col) {
                        d.insert(f, v);
                    }
                };
                let i = |k: &str| t.get(k).and_then(bson_i64).unwrap_or(0);
                put("oid", Bson::Int64(i("oid")));
                put("trftype", Bson::Int64(i("type")));
                put("trflang", Bson::Int64(i("lang")));
                put(
                    "trffromsql",
                    secantus_pgplan::regobj::value("regproc", i("fromsql")),
                );
                put(
                    "trftosql",
                    secantus_pgplan::regobj::value("regproc", i("tosql")),
                );
                d
            })
            .collect()
    }
}
