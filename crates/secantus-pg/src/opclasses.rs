//! `CREATE / ALTER / DROP OPERATOR FAMILY` and `OPERATOR CLASS` (RP-169):
//! a database's own operator families, classes and their members, checked
//! as PostgreSQL 15.19's `opclasscmds.c` checks them and listed in
//! `pg_opfamily`, `pg_opclass`, `pg_amop` and `pg_amproc` after the
//! built-in rows. A member's dependency is the one `btadjustmembers` /
//! `hashadjustmembers` gives it: a "hard" member belongs to its class and
//! goes with it, a "soft" one to the family.
//!
//! Classes of the btree and hash methods are served. An index over a key
//! with a user class is kept as any index is: its order and equality are
//! the type's own, so a UNIQUE index whose class's equality is not the
//! type's default is refused by name. The GiST, GIN, SP-GiST and BRIN
//! methods' classes (whose support functions are C internals) are refused
//! by name.

use bson::{Bson, Document};
use pgwire::api::results::{Response, Tag};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgcatalog::TableDef;
use secantus_pgplan::misc_alter::{self, OpClass};
use secantus_pgplan::opclasses::{MemberKind, OpClassItem, OpClassOp};

use crate::system_catalogs::{builtin_members, builtin_opfamilies, builtin_procs};
use crate::{bson_i64, PgHandler};

pub(crate) const OPFAMILY_COLLECTION: &str = "__sql_opfamilies__";
pub(crate) const OPCLASS_COLLECTION: &str = "__sql_opclasses__";
pub(crate) const AMOP_COLLECTION: &str = "__sql_amops__";
pub(crate) const AMPROC_COLLECTION: &str = "__sql_amprocs__";

pub(crate) const PG_OPCLASS: i64 = 2616;
pub(crate) const PG_OPFAMILY: i64 = 2753;

const BTREE: i64 = 403;
const HASH: i64 = 405;
const BOOL: i64 = 16;
const INT8: i64 = 20;
const INT4: i64 = 23;
const VOID: i64 = 2278;
const INTERNAL: i64 = 2281;

/// `(amstrategies, amsupport, amoptsprocnum)` of the methods served.
fn am_limits(method: i64) -> Option<(i32, i32, i32)> {
    match method {
        BTREE => Some((5, 5, 5)),
        HASH => Some((1, 3, 3)),
        _ => None,
    }
}

fn am_name(method: i64) -> &'static str {
    match method {
        BTREE => "btree",
        HASH => "hash",
        783 => "gist",
        2742 => "gin",
        4000 => "spgist",
        3580 => "brin",
        _ => "",
    }
}

/// A family: PostgreSQL's or the database's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Family {
    pub oid: i64,
    pub method: i64,
    pub schema: String,
    pub name: String,
    pub owner: String,
    pub builtin: bool,
}

/// A `pg_amop` (operator) or `pg_amproc` (function) row the database added.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Member {
    pub oid: i64,
    pub family: i64,
    pub left: i64,
    pub right: i64,
    pub number: i64,
    /// The operator's or function's oid.
    pub object: i64,
    pub sortfamily: i64,
    /// The class a hard member belongs to; 0 for a soft member of the
    /// family.
    pub class: i64,
}

impl Member {
    fn of(d: &Document) -> Member {
        let i = |k: &str| d.get(k).and_then(bson_i64).unwrap_or(0);
        Member {
            oid: i("oid"),
            family: i("family"),
            left: i("left"),
            right: i("right"),
            number: i("number"),
            object: i("object"),
            sortfamily: i("sortfamily"),
            class: i("class"),
        }
    }

    fn doc(&self) -> Document {
        bson::doc! {
            "_id": self.oid.to_string(),
            "oid": self.oid,
            "family": self.family,
            "left": self.left,
            "right": self.right,
            "number": self.number,
            "object": self.object,
            "sortfamily": self.sortfamily,
            "class": self.class,
        }
    }
}

/// An item resolved, before it is stored (`OpFamilyMember`).
#[derive(Debug, Clone)]
struct Pending {
    is_func: bool,
    number: i64,
    object: i64,
    left: i64,
    right: i64,
    sortfamily: i64,
    class: i64,
}

/// A function as `opclasscmds.c` and `CreateTransform` look at it.
#[derive(Debug, Clone)]
pub(crate) struct Proc {
    pub oid: i64,
    pub args: Vec<i64>,
    pub ret: i64,
    pub kind: char,
    pub retset: bool,
    pub volatile: char,
}

fn class_of_doc(d: &Document) -> OpClass {
    let i = |k: &str| d.get(k).and_then(bson_i64).unwrap_or(0);
    OpClass {
        oid: i("oid"),
        method: i("method"),
        schema: d.get_str("schema").unwrap_or("public").to_string(),
        name: d.get_str("name").unwrap_or_default().to_string(),
        family: i("family"),
        intype: i("intype"),
        default: d.get_bool("default").unwrap_or(false),
    }
}

fn family_of_doc(d: &Document) -> Family {
    Family {
        oid: d.get("oid").and_then(bson_i64).unwrap_or(0),
        method: d.get("method").and_then(bson_i64).unwrap_or(0),
        schema: d.get_str("schema").unwrap_or("public").to_string(),
        name: d.get_str("name").unwrap_or_default().to_string(),
        owner: d.get_str("owner").unwrap_or_default().to_string(),
        builtin: false,
    }
}

fn key(method: i64, schema: &str, name: &str) -> String {
    format!("{method}/{schema}.{name}")
}

fn invalid(m: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "42P17".into(), // invalid_object_definition
        m.into(),
    )))
}

impl PgHandler {
    fn opfamily_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(OPFAMILY_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    fn opclass_docs(&self) -> Vec<Document> {
        self.type_catalog_docs(OPCLASS_COLLECTION)
            .map(|d| d.to_vec())
            .unwrap_or_default()
    }

    /// The database's operators (`is_func` false) or support functions.
    pub(crate) fn opclass_members(&self, is_func: bool) -> Vec<Member> {
        self.type_catalog_docs(if is_func {
            AMPROC_COLLECTION
        } else {
            AMOP_COLLECTION
        })
        .map(|d| d.iter().map(Member::of).collect())
        .unwrap_or_default()
    }

    /// The database's classes, as the planner reads them.
    pub(crate) fn user_opclasses(&self) -> Vec<OpClass> {
        self.opclass_docs().iter().map(class_of_doc).collect()
    }

    fn publish_opclasses(&self) {
        misc_alter::set_user_opclasses(self.user_opclasses());
    }

    /// Every family: PostgreSQL's, then the database's.
    pub(crate) fn opfamilies(&self) -> Vec<Family> {
        let mut out: Vec<Family> = builtin_opfamilies()
            .iter()
            .map(|(oid, method, name)| Family {
                oid: *oid,
                method: *method,
                schema: "pg_catalog".into(),
                name: name.clone(),
                owner: "postgres".into(),
                builtin: true,
            })
            .collect();
        out.extend(self.opfamily_docs().iter().map(family_of_doc));
        out
    }

    fn family_by_oid(&self, oid: i64) -> Option<Family> {
        self.opfamilies().into_iter().find(|f| f.oid == oid)
    }

    /// `OpFamilyCacheLookup`: `[schema.]name` of `method`.
    fn find_family(&self, method: i64, parts: &[String]) -> Option<Family> {
        let all = self.opfamilies();
        let of = |schema: &str, name: &str| {
            all.iter()
                .find(|f| f.method == method && f.schema == schema && f.name == name)
                .cloned()
        };
        match parts {
            [schema, name] => of(schema, name),
            [name] => std::iter::once("pg_catalog".to_string())
                .chain(secantus_pgplan::schemas::search_path())
                .find_map(|s| of(&s, name)),
            _ => None,
        }
    }

    fn family_visible(&self, f: &Family) -> bool {
        self.find_family(f.method, std::slice::from_ref(&f.name))
            .is_some_and(|v| v.oid == f.oid)
    }

    /// `getObjectDescription` of a family.
    pub(crate) fn family_description(&self, f: &Family) -> String {
        format!(
            "operator family {} for access method {}",
            self.qualified(self.family_visible(f), &f.schema, &f.name),
            am_name(f.method)
        )
    }

    /// `getObjectDescription` of a class.
    pub(crate) fn class_description(&self, c: &OpClass) -> String {
        format!(
            "operator class {} for access method {}",
            self.qualified(misc_alter::opclass_visible(c), &c.schema, &c.name),
            am_name(c.method)
        )
    }

    fn qualified(&self, visible: bool, schema: &str, name: &str) -> String {
        let q = secantus_pgplan::scalar::quote_identifier;
        if visible {
            q(name)
        } else {
            format!("{}.{}", q(schema), q(name))
        }
    }

    /// `getObjectDescription` of a member.
    fn member_description(&self, m: &Member, is_func: bool) -> String {
        let family = self
            .family_by_oid(m.family)
            .map(|f| self.family_description(&f))
            .unwrap_or_default();
        let t = secantus_pgplan::regtype_text;
        let object = if is_func {
            secantus_pgplan::regobj::text("regprocedure", m.object)
        } else {
            secantus_pgplan::regobj::text("regoperator", m.object)
        };
        format!(
            "{} {} ({}, {}) of {family}: {object}",
            if is_func { "function" } else { "operator" },
            m.number,
            t(m.left),
            t(m.right)
        )
    }

    /// A type's oid, pseudo-types (`internal`, `anyenum`) included.
    pub(crate) fn any_type_oid(&self, name: &str) -> Option<i64> {
        self.type_oid_by_name(name).or_else(|| {
            secantus_pgplan::pgtypes::PSEUDO_TYPES
                .iter()
                .find(|(_, n)| *n == name)
                .map(|(o, _)| *o)
        })
    }

    fn type_or_missing(&self, name: &str, written: &str) -> PgWireResult<i64> {
        self.any_type_oid(name)
            .ok_or_else(|| Self::user_error("42704", format!("type \"{written}\" does not exist")))
    }

    /// `LookupOperWithArgs` / `LookupOperName`: the operator `name` over
    /// exactly `(left, right)`: `(oid, result, binary)`.
    fn lookup_operator(&self, name: &[String], left: i64, right: i64) -> PgWireResult<(i64, i64)> {
        let bare = name.last().cloned().unwrap_or_default();
        if left != 0 {
            if let Some(found) = secantus_pgplan::regobj::builtin_operator(&bare, left, right) {
                return Ok(found);
            }
        }
        let user = self.catalog_docs(crate::catalog_objects::OPERATOR_COLLECTION);
        for d in user.iter() {
            if d.get_str("name") != Ok(bare.as_str()) {
                continue;
            }
            let l = d
                .get_str("left")
                .ok()
                .and_then(|t| self.type_oid_by_name(t))
                .unwrap_or(0);
            let r = d
                .get_str("right")
                .ok()
                .and_then(|t| self.type_oid_by_name(t))
                .unwrap_or(0);
            if l == left && r == right {
                let result = d
                    .get_str("result")
                    .ok()
                    .and_then(|t| self.type_oid_by_name(t))
                    .unwrap_or(0);
                let id = d.get_str("_id").unwrap_or_default();
                return Ok((Self::index_oid(&format!("opr:{id}")), result));
            }
        }
        let t = secantus_pgplan::regtype_text;
        let shown = if left == 0 {
            format!("{} {}", name.join("."), t(right))
        } else {
            format!("{} {} {}", t(left), name.join("."), t(right))
        };
        Err(Self::user_error(
            "42883",
            format!("operator does not exist: {shown}"),
        ))
    }

    /// `LookupFuncWithArgs`: the function `name(args)`, or the one so named
    /// when `args` is `None`.
    pub(crate) fn lookup_proc(&self, name: &[String], args: Option<&[i64]>) -> PgWireResult<Proc> {
        let bare = name.last().cloned().unwrap_or_default();
        let schema = (name.len() > 1).then(|| name[name.len() - 2].clone());
        let mut found: Vec<Proc> = Vec::new();
        if schema.as_deref().is_none_or(|s| s == "pg_catalog") {
            for p in builtin_procs().iter().filter(|p| p.name == bare) {
                if args.is_none_or(|a| a == p.args.as_slice()) {
                    found.push(Proc {
                        oid: p.oid,
                        args: p.args.clone(),
                        ret: p.ret,
                        kind: p.kind,
                        retset: p.retset,
                        volatile: p.volatile,
                    });
                }
            }
        }
        if found.is_empty() || args.is_none() {
            for d in self.user_function_docs().unwrap_or_default() {
                let u = crate::user_fn_of(&d);
                if u.name != bare || schema.as_deref().is_some_and(|s| s != u.schema) {
                    continue;
                }
                let types: Vec<i64> = u
                    .arg_types
                    .iter()
                    .map(|t| self.any_type_oid(t).unwrap_or(0))
                    .collect();
                if args.is_some_and(|a| a != types.as_slice()) {
                    continue;
                }
                let volatile = match d.get_str("volatility").unwrap_or("volatile") {
                    "immutable" | "i" => 'i',
                    "stable" | "s" => 's',
                    _ => 'v',
                };
                found.push(Proc {
                    oid: Self::index_oid(&format!("fn:{}", u.key)),
                    args: types,
                    ret: self.any_type_oid(&u.return_type).unwrap_or(0),
                    kind: if d.get_bool("is_procedure").unwrap_or(false) {
                        'p'
                    } else {
                        'f'
                    },
                    retset: u.returns_set,
                    volatile,
                });
            }
        }
        match (found.len(), args) {
            (0, Some(a)) => Err(Self::user_error(
                "42883",
                format!(
                    "function {}({}) does not exist",
                    name.join("."),
                    a.iter()
                        .map(|t| secantus_pgplan::regtype_text(*t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
            (0, None) => Err(Self::user_error(
                "42883",
                format!("could not find a function named \"{}\"", name.join(".")),
            )),
            (1, _) | (_, Some(_)) => Ok(found.remove(0)),
            _ => {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "42725".into(),
                    format!("function name \"{}\" is not unique", name.join(".")),
                );
                info.hint =
                    Some("Specify the argument list to select the function unambiguously.".into());
                Err(PgWireError::UserError(Box::new(info)))
            }
        }
    }

    fn arg_oids(&self, args: &[Option<String>]) -> PgWireResult<Vec<i64>> {
        args.iter()
            .map(|a| match a {
                None => Ok(0),
                Some(t) => self.type_or_missing(t, &secantus_pgplan::display_type(t)),
            })
            .collect()
    }

    /// `processTypesSpec`.
    fn types_spec(&self, args: &[String]) -> PgWireResult<(i64, i64)> {
        let left = self.type_or_missing(&args[0], &args[0])?;
        let right = match args.get(1) {
            Some(r) => self.type_or_missing(r, r)?,
            None => left,
        };
        if args.len() > 2 {
            return Err(Self::user_error(
                "42601",
                "one or two argument types must be specified".into(),
            ));
        }
        Ok((left, right))
    }

    /// `assignOperTypes`.
    fn assign_oper_types(
        &self,
        p: &mut Pending,
        method: i64,
        binary: bool,
        result: i64,
    ) -> PgWireResult<()> {
        if !binary {
            return Err(invalid("index operators must be binary"));
        }
        if p.sortfamily != 0 {
            return Err(invalid(format!(
                "access method \"{}\" does not support ordering operators",
                am_name(method)
            )));
        }
        if result != BOOL {
            return Err(invalid("index search operators must return boolean"));
        }
        Ok(())
    }

    /// `assignProcTypes`.
    fn assign_proc_types(
        &self,
        p: &mut Pending,
        proc: &Proc,
        method: i64,
        typeoid: i64,
        opts_proc: i64,
    ) -> PgWireResult<()> {
        let nargs = proc.args.len();
        let arg = |i: usize| proc.args.get(i).copied().unwrap_or(0);
        if p.number == opts_proc {
            if typeoid != 0 {
                if (p.left != 0 && p.left != typeoid) || (p.right != 0 && p.right != typeoid) {
                    return Err(invalid(
                        "associated data types for operator class options parsing functions \
                         must match opclass input type",
                    ));
                }
            } else if p.left != p.right {
                return Err(invalid(
                    "left and right associated data types for operator class options parsing \
                     functions must match",
                ));
            }
            if proc.ret != VOID || nargs != 1 || arg(0) != INTERNAL {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "42P17".into(),
                    "invalid operator class options parsing function".into(),
                );
                info.hint = Some(
                    "Valid signature of operator class options parsing function is (internal) \
                     RETURNS void."
                        .into(),
                );
                return Err(PgWireError::UserError(Box::new(info)));
            }
        } else if method == BTREE {
            match p.number {
                1 => {
                    if nargs != 2 {
                        return Err(invalid(
                            "btree comparison functions must have two arguments",
                        ));
                    }
                    if proc.ret != INT4 {
                        return Err(invalid("btree comparison functions must return integer"));
                    }
                    if p.left == 0 {
                        p.left = arg(0);
                    }
                    if p.right == 0 {
                        p.right = arg(1);
                    }
                }
                2 => {
                    if nargs != 1 || arg(0) != INTERNAL {
                        return Err(invalid(
                            "btree sort support functions must accept type \"internal\"",
                        ));
                    }
                    if proc.ret != VOID {
                        return Err(invalid("btree sort support functions must return void"));
                    }
                }
                3 => {
                    if nargs != 5 {
                        return Err(invalid("btree in_range functions must have five arguments"));
                    }
                    if proc.ret != BOOL {
                        return Err(invalid("btree in_range functions must return boolean"));
                    }
                    if p.left == 0 {
                        p.left = arg(0);
                    }
                    if p.right == 0 {
                        p.right = arg(2);
                    }
                }
                4 => {
                    if nargs != 1 {
                        return Err(invalid(
                            "btree equal image functions must have one argument",
                        ));
                    }
                    if proc.ret != BOOL {
                        return Err(invalid("btree equal image functions must return boolean"));
                    }
                    if p.left != p.right {
                        return Err(invalid(
                            "btree equal image functions must not be cross-type",
                        ));
                    }
                }
                _ => {}
            }
        } else if method == HASH {
            match p.number {
                1 => {
                    if nargs != 1 {
                        return Err(invalid("hash function 1 must have one argument"));
                    }
                    if proc.ret != INT4 {
                        return Err(invalid("hash function 1 must return integer"));
                    }
                }
                2 => {
                    if nargs != 2 {
                        return Err(invalid("hash function 2 must have two arguments"));
                    }
                    if proc.ret != INT8 {
                        return Err(invalid("hash function 2 must return bigint"));
                    }
                }
                _ => {}
            }
            if p.left == 0 {
                p.left = arg(0);
            }
            if p.right == 0 {
                p.right = arg(0);
            }
        }
        if p.left == 0 {
            p.left = typeoid;
        }
        if p.right == 0 {
            p.right = typeoid;
        }
        if p.left == 0 || p.right == 0 {
            return Err(invalid(
                "associated data types must be specified for index support function",
            ));
        }
        Ok(())
    }

    /// `addFamilyMember`.
    fn add_member(list: &mut Vec<Pending>, p: Pending) -> PgWireResult<()> {
        if list
            .iter()
            .any(|o| o.number == p.number && o.left == p.left && o.right == p.right)
        {
            let t = secantus_pgplan::regtype_text;
            return Err(invalid(format!(
                "{} number {} for ({},{}) appears more than once",
                if p.is_func { "function" } else { "operator" },
                p.number,
                t(p.left),
                t(p.right)
            )));
        }
        list.push(p);
        Ok(())
    }

    /// The method a statement names: 42704 for none, refused by name for a
    /// method whose classes this server does not keep.
    fn opclass_method(&self, name: &str) -> PgWireResult<(i64, (i32, i32, i32))> {
        let Some(oid) = misc_alter::method_oid(name) else {
            return Err(Self::user_error(
                "42704",
                format!("access method \"{name}\" does not exist"),
            ));
        };
        match am_limits(oid) {
            Some(l) => Ok((oid, l)),
            None => Err(Self::user_error(
                "0A000",
                format!("operator classes of access method \"{name}\" are not supported"),
            )),
        }
    }

    fn require_superuser(&self, what: &str) -> PgWireResult<()> {
        if self.is_superuser(&self.current_role_name()) {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!("must be superuser to {what}"),
        ))
    }

    /// The items of a CREATE or ALTER ... ADD, resolved and checked.
    fn resolve_items(
        &self,
        items: &[OpClassItem],
        method: i64,
        limits: (i32, i32, i32),
        typeoid: i64,
        alter: bool,
    ) -> PgWireResult<(Vec<Pending>, Vec<Pending>, i64)> {
        let (max_op, max_proc, opts_proc) = limits;
        let mut operators = Vec::new();
        let mut procedures = Vec::new();
        let mut storage = 0;
        for item in items {
            match item.kind {
                MemberKind::Operator => {
                    if item.number <= 0 || item.number > max_op {
                        return Err(invalid(format!(
                            "invalid operator number {}, must be between 1 and {max_op}",
                            item.number
                        )));
                    }
                    let (oid, result, binary) = match &item.args {
                        Some(args) => {
                            let a = self.arg_oids(args)?;
                            let (l, r) = match a.as_slice() {
                                [l, r] => (*l, *r),
                                [r] => (0, *r),
                                _ => (0, 0),
                            };
                            let (o, res) = self.lookup_operator(&item.name, l, r)?;
                            (o, res, l != 0)
                        }
                        None if alter => {
                            return Err(Self::user_error(
                                "42601",
                                "operator argument types must be specified in ALTER OPERATOR \
                                 FAMILY"
                                    .into(),
                            ))
                        }
                        None => {
                            let (o, res) = self.lookup_operator(&item.name, typeoid, typeoid)?;
                            (o, res, true)
                        }
                    };
                    let sortfamily = match &item.order_family {
                        Some(f) => {
                            self.find_family(BTREE, f)
                                .ok_or_else(|| {
                                    Self::user_error(
                                        "42704",
                                        format!(
                                            "operator family \"{}\" does not exist for access \
                                             method \"btree\"",
                                            f.join(".")
                                        ),
                                    )
                                })?
                                .oid
                        }
                        None => 0,
                    };
                    let mut p = Pending {
                        is_func: false,
                        number: i64::from(item.number),
                        object: oid,
                        left: 0,
                        right: 0,
                        sortfamily,
                        class: 0,
                    };
                    self.assign_oper_types(&mut p, method, binary, result)?;
                    let (l, r) = match &item.args {
                        Some(args) => {
                            let a = self.arg_oids(args)?;
                            (
                                a.first().copied().unwrap_or(0),
                                a.get(1).copied().unwrap_or(0),
                            )
                        }
                        None => (typeoid, typeoid),
                    };
                    p.left = l;
                    p.right = r;
                    Self::add_member(&mut operators, p)?;
                }
                MemberKind::Function => {
                    if item.number <= 0 || item.number > max_proc {
                        return Err(invalid(format!(
                            "invalid function number {}, must be between 1 and {max_proc}",
                            item.number
                        )));
                    }
                    let args = match &item.args {
                        Some(a) => Some(self.arg_oids(a)?),
                        None => None,
                    };
                    let proc = self.lookup_proc(&item.name, args.as_deref())?;
                    let mut p = Pending {
                        is_func: true,
                        number: i64::from(item.number),
                        object: proc.oid,
                        left: 0,
                        right: 0,
                        sortfamily: 0,
                        class: 0,
                    };
                    if !item.class_args.is_empty() {
                        (p.left, p.right) = self.types_spec(&item.class_args)?;
                    }
                    self.assign_proc_types(&mut p, &proc, method, typeoid, i64::from(opts_proc))?;
                    Self::add_member(&mut procedures, p)?;
                }
                MemberKind::Storage => {
                    if alter {
                        return Err(Self::user_error(
                            "42601",
                            "STORAGE cannot be specified in ALTER OPERATOR FAMILY".into(),
                        ));
                    }
                    if storage != 0 {
                        return Err(invalid("storage type specified more than once"));
                    }
                    let t = item.storage.clone().unwrap_or_default();
                    storage = self.type_or_missing(&t, &t)?;
                }
            }
        }
        Ok((operators, procedures, storage))
    }

    /// `opclass_for_family_datatype`.
    fn class_for_family_type(&self, family: i64, intype: i64) -> i64 {
        misc_alter::all_opclasses()
            .iter()
            .find(|c| c.family == family && c.intype == intype)
            .map_or(0, |c| c.oid)
    }

    /// `btadjustmembers` / `hashadjustmembers`: which members are hard.
    fn adjust_members(&self, family: i64, class: i64, intype: i64, members: &mut [Pending]) {
        let standard = 1;
        let mut cached = (intype, class);
        for m in members.iter_mut() {
            if (m.is_func && m.number != standard) || m.left != m.right {
                m.class = 0;
                continue;
            }
            if m.left != cached.0 {
                cached = (m.left, self.class_for_family_type(family, m.left));
            }
            m.class = cached.1;
        }
    }

    /// Does the family hold a member `(left, right, number)`?
    fn family_has(&self, family: i64, is_func: bool, left: i64, right: i64, number: i64) -> bool {
        builtin_members(is_func).contains(&(family, left, right, number))
            || self.opclass_members(is_func).iter().any(|m| {
                m.family == family && m.left == left && m.right == right && m.number == number
            })
    }

    /// `storeOperators` / `storeProcedures`.
    fn store_members(
        &self,
        family: &Family,
        written: &[String],
        members: &[Pending],
        add: bool,
    ) -> PgWireResult<()> {
        let t = secantus_pgplan::regtype_text;
        for m in members {
            if self.family_has(family.oid, m.is_func, m.left, m.right, m.number) {
                if add {
                    return Err(Self::user_error(
                        "42710",
                        format!(
                            "{} {}({},{}) already exists in operator family \"{}\"",
                            if m.is_func { "function" } else { "operator" },
                            m.number,
                            t(m.left),
                            t(m.right),
                            written.join(".")
                        ),
                    ));
                }
                // CREATE OPERATOR CLASS does not check: the catalog's
                // unique index refuses the row.
                let (index, cols) = if m.is_func {
                    (
                        "pg_amproc_fam_proc_index",
                        "amprocfamily, amproclefttype, amprocrighttype, amprocnum",
                    )
                } else {
                    (
                        "pg_amop_fam_strat_index",
                        "amopfamily, amoplefttype, amoprighttype, amopstrategy",
                    )
                };
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "23505".into(),
                    format!("duplicate key value violates unique constraint \"{index}\""),
                );
                info.detail = Some(format!(
                    "Key ({cols})=({}, {}, {}, {}) already exists.",
                    family.oid, m.left, m.right, m.number
                ));
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let kind = if m.is_func { "amproc" } else { "amop" };
            let member = Member {
                oid: Self::index_oid(&format!(
                    "{kind}:{}:{}:{}:{}",
                    family.oid, m.left, m.right, m.number
                )),
                family: family.oid,
                left: m.left,
                right: m.right,
                number: m.number,
                object: m.object,
                sortfamily: m.sortfamily,
                class: m.class,
            };
            let collection = if m.is_func {
                AMPROC_COLLECTION
            } else {
                AMOP_COLLECTION
            };
            self.put(collection, &member.oid.to_string(), member.doc())?;
        }
        Ok(())
    }

    fn create_family(&self, method: i64, schema: &str, name: &str) -> PgWireResult<Family> {
        let id = key(method, schema, name);
        let family = Family {
            oid: Self::index_oid(&format!("opf:{id}")),
            method,
            schema: schema.to_string(),
            name: name.to_string(),
            owner: self.current_role_name(),
            builtin: false,
        };
        self.put(
            OPFAMILY_COLLECTION,
            &id,
            bson::doc! {
                "_id": &id,
                "oid": family.oid,
                "method": method,
                "schema": schema,
                "name": name,
                "owner": &family.owner,
            },
        )?;
        Ok(family)
    }

    /// `[schema.]name` for a CREATE: the schema it goes in, and the name.
    fn creation_name(&self, parts: &[String]) -> PgWireResult<(String, String)> {
        match parts {
            [schema, name] => {
                self.require_schema(schema)?;
                Ok((schema.clone(), name.clone()))
            }
            [name] => Ok((
                secantus_pgplan::schemas::creation_schema()
                    .map_err(|e| Self::user_error("3F000", e.to_string()))?,
                name.clone(),
            )),
            _ => Err(Self::user_error(
                "42601",
                format!(
                    "improper qualified name (too many dotted names): {}",
                    parts.join(".")
                ),
            )),
        }
    }

    fn family_exists_error(&self, f_name: &str, method: i64, schema: Option<&str>) -> PgWireError {
        Self::user_error(
            "42710",
            match schema {
                None => format!(
                    "operator family \"{f_name}\" for access method \"{}\" already exists",
                    am_name(method)
                ),
                Some(s) => format!(
                    "operator family \"{f_name}\" for access method \"{}\" already exists in \
                     schema \"{s}\"",
                    am_name(method)
                ),
            },
        )
    }

    fn class_exists_error(&self, c_name: &str, method: i64, schema: Option<&str>) -> PgWireError {
        Self::user_error(
            "42710",
            match schema {
                None => format!(
                    "operator class \"{c_name}\" for access method \"{}\" already exists",
                    am_name(method)
                ),
                Some(s) => format!(
                    "operator class \"{c_name}\" for access method \"{}\" already exists in \
                     schema \"{s}\"",
                    am_name(method)
                ),
            },
        )
    }

    fn missing_family(parts: &[String], method: &str) -> PgWireError {
        Self::user_error(
            "42704",
            format!(
                "operator family \"{}\" does not exist for access method \"{method}\"",
                parts.join(".")
            ),
        )
    }

    fn missing_class(parts: &[String], method: &str) -> PgWireError {
        Self::user_error(
            "42704",
            format!(
                "operator class \"{}\" does not exist for access method \"{method}\"",
                parts.join(".")
            ),
        )
    }

    /// The indexes over a key with each class: `(index, table, class)`.
    pub(crate) fn indexes_by_class(&self) -> Vec<(String, String, i64)> {
        let mut out = Vec::new();
        for def in self.all_table_defs().unwrap_or_default() {
            for ix in self
                .storage
                .list_indexes(self.db(), &def.name)
                .unwrap_or_default()
            {
                let Ok(classes) = ix.get_array("sqlOpclasses") else {
                    continue;
                };
                let name = ix.get_str("name").unwrap_or_default().to_string();
                for c in classes.iter().filter_map(bson_i64).filter(|c| *c != 0) {
                    out.push((name.clone(), def.name.clone(), c));
                }
            }
        }
        out
    }

    /// The objects that go with `classes` (the indexes over them), as
    /// `findDependentObjects` lists them: `(index key, description, class)`.
    fn class_dependents(&self, classes: &[OpClass]) -> Vec<(String, String, OpClass)> {
        let mut out = Vec::new();
        for (index, table, class) in self.indexes_by_class() {
            if let Some(c) = classes.iter().find(|c| c.oid == class) {
                let (schema, _) = secantus_pgplan::schemas::split_key(&table);
                let key = if schema == "public" {
                    index.clone()
                } else {
                    format!("{schema}.{index}")
                };
                if !out
                    .iter()
                    .any(|(k, _, _): &(String, String, OpClass)| *k == key)
                {
                    out.push((key, format!("index {index}"), c.clone()));
                }
            }
        }
        out
    }

    /// Drop `classes` and, with `cascade`, the indexes over them; refuse
    /// with 2BP01 naming `what` otherwise.
    fn drop_classes(&self, what: &str, classes: &[OpClass], cascade: bool) -> PgWireResult<()> {
        let dependents = self.class_dependents(classes);
        if !dependents.is_empty() {
            if !cascade {
                let lines: Vec<String> = dependents
                    .iter()
                    .map(|(_, d, c)| format!("{d} depends on {}", self.class_description(c)))
                    .collect();
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "2BP01".into(),
                    format!("cannot drop {what} because other objects depend on it"),
                );
                info.detail = Some(lines.join("\n"));
                info.hint = Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let descs: Vec<String> = dependents.iter().map(|(_, d, _)| d.clone()).collect();
            self.cascade_notice(&descs);
            for (index, _, _) in &dependents {
                self.execute_inner_keys(
                    secantus_pgplan::Statement::DropIndex {
                        names: vec![index.clone()],
                        if_exists: true,
                    },
                    0,
                )?;
            }
        }
        for c in classes {
            for is_func in [false, true] {
                for m in self
                    .opclass_members(is_func)
                    .iter()
                    .filter(|m| m.class == c.oid)
                {
                    self.delete_type_doc(
                        if is_func {
                            AMPROC_COLLECTION
                        } else {
                            AMOP_COLLECTION
                        },
                        &m.oid.to_string(),
                    )?;
                }
            }
            self.delete_type_doc(OPCLASS_COLLECTION, &key(c.method, &c.schema, &c.name))?;
        }
        Ok(())
    }

    /// `DROP FUNCTION` / `DROP OPERATOR`: the members using `object`. A
    /// soft member goes silently (`DEPENDENCY_AUTO`); a hard one makes its
    /// class a dependent of the function.
    pub(crate) fn drop_opclass_members_of(
        &self,
        is_func: bool,
        object: i64,
        what: &str,
        cascade: bool,
    ) -> PgWireResult<()> {
        let members: Vec<Member> = self
            .opclass_members(is_func)
            .into_iter()
            .filter(|m| m.object == object)
            .collect();
        let classes: Vec<OpClass> = self
            .user_opclasses()
            .into_iter()
            .filter(|c| members.iter().any(|m| m.class == c.oid))
            .collect();
        if !classes.is_empty() {
            if !cascade {
                let mut info = ErrorInfo::new(
                    "ERROR".into(),
                    "2BP01".into(),
                    format!("cannot drop {what} because other objects depend on it"),
                );
                info.detail = Some(
                    classes
                        .iter()
                        .map(|c| format!("{} depends on {what}", self.class_description(c)))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                info.hint = Some("Use DROP ... CASCADE to drop the dependent objects too.".into());
                return Err(PgWireError::UserError(Box::new(info)));
            }
            let mut descs: Vec<String> =
                classes.iter().map(|c| self.class_description(c)).collect();
            descs.extend(
                self.class_dependents(&classes)
                    .into_iter()
                    .map(|(_, d, _)| d),
            );
            self.cascade_notice(&descs);
            let indexes = self.class_dependents(&classes);
            for (index, _, _) in &indexes {
                self.execute_inner_keys(
                    secantus_pgplan::Statement::DropIndex {
                        names: vec![index.clone()],
                        if_exists: true,
                    },
                    0,
                )?;
            }
            self.drop_classes(what, &classes, true)?;
        }
        for m in members.iter().filter(|m| m.class == 0) {
            self.delete_type_doc(
                if is_func {
                    AMPROC_COLLECTION
                } else {
                    AMOP_COLLECTION
                },
                &m.oid.to_string(),
            )?;
        }
        self.publish_opclasses();
        Ok(())
    }

    /// CREATE INDEX over keys with these classes: each class's family must
    /// hold support function 1 for its type, and a UNIQUE index's equality
    /// must be the type's own.
    pub(crate) fn check_index_classes(
        &self,
        index: &str,
        classes: &[i64],
        unique: bool,
    ) -> PgWireResult<()> {
        let all = misc_alter::all_opclasses();
        for (i, oid) in classes.iter().enumerate() {
            if *oid == 0 || *oid < 16384 {
                continue;
            }
            let Some(c) = all.iter().find(|c| c.oid == *oid) else {
                continue;
            };
            if !self.family_has(c.family, true, c.intype, c.intype, 1) {
                return Err(Self::user_error(
                    "XX000",
                    format!(
                        "missing support function 1 for attribute {} of index \"{index}\"",
                        i + 1
                    ),
                ));
            }
            if unique {
                let strategy = if c.method == BTREE { 3 } else { 1 };
                let own = secantus_pgplan::regobj::builtin_operator("=", c.intype, c.intype)
                    .map(|(o, _)| o);
                let equality = self.opclass_members(false).into_iter().find(|m| {
                    m.family == c.family
                        && m.left == c.intype
                        && m.right == c.intype
                        && m.number == strategy
                });
                if equality.is_none_or(|m| Some(m.object) != own) {
                    return Err(Self::user_error(
                        "0A000",
                        format!(
                            "a unique index over operator class \"{}\" is not supported: its \
                             equality is not the type's own",
                            c.name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn owned_or_superuser(&self, owner: &str, what: &str) -> PgWireResult<()> {
        let role = self.current_role_name();
        if owner == role || self.is_superuser(&role) {
            return Ok(());
        }
        Err(Self::user_error(
            "42501",
            format!("must be owner of {what}"),
        ))
    }

    fn found_class(&self, method: &str, parts: &[String]) -> PgWireResult<OpClass> {
        misc_alter::find_opclass(method, parts).ok_or_else(|| Self::missing_class(parts, method))
    }

    fn class_doc(&self, c: &OpClass) -> Option<Document> {
        self.opclass_docs()
            .into_iter()
            .find(|d| d.get("oid").and_then(bson_i64) == Some(c.oid))
    }

    fn family_doc(&self, f: &Family) -> Option<Document> {
        self.opfamily_docs()
            .into_iter()
            .find(|d| d.get("oid").and_then(bson_i64) == Some(f.oid))
    }

    fn pinned(what: &str) -> PgWireError {
        Self::user_error(
            "2BP01",
            format!("cannot drop {what} because it is required by the database system"),
        )
    }

    pub(crate) fn opclass_statement(&self, op: OpClassOp) -> PgWireResult<Vec<Response>> {
        let tag = op.tag();
        match op {
            OpClassOp::CreateFamily { name, method } => {
                let (schema, bare) = self.creation_name(&name)?;
                let Some(m) = misc_alter::method_oid(&method) else {
                    return Err(Self::user_error(
                        "42704",
                        format!("access method \"{method}\" does not exist"),
                    ));
                };
                self.require_superuser("create an operator family")?;
                if am_limits(m).is_none() {
                    self.opclass_method(&method)?;
                }
                if self
                    .opfamilies()
                    .iter()
                    .any(|f| f.method == m && f.schema == schema && f.name == bare)
                {
                    return Err(self.family_exists_error(&bare, m, None));
                }
                self.create_family(m, &schema, &bare)?;
            }
            OpClassOp::CreateClass {
                name,
                method,
                datatype,
                datatype_written,
                family,
                default,
                items,
            } => {
                let (schema, bare) = self.creation_name(&name)?;
                if misc_alter::method_oid(&method).is_none() {
                    return Err(Self::user_error(
                        "42704",
                        format!("access method \"{method}\" does not exist"),
                    ));
                }
                self.require_superuser("create an operator class")?;
                let (m, limits) = self.opclass_method(&method)?;
                let typeoid = self.type_or_missing(&datatype, &datatype_written)?;
                let existing_family = match &family {
                    Some(parts) => Some(
                        self.find_family(m, parts)
                            .ok_or_else(|| Self::missing_family(parts, &method))?,
                    ),
                    None => self
                        .opfamilies()
                        .into_iter()
                        .find(|f| f.method == m && f.schema == schema && f.name == bare),
                };
                let (mut operators, mut procedures, storage) =
                    self.resolve_items(&items, m, limits, typeoid, false)?;
                let storage = if storage == typeoid { 0 } else { storage };
                if storage != 0 {
                    return Err(invalid(format!(
                        "storage type cannot be different from data type for access method \
                         \"{method}\""
                    )));
                }
                let all = misc_alter::all_opclasses();
                if all
                    .iter()
                    .any(|c| c.method == m && c.schema == schema && c.name == bare)
                {
                    return Err(self.class_exists_error(&bare, m, None));
                }
                if default {
                    if let Some(d) = all
                        .iter()
                        .find(|c| c.method == m && c.intype == typeoid && c.default)
                    {
                        let mut info = ErrorInfo::new(
                            "ERROR".into(),
                            "42710".into(),
                            format!(
                                "could not make operator class \"{bare}\" be default for type \
                                 {datatype_written}"
                            ),
                        );
                        info.detail = Some(format!(
                            "Operator class \"{}\" already is the default.",
                            d.name
                        ));
                        return Err(PgWireError::UserError(Box::new(info)));
                    }
                }
                let family = match existing_family {
                    Some(f) => f,
                    None => self.create_family(m, &schema, &bare)?,
                };
                let id = key(m, &schema, &bare);
                let oid = Self::index_oid(&format!("opc:{id}"));
                self.put(
                    OPCLASS_COLLECTION,
                    &id,
                    bson::doc! {
                        "_id": &id,
                        "oid": oid,
                        "method": m,
                        "schema": &schema,
                        "name": &bare,
                        "owner": self.current_role_name(),
                        "family": family.oid,
                        "intype": typeoid,
                        "default": default,
                        "keytype": storage,
                    },
                )?;
                self.publish_opclasses();
                self.adjust_members(family.oid, oid, typeoid, &mut operators);
                self.adjust_members(family.oid, oid, typeoid, &mut procedures);
                let written = vec![family.name.clone()];
                self.store_members(&family, &written, &operators, false)?;
                self.store_members(&family, &written, &procedures, false)?;
            }
            OpClassOp::AlterFamily {
                name,
                method,
                drop,
                items,
            } => {
                let (m, limits) = self.opclass_method(&method)?;
                let family = self
                    .find_family(m, &name)
                    .ok_or_else(|| Self::missing_family(&name, &method))?;
                self.require_superuser("alter an operator family")?;
                let (max_op, max_proc, _) = limits;
                if drop {
                    let mut doomed: Vec<(bool, Member)> = Vec::new();
                    for item in &items {
                        let is_func = item.kind == MemberKind::Function;
                        let max = if is_func { max_proc } else { max_op };
                        if item.number <= 0 || item.number > max {
                            return Err(invalid(format!(
                                "invalid {} number {}, must be between 1 and {max}",
                                if is_func { "function" } else { "operator" },
                                item.number
                            )));
                        }
                        let (l, r) = self.types_spec(&item.class_args)?;
                        let n = i64::from(item.number);
                        let found = self.opclass_members(is_func).into_iter().find(|x| {
                            x.family == family.oid && x.left == l && x.right == r && x.number == n
                        });
                        let Some(found) = found else {
                            let t = secantus_pgplan::regtype_text;
                            return Err(Self::user_error(
                                "42704",
                                format!(
                                    "{} {n}({},{}) does not exist in operator family \"{}\"",
                                    if is_func { "function" } else { "operator" },
                                    t(l),
                                    t(r),
                                    name.join(".")
                                ),
                            ));
                        };
                        if found.class != 0 {
                            if let Some(c) = misc_alter::all_opclasses()
                                .into_iter()
                                .find(|c| c.oid == found.class)
                            {
                                let desc = self.member_description(&found, is_func);
                                let class = self.class_description(&c);
                                let mut info = ErrorInfo::new(
                                    "ERROR".into(),
                                    "2BP01".into(),
                                    format!("cannot drop {desc} because {class} requires it"),
                                );
                                info.hint = Some(format!("You can drop {class} instead."));
                                return Err(PgWireError::UserError(Box::new(info)));
                            }
                        }
                        doomed.push((is_func, found));
                    }
                    for (is_func, d) in doomed {
                        self.delete_type_doc(
                            if is_func {
                                AMPROC_COLLECTION
                            } else {
                                AMOP_COLLECTION
                            },
                            &d.oid.to_string(),
                        )?;
                    }
                } else {
                    let (mut operators, mut procedures, _) =
                        self.resolve_items(&items, m, limits, 0, true)?;
                    self.adjust_members(family.oid, 0, 0, &mut operators);
                    self.adjust_members(family.oid, 0, 0, &mut procedures);
                    self.store_members(&family, &name, &operators, true)?;
                    self.store_members(&family, &name, &procedures, true)?;
                }
            }
            OpClassOp::Drop {
                class,
                name,
                method,
                if_exists,
                cascade,
            } => {
                let Some(m) = misc_alter::method_oid(&method) else {
                    return Err(Self::user_error(
                        "42704",
                        format!("access method \"{method}\" does not exist"),
                    ));
                };
                let noun = if class {
                    "operator class"
                } else {
                    "operator family"
                };
                if class {
                    let Some(c) = misc_alter::find_opclass(&method, &name) else {
                        if if_exists {
                            self.notice(
                                "00000",
                                format!(
                                    "{noun} \"{}\" does not exist for access method \"{method}\", \
                                     skipping",
                                    name.join(".")
                                ),
                                None,
                            );
                            return Ok(vec![Response::Execution(Tag::new(tag))]);
                        }
                        return Err(Self::missing_class(&name, &method));
                    };
                    let what = self.class_description(&c);
                    if c.oid < 16384 {
                        return Err(Self::pinned(&what));
                    }
                    self.drop_classes(&what, std::slice::from_ref(&c), cascade)?;
                } else {
                    let Some(f) = self.find_family(m, &name) else {
                        if if_exists {
                            self.notice(
                                "00000",
                                format!(
                                    "{noun} \"{}\" does not exist for access method \"{method}\", \
                                     skipping",
                                    name.join(".")
                                ),
                                None,
                            );
                            return Ok(vec![Response::Execution(Tag::new(tag))]);
                        }
                        return Err(Self::missing_family(&name, &method));
                    };
                    let what = self.family_description(&f);
                    if f.builtin {
                        return Err(Self::pinned(&what));
                    }
                    let classes: Vec<OpClass> = self
                        .user_opclasses()
                        .into_iter()
                        .filter(|c| c.family == f.oid)
                        .collect();
                    self.drop_classes(&what, &classes, cascade)?;
                    for is_func in [false, true] {
                        for x in self
                            .opclass_members(is_func)
                            .iter()
                            .filter(|x| x.family == f.oid)
                        {
                            self.delete_type_doc(
                                if is_func {
                                    AMPROC_COLLECTION
                                } else {
                                    AMOP_COLLECTION
                                },
                                &x.oid.to_string(),
                            )?;
                        }
                    }
                    self.delete_type_doc(OPFAMILY_COLLECTION, &key(f.method, &f.schema, &f.name))?;
                }
            }
            OpClassOp::Rename {
                class,
                name,
                method,
                to,
            } => {
                self.alter_named(class, &name, &method, |s, mut d, m, schema| {
                    let clash = if class {
                        misc_alter::all_opclasses()
                            .iter()
                            .any(|c| c.method == m && c.schema == schema && c.name == to)
                    } else {
                        s.opfamilies()
                            .iter()
                            .any(|f| f.method == m && f.schema == schema && f.name == to)
                    };
                    if clash {
                        return Err(if class {
                            s.class_exists_error(&to, m, Some(&schema))
                        } else {
                            s.family_exists_error(&to, m, Some(&schema))
                        });
                    }
                    d.insert("name", &to);
                    Ok(d)
                })?;
            }
            OpClassOp::Owner {
                class,
                name,
                method,
                owner,
            } => {
                let owner = self.grantee_name(&owner)?;
                if self.role(&owner)?.is_none() && owner != self.session_user_name() {
                    return Err(Self::user_error(
                        "42704",
                        format!("role \"{owner}\" does not exist"),
                    ));
                }
                self.alter_named(class, &name, &method, |_, mut d, _, _| {
                    d.insert("owner", &owner);
                    Ok(d)
                })?;
            }
            OpClassOp::SetSchema {
                class,
                name,
                method,
                schema,
            } => {
                self.require_schema(&schema)?;
                self.alter_named(class, &name, &method, |s, mut d, m, _| {
                    let bare = d.get_str("name").unwrap_or_default().to_string();
                    let clash = if class {
                        misc_alter::all_opclasses()
                            .iter()
                            .any(|c| c.method == m && c.schema == schema && c.name == bare)
                    } else {
                        s.opfamilies()
                            .iter()
                            .any(|f| f.method == m && f.schema == schema && f.name == bare)
                    };
                    if clash {
                        return Err(if class {
                            s.class_exists_error(&bare, m, Some(&schema))
                        } else {
                            s.family_exists_error(&bare, m, Some(&schema))
                        });
                    }
                    d.insert("schema", &schema);
                    Ok(d)
                })?;
            }
        }
        self.publish_opclasses();
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// Rewrite the class or family `name` of `method` that the role owns.
    fn alter_named(
        &self,
        class: bool,
        name: &[String],
        method: &str,
        change: impl FnOnce(&Self, Document, i64, String) -> PgWireResult<Document>,
    ) -> PgWireResult<()> {
        let Some(m) = misc_alter::method_oid(method) else {
            return Err(Self::user_error(
                "42704",
                format!("access method \"{method}\" does not exist"),
            ));
        };
        let (doc, collection, what) = if class {
            let c = self.found_class(method, name)?;
            let what = format!("operator class {}", name.join("."));
            (self.class_doc(&c), OPCLASS_COLLECTION, what)
        } else {
            let f = self
                .find_family(m, name)
                .ok_or_else(|| Self::missing_family(name, method))?;
            let what = format!("operator family {}", name.join("."));
            (self.family_doc(&f), OPFAMILY_COLLECTION, what)
        };
        let Some(doc) = doc else {
            return Err(Self::user_error(
                "42501",
                format!("must be owner of {what}"),
            ));
        };
        self.owned_or_superuser(doc.get_str("owner").unwrap_or_default(), &what)?;
        let schema = doc.get_str("schema").unwrap_or("public").to_string();
        let old = doc.get_str("_id").unwrap_or_default().to_string();
        let mut d = change(self, doc, m, schema)?;
        let new = key(
            m,
            d.get_str("schema").unwrap_or("public"),
            d.get_str("name").unwrap_or_default(),
        );
        d.insert("_id", &new);
        self.delete_type_doc(collection, &old)?;
        self.put(collection, &new, d)?;
        self.publish_opclasses();
        Ok(())
    }

    /// `COMMENT ON OPERATOR CLASS / FAMILY name USING method`: `names` is
    /// `[method, schema?, name]`.
    pub(crate) fn comment_on_opclass(
        &self,
        class: bool,
        names: &[String],
        comment: Option<String>,
    ) -> PgWireResult<()> {
        let Some((method, name)) = names.split_first() else {
            return Ok(());
        };
        let m = misc_alter::method_oid(method).unwrap_or(0);
        let (doc, collection) = if class {
            let c = self.found_class(method, name)?;
            (self.class_doc(&c), OPCLASS_COLLECTION)
        } else {
            let f = self
                .find_family(m, name)
                .ok_or_else(|| Self::missing_family(name, method))?;
            (self.family_doc(&f), OPFAMILY_COLLECTION)
        };
        let Some(mut d) = doc else {
            return Err(Self::user_error(
                "0A000",
                "COMMENT ON a built-in operator class or family is not supported".into(),
            ));
        };
        match comment {
            Some(c) => d.insert("comment", c),
            None => d.remove("comment"),
        };
        let id = d.get_str("_id").unwrap_or_default().to_string();
        self.put(collection, &id, d)
    }

    /// `(classoid, oid, comment)` of each class and family with one.
    pub(crate) fn opclass_comments(&self) -> Vec<(i64, i64, String)> {
        let mut out = Vec::new();
        for (classoid, docs) in [
            (PG_OPCLASS, self.opclass_docs()),
            (PG_OPFAMILY, self.opfamily_docs()),
        ] {
            for d in docs {
                if let (Some(oid), Ok(c)) = (d.get("oid").and_then(bson_i64), d.get_str("comment"))
                {
                    out.push((classoid, oid, c.to_string()));
                }
            }
        }
        out
    }

    /// The database's rows of `pg_opfamily`, `pg_opclass`, `pg_amop` or
    /// `pg_amproc`.
    pub(crate) fn user_opclass_rows(&self, name: &str, def: &TableDef) -> Vec<Document> {
        let row = |values: Vec<(&str, Bson)>| {
            let mut d = Document::new();
            for (col, v) in values {
                if let Some(c) = def.columns.iter().find(|c| c.name == col) {
                    let v = match (c.pg_type.as_str(), v) {
                        ("int2" | "int4", Bson::Int64(i)) => Bson::Int32(i as i32),
                        ("regproc", Bson::Int64(i)) => secantus_pgplan::regobj::value("regproc", i),
                        (_, v) => v,
                    };
                    if let Some(f) = def.field_of(col) {
                        d.insert(f, v);
                    }
                }
            }
            d
        };
        let role = |r: &str| Bson::Int64(self.role_oid_of(r));
        let ns = |s: &str| Bson::Int64(self.namespace_oid(s));
        match name {
            "pg_opfamily" => self
                .opfamily_docs()
                .iter()
                .map(|d| {
                    let f = family_of_doc(d);
                    row(vec![
                        ("oid", Bson::Int64(f.oid)),
                        ("opfmethod", Bson::Int64(f.method)),
                        ("opfname", Bson::String(f.name.clone())),
                        ("opfnamespace", ns(&f.schema)),
                        ("opfowner", role(&f.owner)),
                    ])
                })
                .collect(),
            "pg_opclass" => self
                .opclass_docs()
                .iter()
                .map(|d| {
                    let c = class_of_doc(d);
                    row(vec![
                        ("oid", Bson::Int64(c.oid)),
                        ("opcmethod", Bson::Int64(c.method)),
                        ("opcname", Bson::String(c.name.clone())),
                        ("opcnamespace", ns(&c.schema)),
                        ("opcowner", role(d.get_str("owner").unwrap_or_default())),
                        ("opcfamily", Bson::Int64(c.family)),
                        ("opcintype", Bson::Int64(c.intype)),
                        ("opcdefault", Bson::Boolean(c.default)),
                        (
                            "opckeytype",
                            Bson::Int64(d.get("keytype").and_then(bson_i64).unwrap_or(0)),
                        ),
                    ])
                })
                .collect(),
            "pg_amop" => {
                let methods = self.opfamilies();
                self.opclass_members(false)
                    .iter()
                    .map(|m| {
                        let method = methods
                            .iter()
                            .find(|f| f.oid == m.family)
                            .map_or(0, |f| f.method);
                        row(vec![
                            ("oid", Bson::Int64(m.oid)),
                            ("amopfamily", Bson::Int64(m.family)),
                            ("amoplefttype", Bson::Int64(m.left)),
                            ("amoprighttype", Bson::Int64(m.right)),
                            ("amopstrategy", Bson::Int64(m.number)),
                            (
                                "amoppurpose",
                                Bson::String(if m.sortfamily != 0 { "o" } else { "s" }.into()),
                            ),
                            ("amopopr", Bson::Int64(m.object)),
                            ("amopmethod", Bson::Int64(method)),
                            ("amopsortfamily", Bson::Int64(m.sortfamily)),
                        ])
                    })
                    .collect()
            }
            "pg_amproc" => self
                .opclass_members(true)
                .iter()
                .map(|m| {
                    row(vec![
                        ("oid", Bson::Int64(m.oid)),
                        ("amprocfamily", Bson::Int64(m.family)),
                        ("amproclefttype", Bson::Int64(m.left)),
                        ("amprocrighttype", Bson::Int64(m.right)),
                        ("amprocnum", Bson::Int64(m.number)),
                        ("amproc", Bson::Int64(m.object)),
                    ])
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}
