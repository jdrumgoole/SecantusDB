//! `CREATE CAST` / `DROP CAST` and the `pg_cast` catalog.
//!
//! PostgreSQL's own casts are its 229 `pg_cast` rows (PostgreSQL 15, oids
//! below 16384): they answer `pg_cast` and are what "already exists" is
//! checked against. A user cast is stored in `__sql_casts__` and installed
//! into the planner (`secantus_pgplan::user_casts`), which evaluates it.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::PgWireResult;
use secantus_pgcatalog::{Column, TableDef};
use secantus_pgplan::user_casts::{type_oid, UserCast};

use crate::PgHandler;

pub(crate) const CAST_COLLECTION: &str = "__sql_casts__";

use secantus_pgplan::pg_casts::BUILTIN_CASTS;

/// `(type oid, typlen, typbyval, typalign)` of PostgreSQL 15's scalar types:
/// what a `WITHOUT FUNCTION` cast must match on both sides.
const TYPE_LAYOUT: &[(i64, i64, bool, u8)] = &[
    (16, 1, true, b'c'),
    (17, -1, false, b'i'),
    (18, 1, true, b'c'),
    (20, 8, true, b'd'),
    (21, 2, true, b's'),
    (23, 4, true, b'i'),
    (24, 4, true, b'i'),
    (25, -1, false, b'i'),
    (26, 4, true, b'i'),
    (27, 6, false, b's'),
    (28, 4, true, b'i'),
    (29, 4, true, b'i'),
    (32, 8, true, b'd'),
    (114, -1, false, b'i'),
    (142, -1, false, b'i'),
    (194, -1, false, b'i'),
    (269, 4, true, b'i'),
    (325, 4, true, b'i'),
    (602, -1, false, b'd'),
    (604, -1, false, b'd'),
    (650, -1, false, b'i'),
    (700, 4, true, b'i'),
    (701, 8, true, b'd'),
    (705, -2, false, b'c'),
    (718, 24, false, b'd'),
    (774, 8, false, b'i'),
    (790, 8, true, b'd'),
    (829, 6, false, b'i'),
    (869, -1, false, b'i'),
    (1033, 12, false, b'i'),
    (1042, -1, false, b'i'),
    (1043, -1, false, b'i'),
    (1082, 4, true, b'i'),
    (1083, 8, true, b'd'),
    (1114, 8, true, b'd'),
    (1184, 8, true, b'd'),
    (1186, 16, false, b'd'),
    (1266, 12, false, b'd'),
    (1560, -1, false, b'i'),
    (1562, -1, false, b'i'),
    (1700, -1, false, b'i'),
    (1790, -1, false, b'i'),
    (2202, 4, true, b'i'),
    (2203, 4, true, b'i'),
    (2204, 4, true, b'i'),
    (2205, 4, true, b'i'),
    (2206, 4, true, b'i'),
    (2249, -1, false, b'd'),
    (2275, -2, false, b'c'),
    (2276, 4, true, b'i'),
    (2277, -1, false, b'd'),
    (2278, 4, true, b'i'),
    (2279, 4, true, b'i'),
    (2280, 4, true, b'i'),
    (2281, 8, true, b'd'),
    (2283, 4, true, b'i'),
    (2776, 4, true, b'i'),
    (2950, 16, false, b'c'),
    (2970, -1, false, b'd'),
    (3115, 4, true, b'i'),
    (3220, 8, true, b'd'),
    (3310, 4, true, b'i'),
    (3361, -1, false, b'i'),
    (3402, -1, false, b'i'),
    (3500, 4, true, b'i'),
    (3614, -1, false, b'i'),
    (3615, -1, false, b'i'),
    (3642, -1, false, b'i'),
    (3734, 4, true, b'i'),
    (3769, 4, true, b'i'),
    (3802, -1, false, b'i'),
    (3831, -1, false, b'd'),
    (3838, 4, true, b'i'),
    (3904, -1, false, b'i'),
    (3906, -1, false, b'i'),
    (3908, -1, false, b'd'),
    (3910, -1, false, b'd'),
    (3912, -1, false, b'i'),
    (3926, -1, false, b'd'),
    (4072, -1, false, b'i'),
    (4089, 4, true, b'i'),
    (4096, 4, true, b'i'),
    (4191, 4, true, b'i'),
    (4451, -1, false, b'i'),
    (4532, -1, false, b'i'),
    (4533, -1, false, b'd'),
    (4534, -1, false, b'd'),
    (4535, -1, false, b'i'),
    (4536, -1, false, b'd'),
    (4537, -1, false, b'd'),
    (4538, -1, false, b'd'),
    (4600, -1, false, b'i'),
    (4601, -1, false, b'i'),
    (5017, -1, false, b'i'),
    (5038, -1, false, b'd'),
    (5069, 8, true, b'd'),
    (5077, 4, true, b'i'),
    (5078, -1, false, b'd'),
    (5079, 4, true, b'i'),
    (5080, -1, false, b'd'),
];

/// Is there an ASSIGNMENT coercion from built-in type `source` to `target`
/// (`find_coercion_pathway` under `COERCION_ASSIGNMENT`)? The same type; a
/// `pg_cast` row of context `a` or `i`; any type into the string category
/// (text, varchar, bpchar, name) by its output function; an array into an
/// array whose elements have one. A type this table does not know (a user
/// type, a domain) is given the benefit of the doubt: that is the caller's
/// own coercion's to refuse.
pub(crate) fn assignment_path(source: i64, target: i64) -> bool {
    const UNKNOWN: i64 = 705;
    if source == target || source == UNKNOWN || source <= 0 || target <= 0 {
        return true;
    }
    let element = |oid: i64| {
        secantus_pgplan::pgtypes::BUILTIN_TYPES
            .iter()
            .find(|(_, _, arr)| *arr == oid)
            .map(|(_, el, _)| *el)
    };
    let known = |oid: i64| {
        secantus_pgplan::pgtypes::BUILTIN_TYPES
            .iter()
            .any(|(_, el, arr)| *el == oid || *arr == oid)
    };
    if !known(source) || !known(target) {
        return true;
    }
    if BUILTIN_CASTS
        .iter()
        .any(|(_, s, t, _, c, _)| *s == source && *t == target && matches!(c, b'a' | b'i'))
    {
        return true;
    }
    if matches!(target, 25 | 1043 | 1042 | 19) {
        return true;
    }
    match (element(source), element(target)) {
        (Some(s), Some(t)) => assignment_path(s, t),
        _ => false,
    }
}

/// Is there an IMPLICIT built-in cast from type `source` to `target`?
pub(crate) fn implicit_cast(source: i64, target: i64) -> bool {
    BUILTIN_CASTS
        .iter()
        .any(|(_, s, t, _, c, _)| *s == source && *t == target && *c == b'i')
}

fn layout(oid: i64) -> Option<(i64, bool, u8)> {
    TYPE_LAYOUT
        .iter()
        .find(|(o, ..)| *o == oid)
        .map(|(_, l, b, a)| (*l, *b, *a))
}

/// `pg_cast`.
pub(crate) fn pg_cast_def() -> TableDef {
    TableDef::new(
        "pg_cast",
        vec![
            Column::new("oid", "oid", false),
            Column::new("castsource", "oid", false),
            Column::new("casttarget", "oid", false),
            Column::new("castfunc", "oid", false),
            Column::new("castcontext", "\"char\"", false),
            Column::new("castmethod", "\"char\"", false),
        ],
    )
}

fn cast_key(source: i64, target: i64) -> String {
    format!("{source}->{target}")
}

impl PgHandler {
    /// The database's user casts, for the planner.
    pub(crate) fn user_casts(&self) -> Vec<UserCast> {
        self.type_catalog_docs(CAST_COLLECTION)
            .map(|docs| docs.iter().map(cast_of).collect())
            .unwrap_or_default()
    }

    fn display_cast_type(&self, name: &str) -> String {
        self.display_type_name(name)
    }

    pub(crate) fn create_cast(&self, mut cast: UserCast) -> PgWireResult<Vec<Response>> {
        let missing = |t: &str| Self::user_error("42704", format!("type \"{t}\" does not exist"));
        let source_oid = type_oid(&cast.source).ok_or_else(|| missing(&cast.source))?;
        let target_oid = type_oid(&cast.target).ok_or_else(|| missing(&cast.target))?;
        let invalid = |m: &str| Self::user_error("42P17", m.to_string());
        if source_oid == target_oid {
            return Err(invalid(
                "source data type and target data type are the same",
            ));
        }
        let (ds, dt) = (
            self.display_cast_type(&cast.source),
            self.display_cast_type(&cast.target),
        );
        match cast.method {
            'f' => {
                let (name, args) = cast.function.clone().unwrap_or_default();
                let same = |a: &str, b: &str| type_oid(a).is_some() && type_oid(a) == type_oid(b);
                let found = self
                    .user_function_docs()?
                    .iter()
                    .map(crate::user_fn_of)
                    .find(|u| {
                        u.name == name
                            && u.arg_types.len() == args.len()
                            && u.arg_types.iter().zip(&args).all(|(p, a)| same(p, a))
                    });
                let Some(f) = found else {
                    return Err(Self::user_error(
                        "42883",
                        format!(
                            "function {name}({}) does not exist",
                            args.iter()
                                .map(|a| self.display_cast_type(a))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                };
                if !(1..=3).contains(&f.arg_types.len()) {
                    return Err(invalid("cast function must take one to three arguments"));
                }
                if type_oid(&f.arg_types[0]) != Some(source_oid) {
                    return Err(invalid(
                        "argument of cast function must match or be binary-coercible from source data type",
                    ));
                }
                if type_oid(&f.return_type) != Some(target_oid) {
                    return Err(invalid(
                        "return data type of cast function must match or be binary-coercible to target data type",
                    ));
                }
                if f.returns_set {
                    return Err(invalid("cast function must not return a set"));
                }
            }
            'b' => {
                let is_enum = |n: &str| {
                    self.enums()
                        .unwrap_or_default()
                        .iter()
                        .any(|(e, ..)| e.eq_ignore_ascii_case(n))
                };
                let is_composite = |n: &str| secantus_pgplan::user_composite_oid(n).is_some();
                if is_enum(&cast.source) || is_enum(&cast.target) {
                    return Err(invalid("enum data types are not binary-compatible"));
                }
                if is_composite(&cast.source) || is_composite(&cast.target) {
                    return Err(invalid("composite data types are not binary-compatible"));
                }
                if let (Some(a), Some(b)) = (layout(source_oid), layout(target_oid)) {
                    if a != b {
                        return Err(invalid(
                            "source and target data types are not physically compatible",
                        ));
                    }
                }
            }
            _ => {}
        }
        let exists = BUILTIN_CASTS
            .iter()
            .any(|(_, s, t, ..)| *s == source_oid && *t == target_oid)
            || self
                .user_casts()
                .iter()
                .any(|c| c.source_oid == source_oid && c.target_oid == target_oid);
        if exists {
            return Err(Self::user_error(
                "42710",
                format!("cast from type {ds} to type {dt} already exists"),
            ));
        }
        cast.source_oid = source_oid;
        cast.target_oid = target_oid;
        let key = cast_key(source_oid, target_oid);
        cast.oid = Self::index_oid(&format!("cast:{key}"));
        let mut doc = bson::doc! {
            "_id": &key,
            "source": &cast.source,
            "target": &cast.target,
            "source_oid": source_oid,
            "target_oid": target_oid,
            "method": cast.method.to_string(),
            "context": cast.context.to_string(),
            "oid": cast.oid,
        };
        if let Some((name, args)) = &cast.function {
            doc.insert("function", name);
            doc.insert(
                "function_args",
                args.iter()
                    .map(|a| Bson::String(a.clone()))
                    .collect::<Vec<_>>(),
            );
        }
        self.put(CAST_COLLECTION, &key, doc)?;
        Ok(vec![Response::Execution(Tag::new("CREATE CAST"))])
    }

    pub(crate) fn drop_cast(
        &self,
        source: &str,
        target: &str,
        if_exists: bool,
    ) -> PgWireResult<Vec<Response>> {
        let (ds, dt) = (
            self.display_cast_type(source),
            self.display_cast_type(target),
        );
        let key = match (type_oid(source), type_oid(target)) {
            (Some(s), Some(t)) => Some(cast_key(s, t)),
            _ => None,
        };
        let found = key.as_deref().filter(|k| {
            self.user_casts()
                .iter()
                .any(|c| cast_key(c.source_oid, c.target_oid) == *k)
        });
        let Some(key) = found else {
            let message = format!("cast from type {ds} to type {dt} does not exist");
            if if_exists {
                self.notice("00000", format!("{message}, skipping"), None);
                return Ok(vec![Response::Execution(Tag::new("DROP CAST"))]);
            }
            return Err(Self::user_error("42704", message));
        };
        self.delete_type_doc(CAST_COLLECTION, key)?;
        Ok(vec![Response::Execution(Tag::new("DROP CAST"))])
    }

    /// The user casts that name type `oid`, as `from X to Y` for a
    /// dependency message.
    pub(crate) fn casts_on_type(&self, oid: i64) -> Vec<(String, String)> {
        self.user_casts()
            .into_iter()
            .filter(|c| c.source_oid == oid || c.target_oid == oid)
            .map(|c| {
                (
                    cast_key(c.source_oid, c.target_oid),
                    format!(
                        "cast from {} to {}",
                        self.display_cast_type(&c.source),
                        self.display_cast_type(&c.target)
                    ),
                )
            })
            .collect()
    }

    /// The user casts implemented by function `name(args)`.
    pub(crate) fn casts_on_function(&self, name: &str, args: &[String]) -> Vec<(String, String)> {
        let same = |a: &str, b: &str| type_oid(a).is_some() && type_oid(a) == type_oid(b);
        self.user_casts()
            .into_iter()
            .filter(|c| {
                c.function.as_ref().is_some_and(|(n, a)| {
                    n == name
                        && a.len() == args.len()
                        && a.iter().zip(args).all(|(x, y)| same(x, y))
                })
            })
            .map(|c| {
                (
                    cast_key(c.source_oid, c.target_oid),
                    format!(
                        "cast from {} to {}",
                        self.display_cast_type(&c.source),
                        self.display_cast_type(&c.target)
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn drop_cast_key(&self, key: &str) -> PgWireResult<()> {
        self.delete_type_doc(CAST_COLLECTION, key)
    }

    pub(crate) fn pg_cast_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let row = |oid: i64, s: i64, t: i64, func: i64, context: char, method: char| {
            let mut d = Document::new();
            d.insert(f("oid"), Bson::Int64(oid));
            d.insert(f("castsource"), Bson::Int64(s));
            d.insert(f("casttarget"), Bson::Int64(t));
            d.insert(f("castfunc"), Bson::Int64(func));
            d.insert(f("castcontext"), context.to_string());
            d.insert(f("castmethod"), method.to_string());
            d
        };
        let mut rows: Vec<Document> = BUILTIN_CASTS
            .iter()
            .map(|(o, s, t, func, c, m)| row(*o, *s, *t, *func, *c as char, *m as char))
            .collect();
        let functions: Vec<secantus_pgplan::UserFn> = self
            .user_function_docs()
            .map(|docs| docs.iter().map(crate::user_fn_of).collect())
            .unwrap_or_default();
        for c in self.user_casts() {
            let func = c
                .function
                .as_ref()
                .and_then(|(n, _)| functions.iter().find(|u| &u.name == n))
                .map_or(0, |u| Self::index_oid(&format!("fn:{}", u.key)));
            rows.push(row(
                c.oid,
                c.source_oid,
                c.target_oid,
                func,
                c.context,
                c.method,
            ));
        }
        rows
    }
}

fn cast_of(d: &Document) -> UserCast {
    let s = |k: &str| d.get_str(k).unwrap_or_default().to_string();
    let i = |k: &str| {
        d.get_i64(k)
            .or_else(|_| d.get_i32(k).map(i64::from))
            .unwrap_or(0)
    };
    let c = |k: &str| s(k).chars().next().unwrap_or('e');
    UserCast {
        source: s("source"),
        target: s("target"),
        source_oid: i("source_oid"),
        target_oid: i("target_oid"),
        function: d.get_str("function").ok().map(|n| {
            let args = d
                .get_array("function_args")
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (n.to_string(), args)
        }),
        method: c("method"),
        context: c("context"),
        oid: i("oid"),
    }
}

impl PgHandler {
    /// `DROP TYPE` / `DROP DOMAIN`: the functions whose signature names the
    /// type and the casts that name it depend on it. RESTRICT refuses with
    /// PostgreSQL's 2BP01, one DETAIL line each; CASCADE drops them with a
    /// notice (a cast implemented by a dropped function goes too).
    pub(crate) fn drop_type_signature_dependents(
        &self,
        name: &str,
        cascade: bool,
    ) -> PgWireResult<()> {
        let Some(oid) = type_oid(name) else {
            return Ok(());
        };
        let functions: Vec<crate::UserFunction> = self
            .functions()?
            .into_iter()
            .filter(|f| {
                f.param_types
                    .iter()
                    .chain(std::iter::once(&f.return_type))
                    .any(|t| type_oid(t) == Some(oid))
            })
            .collect();
        let mut casts = self.casts_on_type(oid);
        for f in &functions {
            for c in self.casts_on_function(&f.name, &f.param_types) {
                if !casts.iter().any(|(k, _)| *k == c.0) {
                    casts.push(c);
                }
            }
        }
        if functions.is_empty() && casts.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut lines: Vec<String> = functions
                .iter()
                .map(|f| {
                    format!(
                        "function {} depends on type {name}",
                        self.function_signature(f)
                    )
                })
                .collect();
            lines.extend(
                casts
                    .iter()
                    .map(|(_, c)| format!("{c} depends on type {name}")),
            );
            return Err(Self::dependents_error_lines(&format!("type {name}"), lines));
        }
        let mut descs: Vec<String> = functions
            .iter()
            .map(|f| format!("function {}", self.function_signature(f)))
            .collect();
        descs.extend(casts.iter().map(|(_, c)| c.clone()));
        self.cascade_notice(&descs);
        for (key, _) in &casts {
            self.drop_cast_key(key)?;
        }
        for f in &functions {
            self.delete_type_doc(Self::FUNCTION_COLLECTION, &f.key)?;
        }
        Ok(())
    }

    /// `DROP FUNCTION`: the casts it implements depend on it.
    pub(crate) fn drop_function_casts(
        &self,
        f: &crate::UserFunction,
        cascade: bool,
    ) -> PgWireResult<()> {
        let casts = self.casts_on_function(&f.name, &f.param_types);
        if casts.is_empty() {
            return Ok(());
        }
        let sig = self.function_signature(f);
        if !cascade {
            let lines = casts
                .iter()
                .map(|(_, c)| format!("{c} depends on function {sig}"))
                .collect();
            return Err(Self::dependents_error_lines(
                &format!("function {sig}"),
                lines,
            ));
        }
        let descs: Vec<String> = casts.iter().map(|(_, c)| c.clone()).collect();
        self.cascade_notice(&descs);
        for (key, _) in &casts {
            self.drop_cast_key(key)?;
        }
        Ok(())
    }

    fn dependents_error_lines(what: &str, lines: Vec<String>) -> pgwire::error::PgWireError {
        let mut info = pgwire::error::ErrorInfo::new(
            "ERROR".into(),
            "2BP01".into(), // dependent_objects_still_exist
            format!("cannot drop {what} because other objects depend on it"),
        );
        info.detail = Some(lines.join("\n"));
        info.hint = Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
        pgwire::error::PgWireError::UserError(Box::new(info))
    }
}
