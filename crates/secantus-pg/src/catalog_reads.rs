//! The catalog reads PostgreSQL 15.19 makes for a statement (RP-225),
//! counted in `pg_stat_sys_tables` / `pg_stat_sys_indexes` as PostgreSQL
//! counts them.
//!
//! PostgreSQL reads its catalogs through per-backend caches
//! ([`crate::catcache`]), so what a statement scans depends on what the
//! backend already holds: a new session's first query on a table builds
//! the table's relation cache entry and those of its indexes, looks its
//! name up along the search path, finds its operators and their
//! operator-family memberships, and asks for each column's statistics;
//! the same query again finds all of that cached and scans nothing. This
//! module walks a statement as PostgreSQL's parser, planner and executor
//! do, in the order and under the conditions of their C sources, and asks
//! the caches for each key they would look up:
//!
//! - backend start (`InitPostgres`, with the relation cache init files in
//!   place): the database and role, the four `pg_db_role_setting` scans of
//!   `process_settings`, and `pg_db_role_setting`'s own descriptor, which no
//!   init file holds;
//! - `namespace.c`: the search path's schemas, a relation's or a type's
//!   name along it;
//! - `relcache.c`: a relation's descriptor (`RelationBuildDesc`: its
//!   `pg_class` row, its access method, its attributes; an index's
//!   `pg_index` row, operator classes (`LookupOpclassInfo`) and attribute
//!   options), its index list and extended-statistics list;
//! - `parse_func.c`, `parse_oper.c`, `parse_coerce.c`, `parse_collate.c`:
//!   function and operator resolution, coercions, and the result type of
//!   every expression node;
//! - `plancat.c`, `costsize.c`, `selfuncs.c`, `initsplan.c`: the size
//!   estimate (every column's width when the table was never measured),
//!   each restriction's statistics, an equality's operator families, a
//!   B-tree's correlation, the tablespace's costs;
//! - the executor: the permission check (`RELOID`), the result's types,
//!   and the scans a query makes of a catalog it reads directly.
//!
//! What it does not model is said where it stops; a statement shape it
//! does not walk counts what it reached.

use std::collections::{HashMap, HashSet};

use bson::Bson;
use pg_query::protobuf as pb;
use pg_query::NodeEnum as N;

use crate::catalog_facts::{
    self as cf, builtin, BOOL, BTREE_AM, HEAP_AM, INT4, INT8, NUMERIC, PG_CATALOG, PUBLIC, TEXT,
    TID, UNKNOWN,
};
use crate::catcache::{Backend, Cache, Counter, Inval, Key, Scan, K};
use crate::PgHandler;

/// A relation as the model reads it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Rel {
    pub oid: i64,
    pub name: String,
    pub nsp: i64,
    pub kind: char,
    pub catalog: bool,
    pub in_init_file: bool,
    /// `pg_attribute` rows with attnum > 0: the dropped ones too.
    pub natts: i64,
    /// The live columns: `(attnum, name, type)`.
    pub columns: Vec<(i64, String, i64)>,
    /// `relpages > 0 && reltuples >= 0`: the size estimate needs no widths.
    pub measured: bool,
    /// The columns with a `pg_statistic` row.
    pub stats: HashSet<i64>,
    pub indexes: Vec<Index>,
    /// `reltablespace`, 0 for the database's.
    pub tablespace: i64,
    pub defaults: i64,
    pub checks: i64,
    /// A view's definition: its rule.
    pub view: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Index {
    pub oid: i64,
    pub natts: i64,
    /// Key columns' attnums, 0 for an expression.
    pub keys: Vec<i64>,
    pub classes: Vec<i64>,
    pub am: i64,
    pub in_init_file: bool,
}

/// `typcache.c`'s `TCFLAGS_CHECKED_*` flags, and what a caller asks for.
const TC_BTREE: u32 = 1;
const TC_HASH: u32 = 2;
const TC_EQ: u32 = 4;
const TC_LT: u32 = 8;
const TC_GT: u32 = 16;
const TC_CMP: u32 = 32;
const TC_HASHPROC: u32 = 64;
const HASH_AM: i64 = 405;

/// A type's default operator classes' families and input types, and its
/// ordering and equality operators, as the type cache finds them.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TypeOps {
    pub btree: Option<(i64, i64)>,
    pub hash: Option<(i64, i64)>,
    pub eq: i64,
    pub lt: i64,
    pub gt: i64,
}

/// The database's default tablespace (`MyDatabaseTableSpace`).
const DEFAULT_TABLESPACE: i64 = 1663;

/// How deep an expression is walked.
const MAX_DEPTH: u32 = 400;

/// The longest statement text walked, in bytes.
const MAX_WALKED_TEXT: usize = 64 * 1024;

/// What the statement's FROM put in scope.
#[derive(Debug, Clone, Default)]
struct Scope {
    rels: Vec<(String, Rel)>,
}

/// A base relation's planner inputs: the columns its target needs and its
/// restriction clauses.
#[derive(Debug, Clone, Default)]
struct Needs {
    target: Vec<i64>,
    /// `(attnum, operator, the other side's a constant)`.
    quals: Vec<(i64, i64, bool)>,
    /// Columns read anywhere (for an index-only scan).
    all: HashSet<i64>,
    /// Equality constants on columns, for a catalog's executor scan.
    eq_consts: Vec<(i64, Bson)>,
    /// `column IN (...)`: `(attnum, element type)`.
    saops: Vec<(i64, i64)>,
    /// Columns in join clauses.
    joins: Vec<i64>,
}

/// [`Model::facts`]' memo: `(session, catalog version, invalidation
/// generation)` and what was read under it.
#[derive(Default)]
struct FactMemo {
    key: (usize, u64, u64),
    rels: HashMap<(i64, String), Option<Rel>>,
    catalogs: HashMap<i64, Option<Rel>>,
    functions: HashMap<String, Vec<cf::ProcFact>>,
}

thread_local! {
    static FACTS: std::cell::RefCell<FactMemo> = std::cell::RefCell::new(FactMemo::default());
}

/// The walk over one statement.
pub(crate) struct Model<'a> {
    h: &'a PgHandler,
    be: &'a mut Backend,
    ct: Counter,
    superuser: bool,
    user: String,
    path_setting: String,
    /// The types of `$1`, `$2`, ... (a SQL function's body).
    params: Vec<i64>,
    /// The executor's scans of catalogs, counted again on every run.
    pub(crate) exec: Vec<Scan>,
    /// The planner inputs gathered per range-table relation.
    needs: HashMap<i64, Needs>,
    /// Walking a stored query (a view's rule): its names and operators were
    /// resolved when it was stored, so only the planner and executor read.
    stored: bool,
    /// Inside a stored query's name resolution: nothing is read.
    silent: u32,
    /// Expression nesting, and the SQL functions whose bodies are being
    /// walked (`inline_function`'s `active_fns`).
    depth: u32,
    active_fns: Vec<i64>,
}

fn names_of(nodes: &[pb::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn node(n: &Option<Box<pb::Node>>) -> Option<&N> {
    n.as_ref().and_then(|b| b.node.as_ref())
}

impl<'a> Model<'a> {
    pub(crate) fn new(h: &'a PgHandler, be: &'a mut Backend, ct: Counter) -> Self {
        let user = h.cr_current_user();
        let superuser = h.cr_is_superuser(&user);
        let path_setting = h.cr_search_path_setting();
        Model {
            h,
            be,
            ct,
            superuser,
            user,
            path_setting,
            params: Vec::new(),
            exec: Vec::new(),
            needs: HashMap::new(),
            stored: false,
            silent: 0,
            depth: 0,
            active_fns: Vec::new(),
        }
    }

    /// Run a parser step: in a stored query, without reading anything.
    fn parser<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if !self.stored {
            return f(self);
        }
        self.silent += 1;
        let r = f(self);
        self.silent -= 1;
        r
    }

    // ------------------------------------------------------------- facts

    /// The facts this walk reads, kept per session for as long as no
    /// catalog changed and no invalidation reached it: a statement walked
    /// for the first time asks for the same relations and functions as the
    /// last, and reading them from the catalogs each time cost more than
    /// the statement.
    fn facts<R>(&mut self, f: impl FnOnce(&mut FactMemo) -> R) -> R {
        let key = (
            self.h as *const PgHandler as usize,
            crate::catalog_cache()
                .version
                .load(std::sync::atomic::Ordering::SeqCst),
            self.be.generation(),
        );
        FACTS.with(|m| {
            let mut m = m.borrow_mut();
            if m.key != key {
                *m = FactMemo {
                    key,
                    ..FactMemo::default()
                };
            }
            f(&mut m)
        })
    }

    fn rel_in(&mut self, nsp: i64, name: &str) -> Option<Rel> {
        let k = (nsp, name.to_string());
        if let Some(r) = self.facts(|m| m.rels.get(&k).cloned()) {
            return r;
        }
        let r = self.h.cr_relation_in(nsp, name);
        self.facts(|m| m.rels.insert(k, r.clone()));
        r
    }

    fn catalog_rel(&mut self, oid: i64) -> Option<Rel> {
        if let Some(r) = self.facts(|m| m.catalogs.get(&oid).cloned()) {
            return r;
        }
        let r = self.h.cr_catalog(oid);
        self.facts(|m| m.catalogs.insert(oid, r.clone()));
        r
    }

    fn functions_named(&mut self, name: &str) -> Vec<cf::ProcFact> {
        if let Some(r) = self.facts(|m| m.functions.get(name).cloned()) {
            return r;
        }
        let r = self.h.cr_functions_named(name);
        self.facts(|m| m.functions.insert(name.to_string(), r.clone()));
        r
    }

    // ------------------------------------------------------------- caches

    fn search(&mut self, cache: Cache, key: Key, found: impl FnOnce(&PgHandler) -> bool) {
        if self.silent > 0 || self.be.cached(cache, &key) {
            return;
        }
        let n = i64::from(found(self.h));
        self.be.search(&self.ct, cache, key, n);
    }

    fn type_oid(&mut self, t: i64) {
        if t <= 0 {
            return;
        }
        self.search(Cache::TypeOid, vec![K::O(t)], |_| true);
    }

    /// A type the parser looks up (a coercion's target, a node's type for
    /// its collation): a stored query's were looked up when it was stored.
    fn ptype(&mut self, t: i64) {
        if !self.stored {
            self.type_oid(t);
        }
    }

    fn proc_oid(&mut self, p: i64) {
        if p <= 0 {
            return;
        }
        self.search(Cache::ProcOid, vec![K::O(p)], |_| true);
    }

    fn oper_oid(&mut self, o: i64) {
        if o <= 0 {
            return;
        }
        self.search(Cache::OperOid, vec![K::O(o)], |_| true);
    }

    // ---------------------------------------------------------- backend start

    /// `InitPostgres` with the init files in place.
    pub(crate) fn backend_start(&mut self, db_oid: i64, db_name: &str, role_oid: i64) {
        // `GetDatabaseTuple`: a direct scan by name, uncached.
        self.ct.index(2671, 1);
        // `InitializeSessionUserId`: AUTHNAME, then AUTHOID.
        let user = self.user.clone();
        self.search(Cache::AuthName, vec![K::N(user)], |_| true);
        self.search(Cache::AuthOid, vec![K::O(role_oid)], |_| true);
        // `GetDatabaseTupleByOid`, rechecking after the lock: uncached.
        self.ct.index(2672, 1);
        // `CheckMyDatabase`: DATABASEOID.
        self.search(Cache::DatabaseOid, vec![K::O(db_oid)], |_| true);
        let _ = db_name;
        // `process_settings`: four scans of pg_db_role_setting's index,
        // each through the relation's descriptor, which no init file holds.
        self.open_catalog(2964);
        self.open_catalog_index(2965);
        let rows = self.h.cr_role_settings(db_oid, role_oid);
        for n in rows {
            self.ct.index(2965, n);
        }
        self.be.started = true;
    }

    // ------------------------------------------------------------ namespace.c

    /// `recomputeNamespacePath`: the path's schemas looked up by name, once
    /// per path setting and role.
    fn ensure_path(&mut self) {
        let key = format!("{}\u{1f}{}", self.path_setting, self.user);
        if self.be.search_path.as_deref() == Some(key.as_str()) {
            return;
        }
        for elem in self.path_elements() {
            let name = if elem == "$user" {
                self.user.clone()
            } else {
                elem
            };
            let oid = self.h.cr_namespace_oid(&name);
            self.search(Cache::NamespaceName, vec![K::N(name.clone())], |_| {
                oid.is_some()
            });
            if let (Some(oid), false) = (oid, self.superuser) {
                // `pg_namespace_aclcheck` for USAGE.
                self.search(Cache::NamespaceOid, vec![K::O(oid)], |_| true);
            }
        }
        self.be.search_path = Some(key);
    }

    fn path_elements(&self) -> Vec<String> {
        self.path_setting
            .split(',')
            .map(|p| {
                let p = p.trim();
                if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
                    p[1..p.len() - 1].replace("\"\"", "\"")
                } else {
                    p.to_ascii_lowercase()
                }
            })
            .filter(|p| !p.is_empty())
            .collect()
    }

    /// `activeSearchPath`: pg_catalog first unless the path places it, then
    /// the path's schemas that exist.
    fn active_path(&mut self) -> Vec<i64> {
        self.ensure_path();
        let mut out = Vec::new();
        let elems = self.path_elements();
        if !elems.iter().any(|e| e == "pg_catalog") {
            out.push(PG_CATALOG);
        }
        for e in elems {
            let name = if e == "$user" { self.user.clone() } else { e };
            if let Some(o) = self.h.cr_namespace_oid(&name) {
                if !out.contains(&o) {
                    out.push(o);
                }
            }
        }
        out
    }

    /// `LookupExplicitNamespace`.
    fn explicit_namespace(&mut self, name: &str) -> Option<i64> {
        let oid = self.h.cr_namespace_oid(name);
        self.search(Cache::NamespaceName, vec![K::N(name.to_string())], |_| {
            oid.is_some()
        });
        if let (Some(o), false) = (oid, self.superuser) {
            self.search(Cache::NamespaceOid, vec![K::O(o)], |_| true);
        }
        oid
    }

    /// `RangeVarGetRelidExtended`: the relation along the path (or in its
    /// schema), then its descriptor.
    fn range_var(&mut self, rv: &pb::RangeVar) -> Option<Rel> {
        let name = rv.relname.clone();
        if self.stored {
            // A stored query names its relations by oid.
            let spaces = if rv.schemaname.is_empty() {
                let mut v = vec![PG_CATALOG];
                for e in self.path_elements() {
                    let n = if e == "$user" { self.user.clone() } else { e };
                    if let Some(o) = self.h.cr_namespace_oid(&n) {
                        v.push(o);
                    }
                }
                v
            } else {
                vec![self.h.cr_namespace_oid(&rv.schemaname)?]
            };
            let rel = spaces.into_iter().find_map(|nsp| self.rel_in(nsp, &name))?;
            self.open_relation(&rel);
            return Some(rel);
        }
        let spaces = if rv.schemaname.is_empty() {
            self.active_path()
        } else {
            vec![self.explicit_namespace(&rv.schemaname)?]
        };
        for nsp in spaces {
            let rel = self.rel_in(nsp, &name);
            let key = vec![K::N(name.clone()), K::O(nsp)];
            self.search(Cache::RelNameNsp, key, |_| rel.is_some());
            if let Some(rel) = rel {
                self.open_relation(&rel);
                return Some(rel);
            }
        }
        None
    }

    // ------------------------------------------------------------ relcache.c

    /// `RelationIdGetRelation`: build the descriptor once.
    fn open_relation(&mut self, rel: &Rel) {
        if rel.in_init_file || !self.be.build_relation(rel.oid) {
            return;
        }
        // `ScanPgRelation`: a direct scan of pg_class_oid_index.
        self.ct.index_key(2662, &vec![K::O(rel.oid)], 1);
        if rel.kind == 'i' {
            return;
        }
        // `RelationInitTableAccessMethod`: a user table's access method.
        if !rel.catalog && matches!(rel.kind, 'r' | 'm' | 't') {
            self.search(Cache::AmOid, vec![K::O(HEAP_AM)], |_| true);
        }
        // `RelationBuildTupleDesc`: every attribute numbered above zero.
        self.ct.index_key(2659, &vec![K::O(rel.oid)], rel.natts);
        if rel.defaults > 0 {
            // `AttrDefaultFetch`.
            self.open_catalog(2604);
            self.open_catalog_index(2656);
            self.ct.index(2656, rel.defaults);
        }
        if rel.checks > 0 {
            // `CheckConstraintFetch`.
            self.open_catalog_index(2665);
            self.ct.index(2665, rel.checks);
        }
        if rel.kind == 'v' {
            // `RelationBuildRuleLock`: the view's one rule.
            self.ct.index(2693, 1);
        }
    }

    /// A catalog's descriptor, built on its first open when no init file
    /// holds it.
    fn open_catalog(&mut self, oid: i64) {
        if let Some(rel) = self.catalog_rel(oid) {
            self.open_relation(&rel);
        }
    }

    fn open_catalog_index(&mut self, oid: i64) {
        if let Some(ix) = catalog_index(oid) {
            self.open_index(&ix);
        }
    }

    /// An index's descriptor: its `pg_class` and attribute rows, its
    /// `pg_index` row, access method and operator classes, and each
    /// column's attribute options (`RelationGetIndexAttOptions`).
    fn open_index(&mut self, ix: &Index) {
        if ix.in_init_file || !self.be.build_relation(ix.oid) {
            return;
        }
        self.ct.index_key(2662, &vec![K::O(ix.oid)], 1);
        self.ct.index_key(2659, &vec![K::O(ix.oid)], ix.natts);
        self.search(Cache::IndexRelid, vec![K::O(ix.oid)], |_| true);
        self.search(Cache::AmOid, vec![K::O(ix.am)], |_| true);
        for class in &ix.classes {
            self.opclass_info(*class, ix.am);
        }
        for att in 1..=ix.natts {
            self.search(Cache::AttNum, vec![K::O(ix.oid), K::O(att)], |_| true);
        }
    }

    /// `LookupOpclassInfo`: the class's row and its support procedures,
    /// once per backend.
    fn opclass_info(&mut self, class: i64, am: i64) {
        if !self.be.opclasses.insert(class) {
            return;
        }
        self.ct.index(2687, 1);
        let b = builtin();
        let procs = self.h.cr_opclass(class).map_or(0, |o| {
            if am == BTREE_AM || am != 0 {
                b.amproc_count
                    .get(&(o.family, o.intype, o.intype))
                    .copied()
                    .unwrap_or(0)
            } else {
                0
            }
        });
        self.ct.index(2655, procs);
    }

    /// `RelationGetIndexList`: read once per descriptor.
    fn index_list(&mut self, rel: &Rel) {
        if self.be.load_index_list(rel.oid) {
            self.open_catalog_index(2678);
            self.ct.index(2678, rel.indexes.len() as i64);
        }
    }

    /// `RelationGetStatExtList`.
    fn statext_list(&mut self, rel: &Rel) {
        if self.be.load_statext_list(rel.oid) {
            self.open_catalog_index(3379);
            self.ct.index(3379, self.h.cr_statext_count(rel.oid));
        }
    }

    // -------------------------------------------------------------- planner

    /// `get_relation_info` and the base relation's size and costs.
    fn plan_rel(&mut self, rel: &Rel) {
        if rel.kind == 'v' {
            return;
        }
        // A relation without `relhasindex` has no index list to read.
        if !rel.indexes.is_empty() {
            self.index_list(rel);
        }
        for ix in &rel.indexes {
            self.open_index(ix);
        }
        self.statext_list(rel);
        let needs = self.needs.get(&rel.oid).cloned().unwrap_or_default();
        if matches!(rel.kind, 'r' | 'm' | 't') {
            // `table_block_relation_estimate_size`: a table never measured
            // has its density from every column's width.
            if !rel.measured {
                for (att, _, typ) in rel.columns.clone() {
                    self.att_width(rel, att, typ);
                }
            }
            // `get_tablespace_page_costs`, once per tablespace.
            let spc = if rel.tablespace == 0 {
                DEFAULT_TABLESPACE
            } else {
                rel.tablespace
            };
            if self.be.tablespaces.insert(spc) {
                self.search(Cache::TableSpaceOid, vec![K::O(spc)], |_| true);
            }
        }
        // The restrictions: `clause_selectivity` reads each column's
        // statistics; an equality is checked for merge joins.
        for (att, opr, _) in &needs.quals {
            self.statistic(rel, *att);
            self.mergejoinable(*opr);
        }
        for (att, elem) in &needs.saops {
            // `scalararraysel`: is it the type's equality?
            self.statistic(rel, *att);
            self.type_cache(*elem, TC_EQ);
        }
        for att in &needs.joins {
            self.statistic(rel, *att);
        }
        // `set_rel_width`: the target's columns.
        for att in &needs.target {
            if let Some((_, _, typ)) = rel.columns.iter().find(|c| c.0 == *att).cloned() {
                self.att_width(rel, *att, typ);
            }
        }
        // Index paths: an index whose leading column a restriction matches
        // is costed (`btcostestimate` reads the correlation through the
        // ordering operator when the column has statistics); an index
        // holding every column read gives an index-only scan.
        let mut used: Option<(Index, bool)> = None;
        for ix in &rel.indexes {
            let Some(lead) = ix.keys.first().copied() else {
                continue;
            };
            // `match_clause_to_indexcol`: `op_in_opfamily`, an AMOPOPID
            // search for the operator's membership in the column's family.
            let class = ix.classes.first().copied().unwrap_or(0);
            let family = self.h.cr_opclass(class).map_or(0, |o| o.family);
            let mut matches = false;
            for (a, opr, _) in needs.quals.clone() {
                if a != lead {
                    continue;
                }
                let member = self.op_in_family(opr, class);
                self.search(
                    Cache::AmopOpid,
                    vec![K::O(opr), K::N("s".to_string()), K::O(family)],
                    |_| member,
                );
                matches |= member;
            }
            if !matches {
                continue;
            }
            if rel.stats.contains(&lead) {
                if let Some(o) = ix.classes.first().and_then(|c| self.h.cr_opclass(*c)) {
                    let key = vec![K::O(o.family), K::O(o.intype), K::O(o.intype), K::O(1)];
                    let found = builtin()
                        .amop_strategy
                        .contains_key(&(o.family, o.intype, o.intype, 1));
                    self.search(Cache::AmopStrategy, key, |_| found);
                }
            }
            let covering = needs.all.iter().all(|a| ix.keys.contains(a));
            if used.is_none() {
                used = Some((ix.clone(), covering));
            }
        }
        // The executor's index-only scan builds its tuple from the index's
        // columns' types.
        if let Some((ix, true)) = &used {
            for k in ix.keys.clone() {
                if let Some((_, _, t)) = rel.columns.iter().find(|c| c.0 == k).cloned() {
                    self.type_oid(t);
                }
            }
        }
        // A catalog read directly is scanned by the executor.
        if rel.catalog && matches!(rel.kind, 'r') {
            let scan = match &used {
                Some((ix, _)) => {
                    let n = self.h.cr_catalog_matches(rel, ix, &needs.eq_consts);
                    Scan::Index(ix.oid, n)
                }
                None => Scan::Seq(rel.oid, self.h.cr_catalog_rows(rel)),
            };
            self.exec.push(scan);
        }
    }

    /// `get_attavgwidth`, then `get_typavgwidth` when no statistics give
    /// the width.
    fn att_width(&mut self, rel: &Rel, att: i64, typ: i64) {
        self.statistic(rel, att);
        if !rel.stats.contains(&att) {
            self.type_oid(typ);
        }
    }

    fn statistic(&mut self, rel: &Rel, att: i64) {
        let has = rel.stats.contains(&att);
        self.search(
            Cache::StatRelAttInh,
            vec![K::O(rel.oid), K::O(att), K::O(0)],
            |_| has,
        );
    }

    /// `check_mergejoinable`: an operator that can merge is looked up in
    /// every operator family it belongs to.
    fn mergejoinable(&mut self, opr: i64) {
        let Some(o) = self.h.cr_operator(opr) else {
            return;
        };
        if !o.canmerge {
            return;
        }
        self.amop_list(opr);
    }

    /// `SearchSysCacheList1(AMOPOPID, opr)`: the operator's every operator
    /// family membership (`get_mergejoin_opfamilies`,
    /// `get_ordering_op_properties`, `get_op_hash_functions`).
    fn amop_list(&mut self, opr: i64) {
        if opr <= 0 || self.silent > 0 {
            return;
        }
        let members: Vec<Key> = builtin()
            .amop_by_opr
            .get(&opr)
            .map(|v| {
                v.iter()
                    .map(|(p, f)| vec![K::O(opr), K::N(p.to_string()), K::O(*f)])
                    .collect()
            })
            .unwrap_or_default();
        self.be
            .search_list(&self.ct, Cache::AmopOpid, vec![K::O(opr)], || members);
    }

    /// `get_opfamily_member`: AMOPSTRATEGY.
    fn opfamily_member(&mut self, fam: i64, l: i64, r: i64, strategy: i64) -> i64 {
        let found = builtin()
            .amop_strategy
            .get(&(fam, l, r, strategy))
            .copied()
            .unwrap_or(0);
        self.search(
            Cache::AmopStrategy,
            vec![K::O(fam), K::O(l), K::O(r), K::O(strategy)],
            |_| found != 0,
        );
        found
    }

    /// `get_opfamily_proc`: AMPROCNUM.
    fn opfamily_proc(&mut self, fam: i64, l: i64, r: i64, num: i64) -> bool {
        let found = builtin().amproc_num.contains(&(fam, l, r, num));
        self.search(
            Cache::AmprocNum,
            vec![K::O(fam), K::O(l), K::O(r), K::O(num)],
            |_| found,
        );
        found
    }

    /// `IsBinaryCoercible`.
    fn binary_coercible(&mut self, src: i64, target: i64) -> bool {
        if src == target || matches!(target, cf::ANY | 2283 | 5077) {
            return true;
        }
        self.type_oid(src);
        let src = self.h.cr_base_type(src);
        if src == target {
            return true;
        }
        let t = builtin().types.get(&src).cloned().unwrap_or_default();
        let is_array = t.elem != 0 && t.category == 'A';
        let special = match target {
            2277 | 5078 => is_array,
            2776 | 5079 => !is_array,
            3500 => t.typtype == 'e',
            3831 | 5080 => t.typtype == 'r',
            4537 | 4538 => t.typtype == 'm',
            2249 => t.typtype == 'c',
            _ => false,
        };
        if special {
            return true;
        }
        let cast = self.h.cr_cast(src, target);
        self.search(
            Cache::CastSourceTarget,
            vec![K::O(src), K::O(target)],
            |_| cast.is_some(),
        );
        cast.is_some_and(|(_, ctx, m)| m == 'b' && ctx == 'i')
    }

    /// `GetDefaultOpClass`: a scan of the access method's every operator
    /// class, each default one's input type checked against the type until
    /// one matches exactly.
    fn default_opclass(&mut self, typ: i64, am: i64) -> Option<cf::OpclassFact> {
        self.type_oid(typ);
        let typ = self.h.cr_base_type(typ);
        let (cat, _) = self.category(typ);
        let classes: Vec<cf::OpclassFact> = builtin()
            .opclasses_by_am
            .get(&am)
            .map(|v| {
                v.iter()
                    .filter_map(|o| builtin().opclasses.get(o).cloned())
                    .collect()
            })
            .unwrap_or_default();
        self.open_catalog_index(2686);
        self.ct.index(2686, classes.len() as i64);
        let mut nexact = 0;
        let mut ncompat_pref = 0;
        let mut result = None;
        for o in classes {
            if !o.default {
                continue;
            }
            if o.intype == typ {
                nexact += 1;
                result = Some(o);
            } else if nexact == 0 && self.binary_coercible(typ, o.intype) {
                let (c, pref) = self.category(o.intype);
                if c == cat && pref {
                    ncompat_pref += 1;
                    result = Some(o);
                } else if ncompat_pref == 0 {
                    result = Some(o);
                }
            }
        }
        result
    }

    /// `lookup_type_cache` for `want` (the `TC_*` flags).
    fn type_cache(&mut self, typ: i64, mut want: u32) -> TypeOps {
        self.type_oid(typ);
        let mut flags = self.be.typcache.get(&typ).copied().unwrap_or(0);
        let ops = self.h.cr_type_ops(typ);
        if want & (TC_EQ | TC_LT | TC_GT | TC_CMP) != 0 && flags & TC_BTREE == 0 {
            if let Some(o) = self.default_opclass(typ, BTREE_AM) {
                self.search(Cache::ClaOid, vec![K::O(o.oid)], |_| true);
            }
            flags &= !(TC_EQ | TC_LT | TC_GT | TC_CMP);
            flags |= TC_BTREE;
        }
        if want & TC_EQ != 0 && flags & TC_EQ == 0 && ops.btree.is_none() {
            want |= TC_HASH;
        }
        if want & (TC_HASHPROC | TC_HASH) != 0 && flags & TC_HASH == 0 {
            if let Some(o) = self.default_opclass(typ, HASH_AM) {
                self.search(Cache::ClaOid, vec![K::O(o.oid)], |_| true);
            }
            flags &= !TC_HASHPROC;
            flags |= TC_HASH;
        }
        if want & TC_EQ != 0 && flags & TC_EQ == 0 {
            let mut eq = 0;
            if let Some((f, t)) = ops.btree {
                eq = self.opfamily_member(f, t, t, 3);
            }
            if eq == 0 {
                if let Some((f, t)) = ops.hash {
                    self.opfamily_member(f, t, t, 1);
                }
            }
            flags &= !TC_HASHPROC;
            flags |= TC_EQ;
        }
        if want & TC_LT != 0 && flags & TC_LT == 0 {
            if let Some((f, t)) = ops.btree {
                self.opfamily_member(f, t, t, 1);
            }
            flags |= TC_LT;
        }
        if want & TC_GT != 0 && flags & TC_GT == 0 {
            if let Some((f, t)) = ops.btree {
                self.opfamily_member(f, t, t, 5);
            }
            flags |= TC_GT;
        }
        if want & TC_CMP != 0 && flags & TC_CMP == 0 {
            if let Some((f, t)) = ops.btree {
                self.opfamily_proc(f, t, t, 1);
            }
            flags |= TC_CMP;
        }
        if want & TC_HASHPROC != 0 && flags & TC_HASHPROC == 0 {
            if let Some((f, t)) = ops.hash {
                let matches = ops.eq == 0 || ops.eq == self.opfamily_member(f, t, t, 1);
                if matches {
                    self.opfamily_proc(f, t, t, 1);
                }
            }
            flags |= TC_HASHPROC;
        }
        self.be.typcache.insert(typ, flags);
        ops
    }

    /// An ORDER BY or GROUP BY item: `get_sort_group_operators` (the type
    /// cache's ordering, equality and hash support), the planner's pathkey
    /// (`get_ordering_op_properties`, `get_mergejoin_opfamilies`), and, for
    /// a sort, the executor's sort support.
    fn sort_item(&mut self, typ: i64, desc: bool, group: bool) {
        if typ == UNKNOWN || typ <= 0 {
            return;
        }
        let ops = self.type_cache(typ, TC_LT | TC_EQ | TC_GT | TC_HASHPROC);
        let sortop = if desc { ops.gt } else { ops.lt };
        self.amop_list(sortop);
        self.amop_list(ops.eq);
        if group {
            // Hashed grouping: the equality operator's function.
            self.oper_oid(ops.eq);
        } else if let Some((f, t)) = ops.btree {
            // `PrepareSortSupportFromOrderingOp`.
            if !self.opfamily_proc(f, t, t, 2) {
                self.opfamily_proc(f, t, t, 1);
            }
        }
    }

    /// `op_in_opfamily`, answered from the operator's family list.
    fn op_in_family(&self, opr: i64, class: i64) -> bool {
        let Some(o) = self.h.cr_opclass(class) else {
            return false;
        };
        builtin()
            .amop_by_opr
            .get(&opr)
            .is_some_and(|v| v.iter().any(|(p, f)| *p == 's' && *f == o.family))
    }

    // ---------------------------------------------------------- statements

    /// Walk one statement.
    pub(crate) fn statement(&mut self, stmt: &N) {
        match stmt {
            N::SelectStmt(s) => {
                self.select(s, true);
            }
            N::InsertStmt(i) => self.insert(i),
            N::CreateStmt(c) => self.create_table(c),
            N::UpdateStmt(u) => {
                self.update_or_delete(u.relation.as_ref(), &u.target_list, node(&u.where_clause))
            }
            N::DeleteStmt(d) => {
                self.update_or_delete(d.relation.as_ref(), &[], node(&d.where_clause))
            }
            N::ExplainStmt(e) => {
                if let Some(q) = node(&e.query) {
                    self.statement(q);
                }
            }
            _ => {}
        }
    }

    /// CREATE TABLE's checks for an existing relation and row type of the
    /// name (`heap_create_with_catalog`, `makeArrayTypeName`) and its
    /// columns' types; the rest of its catalog work is not walked (RP-290).
    fn create_table(&mut self, c: &pb::CreateStmt) {
        let Some(rv) = c.relation.as_ref() else {
            return;
        };
        let nsp = if rv.schemaname.is_empty() {
            match self.active_path().into_iter().find(|n| *n != PG_CATALOG) {
                Some(n) => n,
                None => return,
            }
        } else {
            match self.explicit_namespace(&rv.schemaname) {
                Some(n) => n,
                None => return,
            }
        };
        for e in &c.table_elts {
            if let Some(N::ColumnDef(cd)) = e.node.as_ref() {
                if let Some(t) = cd.type_name.as_ref() {
                    self.type_name(t);
                }
            }
        }
        let name = rv.relname.clone();
        self.search(
            Cache::RelNameNsp,
            vec![K::N(name.clone()), K::O(nsp)],
            |_| false,
        );
        self.search(
            Cache::TypeNameNsp,
            vec![K::N(name.clone()), K::O(nsp)],
            |_| false,
        );
        self.search(
            Cache::TypeNameNsp,
            vec![K::N(format!("_{name}")), K::O(nsp)],
            |_| false,
        );
    }

    /// An INSERT: the target's descriptor, each value coerced to its
    /// column, the permission check, the inserted tuple's types, and the
    /// indexes the executor opens to insert into.
    fn insert(&mut self, i: &pb::InsertStmt) {
        let Some(rel) = i.relation.as_ref().and_then(|rv| self.range_var(rv)) else {
            return;
        };
        let cols: Vec<(i64, String, i64)> = if i.cols.is_empty() {
            rel.columns.clone()
        } else {
            i.cols
                .iter()
                .filter_map(|c| match c.node.as_ref() {
                    Some(N::ResTarget(rt)) => rel.columns.iter().find(|x| x.1 == rt.name).cloned(),
                    _ => None,
                })
                .collect()
        };
        if let Some(N::SelectStmt(sel)) = node(&i.select_stmt) {
            let types = self.select(sel, false);
            for (t, (_, _, ct)) in types.iter().zip(&cols) {
                self.coerce(*t, *ct, false);
            }
        }
        self.search(Cache::RelOid, vec![K::O(rel.oid)], |_| true);
        // `ExecInitInsertProjection`: every column's type, a dropped one's
        // NULL as int4.
        for (_, _, t) in rel.columns.clone() {
            self.type_oid(t);
        }
        if rel.natts > rel.columns.len() as i64 {
            self.type_oid(INT4);
        }
        // `ExecOpenIndices`.
        if !rel.indexes.is_empty() {
            self.index_list(&rel);
            for ix in rel.indexes.clone() {
                self.open_index(&ix);
            }
        }
    }

    /// An UPDATE or DELETE: its target planned as a scan, each SET value
    /// coerced to its column; the executor's permission check, the
    /// replica identity's and HOT's index list, the publication descriptor
    /// (`CheckCmdReplicaIdentity`) and the row's `ctid`.
    fn update_or_delete(&mut self, rv: Option<&pb::RangeVar>, set: &[pb::Node], quals: Option<&N>) {
        let Some(rel) = rv.and_then(|rv| self.range_var(rv)) else {
            return;
        };
        let alias = rv
            .and_then(|rv| rv.alias.as_ref().map(|a| a.aliasname.clone()))
            .unwrap_or_else(|| rel.name.clone());
        let scope = Scope {
            rels: vec![(alias, rel.clone())],
        };
        self.needs.entry(rel.oid).or_default();
        for t in set {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                continue;
            };
            let Some(v) = node(&rt.val) else { continue };
            let typ = self.expr(v, &scope, true);
            if let Some((_, _, ct)) = rel.columns.iter().find(|c| c.1 == rt.name).cloned() {
                self.coerce(typ, ct, false);
            }
        }
        if let Some(w) = quals {
            self.qual(w, &scope);
        }
        self.plan_rel(&rel);
        self.search(Cache::RelOid, vec![K::O(rel.oid)], |_| true);
        self.index_list(&rel);
        for ix in rel.indexes.clone() {
            self.open_index(&ix);
        }
        if self.be.build_pubdesc(rel.oid) {
            self.be.search_list(
                &self.ct,
                Cache::PublicationRelMap,
                vec![K::O(rel.oid)],
                Vec::new,
            );
            self.be.search_list(
                &self.ct,
                Cache::PublicationNamespaceMap,
                vec![K::O(rel.nsp)],
                Vec::new,
            );
            // `GetAllTablesPublications`: a heap scan for FOR ALL TABLES.
            self.ct.seq(6104, 0);
        }
        self.type_oid(TID);
    }

    /// A SELECT: its FROM, target list and WHERE as the parser analyses
    /// them, each base relation as the planner sizes it, and the executor's
    /// permission check, scans and result types.
    fn select(&mut self, s: &pb::SelectStmt, top: bool) -> Vec<i64> {
        if !s.values_lists.is_empty() {
            let mut types = Vec::new();
            for row in &s.values_lists {
                if let Some(N::List(l)) = row.node.as_ref() {
                    types = l
                        .items
                        .iter()
                        .map(|i| self.expr_n(i, &Scope::default()))
                        .collect();
                }
            }
            return types;
        }
        if s.op != pb::SetOperation::SetopNone as i32 && s.op != 0 {
            let mut out = Vec::new();
            if let Some(l) = s.larg.as_ref() {
                out = self.select(l, top);
            }
            if let Some(r) = s.rarg.as_ref() {
                self.select(r, top);
            }
            return out;
        }
        let mut scope = Scope::default();
        for f in &s.from_clause {
            self.range_item(f, &mut scope);
        }
        // The target list.
        let mut out_types = Vec::new();
        for t in &s.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                continue;
            };
            let Some(v) = node(&rt.val) else { continue };
            if let N::ColumnRef(c) = v {
                if c.fields
                    .last()
                    .is_some_and(|f| matches!(f.node, Some(N::AStar(_))))
                {
                    for (alias, rel) in scope.rels.clone() {
                        let qual = names_of(&c.fields);
                        if !qual.is_empty() && qual[0] != alias {
                            continue;
                        }
                        for (att, _, typ) in rel.columns.clone() {
                            self.need_target(rel.oid, att);
                            out_types.push(typ);
                        }
                    }
                    continue;
                }
            }
            let t = self.expr(v, &scope, true);
            // `resolveTargetListUnknowns`: an untyped literal becomes text.
            let t = if t == UNKNOWN {
                self.ptype(TEXT);
                TEXT
            } else {
                t
            };
            out_types.push(t);
        }
        if let Some(w) = node(&s.where_clause) {
            let t = self.qual(w, &scope);
            let _ = t;
        }
        // `transformGroupClause` / `transformSortClause`.
        for g in &s.group_clause {
            if let Some(e) = g.node.as_ref() {
                let t = self.expr(e, &scope, true);
                self.sort_item(t, false, true);
            }
        }
        for sb in &s.sort_clause {
            if let Some(N::SortBy(b)) = sb.node.as_ref() {
                if let Some(e) = node(&b.node) {
                    let t = match e {
                        // ORDER BY n: the n-th result column.
                        N::AConst(c) => match &c.val {
                            Some(pb::a_const::Val::Ival(i)) => out_types
                                .get((i.ival - 1).max(0) as usize)
                                .copied()
                                .unwrap_or(UNKNOWN),
                            _ => self.expr(e, &scope, true),
                        },
                        _ => self.expr(e, &scope, true),
                    };
                    let desc = b.sortby_dir == pb::SortByDir::SortbyDesc as i32;
                    self.sort_item(t, desc, false);
                }
            }
        }
        // `transformLimitClause`: the count and offset are int8.
        for l in [node(&s.limit_count), node(&s.limit_offset)]
            .into_iter()
            .flatten()
        {
            let t = self.expr(l, &scope, true);
            self.coerce(t, INT8, false);
        }
        // The planner, per base relation (a view became its query).
        for (_, rel) in scope.rels.clone() {
            if rel.view.is_none() {
                self.plan_rel(&rel);
            }
        }
        // The executor: `ExecCheckRTPerms` on every relation, then the
        // result's types (the tuple descriptor and the row description).
        for (_, rel) in scope.rels.clone() {
            self.search(Cache::RelOid, vec![K::O(rel.oid)], |_| true);
        }
        if top {
            for t in &out_types {
                self.type_oid(*t);
            }
        }
        out_types
    }

    fn range_item(&mut self, f: &pb::Node, scope: &mut Scope) {
        match f.node.as_ref() {
            Some(N::RangeVar(rv)) => {
                if let Some(rel) = self.range_var(rv) {
                    let alias = rv
                        .alias
                        .as_ref()
                        .map_or_else(|| rv.relname.clone(), |a| a.aliasname.clone());
                    self.needs.entry(rel.oid).or_default();
                    if let Some(sql) = rel.view.clone() {
                        // The rewriter puts the view's stored query in its
                        // place: planned and checked as its own relations.
                        self.view_query(&sql);
                    }
                    scope.rels.push((alias, rel));
                }
            }
            Some(N::JoinExpr(j)) => {
                if let Some(l) = j.larg.as_ref() {
                    self.range_item(l, scope);
                }
                if let Some(r) = j.rarg.as_ref() {
                    self.range_item(r, scope);
                }
                if let Some(q) = node(&j.quals) {
                    self.qual(q, scope);
                }
            }
            _ => {}
        }
    }

    /// A query's result columns, `(name, type)`, read silently.
    fn target_columns(&mut self, s: &pb::SelectStmt) -> Vec<(String, i64)> {
        let mut scope = Scope::default();
        for f in &s.from_clause {
            self.range_item(f, &mut scope);
        }
        let mut out = Vec::new();
        for t in &s.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                continue;
            };
            let Some(v) = node(&rt.val) else { continue };
            if let N::ColumnRef(c) = v {
                if c.fields
                    .last()
                    .is_some_and(|f| matches!(f.node, Some(N::AStar(_))))
                {
                    let qual = names_of(&c.fields);
                    for (alias, rel) in &scope.rels {
                        if !qual.is_empty() && qual[0] != *alias {
                            continue;
                        }
                        out.extend(rel.columns.iter().map(|(_, n, t)| (n.clone(), *t)));
                    }
                    continue;
                }
            }
            let typ = self.expr(v, &scope, true);
            let name = if !rt.name.is_empty() {
                rt.name.clone()
            } else {
                match v {
                    N::ColumnRef(c) => names_of(&c.fields).last().cloned().unwrap_or_default(),
                    N::FuncCall(f) => names_of(&f.funcname).last().cloned().unwrap_or_default(),
                    _ => "?column?".to_string(),
                }
            };
            out.push((name, if typ == UNKNOWN { TEXT } else { typ }));
        }
        out
    }

    /// A view's rule, as the rewriter expands it.
    fn view_query(&mut self, sql: &str) {
        let Ok(parsed) = pg_query::parse(sql) else {
            return;
        };
        let Some(N::SelectStmt(sel)) = parsed
            .protobuf
            .stmts
            .first()
            .and_then(|r| r.stmt.as_ref())
            .and_then(|s| s.node.as_ref())
        else {
            return;
        };
        let saved = std::mem::replace(&mut self.stored, true);
        self.select(sel, false);
        self.stored = saved;
    }

    fn need_target(&mut self, rel: i64, att: i64) {
        let n = self.needs.entry(rel).or_default();
        if !n.target.contains(&att) {
            n.target.push(att);
        }
        n.all.insert(att);
    }

    /// A WHERE clause: each conjunct a restriction.
    fn qual(&mut self, n: &N, scope: &Scope) -> i64 {
        let t = self.expr(n, scope, false);
        // `coerce_to_boolean`.
        if t == UNKNOWN {
            self.ptype(BOOL);
        }
        BOOL
    }

    // ----------------------------------------------------------- expressions

    fn expr_n(&mut self, n: &pb::Node, scope: &Scope) -> i64 {
        n.node
            .as_ref()
            .map_or(UNKNOWN, |n| self.expr(n, scope, true))
    }

    /// `transformExpr` and `assign_expr_collations`: the expression's type,
    /// looking up what PostgreSQL looks up for it.
    fn expr(&mut self, n: &N, scope: &Scope, in_target: bool) -> i64 {
        // A nesting deeper than any statement a client writes is walked no
        // further: what it reached is counted.
        if self.depth > MAX_DEPTH {
            return UNKNOWN;
        }
        self.depth += 1;
        let t = self.expr_inner(n, scope, in_target);
        self.depth -= 1;
        t
    }

    fn expr_inner(&mut self, n: &N, scope: &Scope, in_target: bool) -> i64 {
        match n {
            N::AConst(c) => {
                if c.isnull {
                    return UNKNOWN;
                }
                match &c.val {
                    Some(pb::a_const::Val::Ival(_)) => INT4,
                    Some(pb::a_const::Val::Fval(f)) => {
                        if f.fval.parse::<i64>().is_ok() {
                            INT8
                        } else {
                            NUMERIC
                        }
                    }
                    Some(pb::a_const::Val::Boolval(_)) => BOOL,
                    Some(pb::a_const::Val::Sval(_)) => UNKNOWN,
                    Some(pb::a_const::Val::Bsval(_)) => 1560,
                    None => UNKNOWN,
                }
            }
            N::ColumnRef(c) => {
                let names = names_of(&c.fields);
                let Some(col) = names.last().cloned() else {
                    return UNKNOWN;
                };
                for (alias, rel) in &scope.rels {
                    if names.len() > 1 && names[names.len() - 2] != *alias {
                        continue;
                    }
                    if let Some((att, _, typ)) = rel.columns.iter().find(|c| c.1 == col) {
                        let (oid, att, typ) = (rel.oid, *att, *typ);
                        if in_target {
                            self.need_target(oid, att);
                        } else {
                            self.needs.entry(oid).or_default().all.insert(att);
                        }
                        return typ;
                    }
                }
                UNKNOWN
            }
            N::ParamRef(p) => self
                .params
                .get((p.number - 1).max(0) as usize)
                .copied()
                .unwrap_or(UNKNOWN),
            N::TypeCast(tc) => {
                let input = node(&tc.arg).map_or(UNKNOWN, |a| self.expr(a, scope, in_target));
                let Some(target) = tc
                    .type_name
                    .as_ref()
                    .and_then(|t| self.parser(|m| m.type_name(t)))
                else {
                    return UNKNOWN;
                };
                self.coerce(input, target, true);
                target
            }
            N::AExpr(a) => self.a_expr(a, scope, in_target),
            N::BoolExpr(b) => {
                for arg in &b.args {
                    let t = self.expr_n(arg, scope);
                    if t == UNKNOWN {
                        self.ptype(BOOL);
                    }
                }
                self.ptype(BOOL);
                BOOL
            }
            N::NullTest(t) => {
                if let Some(a) = node(&t.arg) {
                    self.expr(a, scope, in_target);
                }
                self.ptype(BOOL);
                BOOL
            }
            N::FuncCall(f) => self.func_call(f, scope, in_target),
            N::CollateClause(c) => node(&c.arg).map_or(UNKNOWN, |a| self.expr(a, scope, in_target)),
            _ => UNKNOWN,
        }
    }

    /// `LookupTypeName` / `typenameType`.
    fn type_name(&mut self, t: &pb::TypeName) -> Option<i64> {
        let names = names_of(&t.names);
        let (schema, name) = match names.as_slice() {
            [n] => (None, n.clone()),
            [s, n] => (Some(s.clone()), n.clone()),
            _ => return None,
        };
        let is_array = !t.array_bounds.is_empty();
        let oid = match schema {
            Some(s) => {
                let nsp = self.explicit_namespace(&s)?;
                let oid = self.h.cr_type_in(nsp, &name);
                self.search(
                    Cache::TypeNameNsp,
                    vec![K::N(name.clone()), K::O(nsp)],
                    |_| oid.is_some(),
                );
                oid?
            }
            None => {
                let mut found = None;
                for nsp in self.active_path() {
                    let oid = self.h.cr_type_in(nsp, &name);
                    self.search(
                        Cache::TypeNameNsp,
                        vec![K::N(name.clone()), K::O(nsp)],
                        |_| oid.is_some(),
                    );
                    if oid.is_some() {
                        found = oid;
                        break;
                    }
                }
                found?
            }
        };
        let oid = if is_array {
            let arr = builtin().types.get(&oid).map_or(0, |t| t.array);
            // `LookupTypeNameExtended` finds the array type by the element's.
            self.ptype(oid);
            arr
        } else {
            oid
        };
        self.ptype(oid);
        Some(oid)
    }

    /// An operator expression: `transformAExprOp` and `make_op`.
    fn a_expr(&mut self, a: &pb::AExpr, scope: &Scope, in_target: bool) -> i64 {
        let kind = a.kind;
        if kind == pb::AExprKind::AexprIn as i32 {
            return self.a_expr_in(a, scope, in_target);
        }
        let l = node(&a.lexpr).map(|e| self.expr(e, scope, in_target));
        let r = node(&a.rexpr).map(|e| self.expr(e, scope, in_target));
        let mut name = names_of(&a.name);
        if kind == pb::AExprKind::AexprLike as i32 {
            name = vec!["~~".to_string()];
        } else if kind == pb::AExprKind::AexprIlike as i32 {
            name = vec!["~~*".to_string()];
        } else if kind != pb::AExprKind::AexprOp as i32 {
            return BOOL;
        }
        let (Some(lt), Some(rt)) = (l, r) else {
            return UNKNOWN;
        };
        let Some(op) = self.parser(|m| m.oper(&name, lt, rt)) else {
            return UNKNOWN;
        };
        // `make_op`: the function's retset, the arguments coerced to the
        // operator's, the node's type for its collation.
        self.proc_oid(op.code);
        self.coerce(lt, op.left, false);
        self.coerce(rt, op.right, false);
        self.ptype(op.result);
        // A restriction `column op constant` the planner estimates.
        if !in_target {
            if self.stored {
                // `restriction_selectivity` reads the operator's estimator.
                self.oper_oid(op.oid);
            }
            self.note_restriction(a, scope, op.oid);
        }
        op.result
    }

    /// `x IN (...)`: `transformAExprIn`, `make_scalar_array_op`, and the
    /// planner's `scalararraysel` and `convert_saop_to_hashed_saop`.
    fn a_expr_in(&mut self, a: &pb::AExpr, scope: &Scope, in_target: bool) -> i64 {
        let lt = node(&a.lexpr).map_or(UNKNOWN, |e| self.expr(e, scope, in_target));
        let Some(N::List(list)) = node(&a.rexpr) else {
            return BOOL;
        };
        let types: Vec<i64> = list.items.iter().map(|i| self.expr_n(i, scope)).collect();
        let name = names_of(&a.name);
        // `select_common_type`: the left side's type leads.
        let common = if lt != UNKNOWN {
            lt
        } else {
            types
                .iter()
                .copied()
                .find(|t| *t != UNKNOWN)
                .unwrap_or(TEXT)
        };
        if types.len() < 2 {
            let rt = types.first().copied().unwrap_or(UNKNOWN);
            let Some(op) = self.parser(|m| m.oper(&name, lt, rt)) else {
                return UNKNOWN;
            };
            self.proc_oid(op.code);
            self.coerce(lt, op.left, false);
            self.coerce(rt, op.right, false);
            self.ptype(op.result);
            return BOOL;
        }
        // `get_array_type`.
        self.ptype(common);
        let array = builtin().types.get(&common).map_or(0, |t| t.array);
        for t in &types {
            self.coerce(*t, common, false);
        }
        let Some(op) = self.parser(|m| m.oper(&name, lt, common)) else {
            return BOOL;
        };
        // `make_scalar_array_op`: the function's retset, the declared right
        // type's array, the arguments coerced, the ArrayExpr's and the
        // node's types for their collations.
        self.proc_oid(op.code);
        self.ptype(op.right);
        self.coerce(lt, op.left, false);
        self.ptype(array);
        self.ptype(BOOL);
        // `convert_saop_to_hashed_saop`: `get_op_hash_functions`.
        self.amop_list(op.oid);
        if let Some(h) = builtin()
            .amop_rows
            .get(&op.oid)
            .and_then(|v| v.iter().find(|r| r.method == HASH_AM && r.strategy == 1))
            .cloned()
        {
            self.opfamily_proc(h.family, h.left, h.left, 1);
        }
        if !in_target {
            if let Some(N::ColumnRef(c)) = node(&a.lexpr) {
                let names = names_of(&c.fields);
                let hit = names.last().and_then(|col| {
                    scope.rels.iter().find_map(|(alias, rel)| {
                        if names.len() > 1 && names[names.len() - 2] != *alias {
                            return None;
                        }
                        rel.columns
                            .iter()
                            .find(|c| &c.1 == col)
                            .map(|c| (rel.oid, c.0))
                    })
                });
                if let Some((rel, att)) = hit {
                    let n = self.needs.entry(rel).or_default();
                    n.saops.push((att, common));
                    n.all.insert(att);
                }
            }
        }
        BOOL
    }

    fn note_restriction(&mut self, a: &pb::AExpr, scope: &Scope, opr: i64) {
        let col = |e: &Option<Box<pb::Node>>| -> Option<(i64, i64)> {
            let N::ColumnRef(c) = node(e)? else {
                return None;
            };
            let names = names_of(&c.fields);
            let col = names.last()?;
            scope.rels.iter().find_map(|(alias, rel)| {
                if names.len() > 1 && names[names.len() - 2] != *alias {
                    return None;
                }
                rel.columns
                    .iter()
                    .find(|c| &c.1 == col)
                    .map(|c| (rel.oid, c.0))
            })
        };
        let konst = |e: &Option<Box<pb::Node>>| -> Option<Bson> {
            match node(e)? {
                N::AConst(c) => match &c.val {
                    Some(pb::a_const::Val::Ival(i)) => Some(Bson::Int64(i64::from(i.ival))),
                    Some(pb::a_const::Val::Sval(s)) => Some(Bson::String(s.sval.clone())),
                    _ => None,
                },
                N::TypeCast(tc) => match node(&tc.arg)? {
                    N::AConst(c) => match &c.val {
                        Some(pb::a_const::Val::Sval(s)) => Some(Bson::String(s.sval.clone())),
                        Some(pb::a_const::Val::Ival(i)) => Some(Bson::Int64(i64::from(i.ival))),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            }
        };
        let (side, value) = match (col(&a.lexpr), col(&a.rexpr)) {
            (Some(c), None) => (Some(c), konst(&a.rexpr)),
            (None, Some(c)) => (Some(c), konst(&a.lexpr)),
            (Some(l), Some(r)) if l.0 != r.0 => {
                self.join_clause(opr, l, r);
                (None, None)
            }
            _ => (None, None),
        };
        if let Some((rel, att)) = side {
            let n = self.needs.entry(rel).or_default();
            n.quals.push((att, opr, value.is_some()));
            n.all.insert(att);
            let is_eq = self.h.cr_operator(opr).is_some_and(|o| o.name == "=");
            if let (true, Some(v)) = (is_eq, value) {
                n.eq_consts.push((att, v));
            }
        }
    }

    /// A join clause between two relations' columns: its operator's merge
    /// join families (`check_mergejoinable`), the type cache's hashing for a
    /// Memoize (`check_memoizable`), each side's statistics (`eqjoinsel`),
    /// and the merge join's scan estimate (`mergejoinscansel`: the family's
    /// `<` and `<=`).
    fn join_clause(&mut self, opr: i64, l: (i64, i64), r: (i64, i64)) {
        for (rel, att) in [l, r] {
            self.needs.entry(rel).or_default().all.insert(att);
            self.needs.entry(rel).or_default().joins.push(att);
        }
        let Some(o) = self.h.cr_operator(opr) else {
            return;
        };
        self.oper_oid(opr);
        self.mergejoinable(opr);
        let lt = self.type_cache(o.left, TC_HASHPROC | TC_EQ);
        if o.right != o.left {
            self.type_cache(o.right, TC_HASHPROC | TC_EQ);
        }
        let _ = lt;
        if o.canmerge {
            if let Some(m) = builtin()
                .amop_rows
                .get(&opr)
                .and_then(|v| v.iter().find(|r| r.method == BTREE_AM && r.strategy == 3))
                .cloned()
            {
                self.opfamily_member(m.family, m.left, m.right, 1);
                self.opfamily_member(m.family, m.left, m.right, 2);
            }
        }
    }

    /// `oper`: the exact match first, then the candidates.
    fn oper(&mut self, name: &[String], lt: i64, rt: i64) -> Option<cf::OperFact> {
        let (schema, opname) = match name {
            [n] => (None, n.clone()),
            [s, n] => (Some(s.clone()), n.clone()),
            _ => return None,
        };
        // `make_oper_cache_key` reads the search path (or the schema).
        let path = match &schema {
            Some(s) => vec![self.explicit_namespace(s)?],
            None => self.active_path(),
        };
        // `binary_oper_exact`.
        let (mut l, mut r) = (lt, rt);
        let mut was_unknown = false;
        if l == UNKNOWN && r != UNKNOWN {
            l = r;
            was_unknown = true;
        } else if r == UNKNOWN && l != UNKNOWN {
            r = l;
            was_unknown = true;
        }
        let exact = self.opername_get_oprid(&schema, &path, &opname, l, r);
        let found = match exact {
            Some(o) => Some(o),
            None => {
                if was_unknown {
                    self.ptype(l);
                }
                // `OpernameGetCandidates` and `oper_select_candidate`.
                let candidates: Vec<cf::OperFact> = self
                    .h
                    .cr_operators_named(&opname)
                    .into_iter()
                    .filter(|o| o.kind == 'b' && path.contains(&o.nsp))
                    .collect();
                let members: Vec<Key> = self
                    .h
                    .cr_operators_named(&opname)
                    .iter()
                    .map(|o| {
                        vec![
                            K::N(o.name.clone()),
                            K::O(o.left),
                            K::O(o.right),
                            K::O(o.nsp),
                        ]
                    })
                    .collect();
                if self.silent == 0 {
                    self.be.search_list(
                        &self.ct,
                        Cache::OperNameNsp,
                        vec![K::N(opname.clone())],
                        || members,
                    );
                }
                let cands: Vec<(i64, Vec<i64>)> = candidates
                    .iter()
                    .map(|o| (o.oid, vec![o.left, o.right]))
                    .collect();
                self.select_candidate(&[lt, rt], cands)
                    .and_then(|oid| candidates.into_iter().find(|o| o.oid == oid))
            }
        }?;
        self.oper_oid(found.oid);
        Some(found)
    }

    /// `OpernameGetOprid`.
    fn opername_get_oprid(
        &mut self,
        schema: &Option<String>,
        path: &[i64],
        name: &str,
        l: i64,
        r: i64,
    ) -> Option<cf::OperFact> {
        let all: Vec<cf::OperFact> = self
            .h
            .cr_operators_named(name)
            .into_iter()
            .filter(|o| o.left == l && o.right == r)
            .collect();
        if schema.is_some() {
            let nsp = path[0];
            let hit = all.iter().find(|o| o.nsp == nsp).cloned();
            self.search(
                Cache::OperNameNsp,
                vec![K::N(name.to_string()), K::O(l), K::O(r), K::O(nsp)],
                |_| hit.is_some(),
            );
            return hit;
        }
        let members: Vec<Key> = all
            .iter()
            .map(|o| vec![K::N(o.name.clone()), K::O(l), K::O(r), K::O(o.nsp)])
            .collect();
        if self.silent == 0 {
            self.be.search_list(
                &self.ct,
                Cache::OperNameNsp,
                vec![K::N(name.to_string()), K::O(l), K::O(r)],
                || members,
            );
        }
        path.iter()
            .find_map(|nsp| all.iter().find(|o| o.nsp == *nsp).cloned())
    }

    /// A function call: `ParseFuncOrColumn` / `func_get_detail`.
    fn func_call(&mut self, f: &pb::FuncCall, scope: &Scope, in_target: bool) -> i64 {
        let names = names_of(&f.funcname);
        let args: Vec<i64> = f
            .args
            .iter()
            .map(|a| {
                a.node
                    .as_ref()
                    .map_or(UNKNOWN, |n| self.expr(n, scope, in_target))
            })
            .collect();
        let argv: Vec<i64> = if f.agg_star { Vec::new() } else { args.clone() };
        let best = self.parser(|m| m.resolve_function(&names, &argv));
        let Some(p) = best else {
            return UNKNOWN;
        };
        self.proc_oid(p.oid);
        if p.kind == 'a' {
            // `transformAggregateCall`, and the planner's
            // `get_agg_clause_costs`: the aggregate's row, its transition
            // and final functions' costs, its state type's width.
            self.search(Cache::AggFnoid, vec![K::O(p.oid)], |_| true);
            if let Some((trans, fin, ttype)) = builtin().aggregates.get(&p.oid).copied() {
                self.proc_oid(trans);
                self.proc_oid(fin);
                self.type_oid(ttype);
            }
        }
        for (a, d) in argv.iter().zip(p.args.iter()) {
            self.coerce(*a, *d, false);
        }
        self.ptype(p.ret);
        // A SQL function's body is parsed when the planner inlines it, or
        // when the executor first calls it.
        let body = if self.active_fns.contains(&p.oid) || self.active_fns.len() > 16 {
            None
        } else {
            self.h.cr_sql_function_body(p.oid)
        };
        if let Some((body, params)) = body {
            self.active_fns.push(p.oid);
            let saved = std::mem::replace(&mut self.params, params);
            if let Ok(parsed) = pg_query::parse(&body) {
                for raw in &parsed.protobuf.stmts {
                    if let Some(s) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
                        let saved_needs = std::mem::take(&mut self.needs);
                        self.statement_in_function(s);
                        self.needs = saved_needs;
                    }
                }
            }
            self.params = saved;
            self.active_fns.pop();
        }
        p.ret
    }

    /// `func_get_detail`: the function the name and argument types select,
    /// with the catalog reads its search makes.
    fn resolve_function(&mut self, names: &[String], argv: &[i64]) -> Option<cf::ProcFact> {
        let (schema, name) = match names {
            [n] => (None, n.clone()),
            [s, n] => (Some(s.clone()), n.clone()),
            _ => return None,
        };
        let path = match &schema {
            Some(s) => vec![self.explicit_namespace(s)?],
            None => self.active_path(),
        };
        // `FuncnameGetCandidates`: every function of the name.
        let all = self.functions_named(&name);
        let members: Vec<Key> = all
            .iter()
            .map(|p| {
                vec![
                    K::N(p.name.clone()),
                    K::N(
                        p.args
                            .iter()
                            .map(i64::to_string)
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    K::O(p.nsp),
                ]
            })
            .collect();
        if self.silent == 0 {
            self.be.search_list(
                &self.ct,
                Cache::ProcNameArgsNsp,
                vec![K::N(name.clone())],
                || members,
            );
        }
        let nargs = argv.len();
        let candidates: Vec<cf::ProcFact> = all
            .into_iter()
            .filter(|p| path.contains(&p.nsp))
            .filter(|p| {
                p.nargs == nargs
                    || (p.variadic != 0 && nargs + 1 >= p.nargs)
                    || (nargs < p.nargs && p.nargs - nargs <= p.ndefaults)
            })
            .collect();
        let exact = candidates
            .iter()
            .find(|p| p.args.len() == argv.len() && p.args == argv)
            .cloned();
        let best = match exact {
            Some(p) => Some(p),
            None => {
                if argv.len() == 1 && schema.is_none() {
                    // `FuncNameAsType`: is the name a type's?
                    for nsp in path.clone() {
                        let oid = self.h.cr_type_in(nsp, &name);
                        self.search(
                            Cache::TypeNameNsp,
                            vec![K::N(name.clone()), K::O(nsp)],
                            |_| oid.is_some(),
                        );
                        if oid.is_some() {
                            break;
                        }
                    }
                }
                let cands: Vec<(i64, Vec<i64>)> = candidates
                    .iter()
                    .filter(|p| p.args.len() == argv.len())
                    .map(|p| (p.oid, p.args.clone()))
                    .collect();
                self.select_candidate(argv, cands)
                    .and_then(|oid| candidates.iter().find(|p| p.oid == oid).cloned())
            }
        };
        best
    }

    /// A SQL function's body: analysed and planned, its result not sent.
    fn statement_in_function(&mut self, s: &N) {
        if let N::SelectStmt(sel) = s {
            if sel.from_clause.is_empty() {
                for t in &sel.target_list {
                    if let Some(N::ResTarget(rt)) = t.node.as_ref() {
                        if let Some(v) = node(&rt.val) {
                            self.expr(v, &Scope::default(), true);
                        }
                    }
                }
                return;
            }
            self.select(sel, false);
        }
    }

    /// `func_match_argtypes` and `func_select_candidate`: the candidate the
    /// arguments select, looking up what the heuristics look up.
    fn select_candidate(&mut self, input: &[i64], cands: Vec<(i64, Vec<i64>)>) -> Option<i64> {
        // `can_coerce_type` against each.
        let mut ok = Vec::new();
        for (oid, args) in cands {
            if self.can_coerce(input, &args) {
                ok.push((oid, args));
            }
        }
        if ok.len() <= 1 {
            return ok.first().map(|c| c.0);
        }
        let nargs = input.len();
        let mut base = Vec::new();
        let mut nunknowns = 0;
        for t in input {
            if *t == UNKNOWN {
                base.push(UNKNOWN);
                nunknowns += 1;
            } else {
                self.ptype(*t);
                base.push(self.h.cr_base_type(*t));
            }
        }
        // Most exact matches.
        let keep = |cands: &Vec<(i64, Vec<i64>)>, score: &dyn Fn(&Vec<i64>) -> usize| {
            let best = cands.iter().map(|c| score(&c.1)).max().unwrap_or(0);
            cands
                .iter()
                .filter(|c| score(&c.1) == best)
                .cloned()
                .collect::<Vec<_>>()
        };
        let exact = |args: &Vec<i64>| {
            (0..nargs)
                .filter(|i| base[*i] != UNKNOWN && args[*i] == base[*i])
                .count()
        };
        let mut c = keep(&ok, &exact);
        if c.len() == 1 {
            return Some(c[0].0);
        }
        // Preferred types in the input's category.
        let cats: Vec<char> = base.iter().map(|t| self.category(*t).0).collect();
        let mut prefs: HashMap<i64, (char, bool)> = HashMap::new();
        for (_, args) in &c {
            for (i, a) in args.iter().enumerate() {
                if base[i] != UNKNOWN {
                    let cp = self.category(*a);
                    prefs.insert(*a, cp);
                }
            }
        }
        let preferred = |args: &Vec<i64>| {
            (0..nargs)
                .filter(|i| {
                    base[*i] != UNKNOWN
                        && (args[*i] == base[*i]
                            || prefs
                                .get(&args[*i])
                                .is_some_and(|(cat, pref)| *pref && *cat == cats[*i]))
                })
                .count()
        };
        c = keep(&c, &preferred);
        if c.len() == 1 || nunknowns == 0 {
            return (c.len() == 1).then(|| c[0].0);
        }
        // The unknown inputs' categories.
        let mut slot: Vec<Option<(char, bool)>> = vec![None; nargs];
        let mut resolved = true;
        for i in 0..nargs {
            if base[i] != UNKNOWN {
                continue;
            }
            let mut cat: Option<(char, bool)> = None;
            let mut conflict = false;
            for (_, args) in &c {
                let (ccat, cpref) = self.category(args[i]);
                match cat {
                    None => cat = Some((ccat, cpref)),
                    Some((sc, sp)) if sc == ccat => cat = Some((sc, sp || cpref)),
                    Some(_) if ccat == 'S' => cat = Some((ccat, cpref)),
                    Some(_) => conflict = true,
                }
            }
            if conflict && cat.map(|c| c.0) != Some('S') {
                resolved = false;
                break;
            }
            slot[i] = cat;
        }
        if resolved {
            let mut kept = Vec::new();
            for cand in &c {
                let mut keepit = true;
                for i in 0..nargs {
                    if base[i] != UNKNOWN {
                        continue;
                    }
                    let (ccat, cpref) = self.category(cand.1[i]);
                    let (scat, spref) = slot[i].unwrap_or((' ', false));
                    if ccat != scat || (spref && !cpref) {
                        keepit = false;
                        break;
                    }
                }
                if keepit {
                    kept.push(cand.clone());
                }
            }
            if !kept.is_empty() {
                c = kept;
            }
            if c.len() == 1 {
                return Some(c[0].0);
            }
        }
        // Last gasp: the known type for the unknowns.
        if nunknowns < nargs {
            let known: HashSet<i64> = base.iter().copied().filter(|t| *t != UNKNOWN).collect();
            if known.len() == 1 {
                let k = *known.iter().next().unwrap_or(&0);
                let full: Vec<i64> = vec![k; nargs];
                let m: Vec<_> = c
                    .iter()
                    .filter(|cand| self_coercible(&full, &cand.1))
                    .cloned()
                    .collect();
                if m.len() == 1 {
                    return Some(m[0].0);
                }
            }
        }
        None
    }

    /// `get_type_category_preferred`.
    fn category(&mut self, t: i64) -> (char, bool) {
        self.ptype(t);
        self.h.cr_type_category(t)
    }

    /// `can_coerce_type`, implicitly.
    fn can_coerce(&mut self, input: &[i64], target: &[i64]) -> bool {
        if input.len() != target.len() {
            return false;
        }
        for (i, t) in input.iter().zip(target) {
            if i == t || *t == cf::ANY || cf::is_polymorphic(*t) || *i == UNKNOWN {
                continue;
            }
            if !self.coercion_pathway(*t, *i, false) {
                return false;
            }
        }
        true
    }

    /// `find_coercion_pathway`: the base types, then `pg_cast`.
    fn coercion_pathway(&mut self, target: i64, source: i64, explicit: bool) -> bool {
        self.ptype(target);
        self.ptype(source);
        let t = self.h.cr_base_type(target);
        let s = self.h.cr_base_type(source);
        if s == t {
            return true;
        }
        let cast = self.h.cr_cast(s, t);
        self.search(Cache::CastSourceTarget, vec![K::O(s), K::O(t)], |_| {
            cast.is_some()
        });
        match cast {
            Some((_, context, _)) => {
                context == 'i' || (explicit && context == 'e') || (explicit && context == 'a')
            }
            None => {
                // CoerceViaIO to or from a string type.
                let tc = self.category(t).0;
                let sc = self.category(s).0;
                tc == 'S' || (explicit && sc == 'S')
            }
        }
    }

    /// `coerce_type`: an untyped literal takes the target's input function;
    /// anything else goes through `pg_cast`.
    fn coerce(&mut self, input: i64, target: i64, explicit: bool) {
        if input == target || target == cf::ANY || cf::is_polymorphic(target) || target == 0 {
            return;
        }
        if input == UNKNOWN {
            self.ptype(target);
            return;
        }
        if self.parser(|m| m.coercion_pathway(target, input, explicit)) {
            if let Some((func, _, _)) = self.h.cr_cast(input, target) {
                if func != 0 {
                    // `build_coercion_expression`.
                    self.proc_oid(func);
                }
            }
            // The coercion node's type, for its collation.
            self.ptype(target);
        }
    }
}

fn self_coercible(input: &[i64], target: &[i64]) -> bool {
    input
        .iter()
        .zip(target)
        .all(|(i, t)| i == t || cf::is_polymorphic(*t) || *t == cf::ANY)
}

/// A catalog index as the model reads it.
fn catalog_index(oid: i64) -> Option<Index> {
    let ix = builtin().indexes.get(&oid)?;
    let rel = builtin().relations.get(&oid)?;
    Some(Index {
        oid,
        natts: ix.natts,
        keys: ix.keys.clone(),
        classes: ix.classes.clone(),
        am: rel.relam,
        in_init_file: rel.in_init_file,
    })
}

// ---------------------------------------------------------------- the facts

impl PgHandler {
    pub(crate) fn cr_current_user(&self) -> String {
        self.session_user_name()
    }

    fn virtual_rows_for(&self, name: &str) -> Option<Vec<bson::Document>> {
        // Uncached: the statement's own read of the catalog, in its
        // transaction, must not find rows read before it began.
        self.virtual_rows_uncached(name, &bson::Document::new())
    }

    fn catalog_def(&self, name: &str) -> Option<secantus_pgcatalog::TableDef> {
        Self::virtual_table(name)
    }

    pub(crate) fn cr_is_superuser(&self, user: &str) -> bool {
        self.is_superuser(user)
    }

    pub(crate) fn cr_search_path_setting(&self) -> String {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get("search_path")
            .cloned()
            .unwrap_or_else(|| "\"$user\", public".to_string())
    }

    /// A schema's oid by name.
    pub(crate) fn cr_namespace_oid(&self, name: &str) -> Option<i64> {
        match name {
            "pg_catalog" => return Some(PG_CATALOG),
            "public" => return Some(PUBLIC),
            "pg_toast" => return Some(99),
            "information_schema" => return Some(13679),
            _ => {}
        }
        if !secantus_pgplan::schemas::schema_exists(name) {
            return None;
        }
        self.namespaces()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, o)| o)
    }

    /// The four `ApplySetting` scans' rows: (database, role), (0, role),
    /// (database, 0), (0, 0).
    pub(crate) fn cr_role_settings(&self, db: i64, role: i64) -> Vec<i64> {
        let Some(def) = Self::virtual_table("pg_db_role_setting") else {
            return vec![0; 4];
        };
        let rows = self.system_catalog_rows("pg_db_role_setting", &def);
        let field = |n: &str| def.field_of(n).unwrap_or_default();
        let (fd, fr) = (field("setdatabase"), field("setrole"));
        let count = |d: i64, r: i64| {
            rows.iter()
                .filter(|row| {
                    row.get(&fd).and_then(crate::bson_i64) == Some(d)
                        && row.get(&fr).and_then(crate::bson_i64) == Some(r)
                })
                .count() as i64
        };
        vec![count(db, role), count(0, role), count(db, 0), count(0, 0)]
    }

    /// A catalog relation by oid.
    pub(crate) fn cr_catalog(&self, oid: i64) -> Option<Rel> {
        let c = builtin().relations.get(&oid)?;
        let mut rel = Rel {
            oid,
            name: c.name.clone(),
            nsp: if c.nsp == "pg_catalog" {
                PG_CATALOG
            } else {
                self.cr_namespace_oid(&c.nsp).unwrap_or(0)
            },
            kind: c.kind,
            catalog: true,
            in_init_file: c.in_init_file,
            natts: c.natts,
            measured: c.analyzed,
            tablespace: c.tablespace,
            ..Rel::default()
        };
        if let Some(cols) = crate::catalog_meta::system_relation_columns(&c.name) {
            rel.columns = cols
                .into_iter()
                .enumerate()
                .map(|(i, (n, t, _))| (i as i64 + 1, n, t))
                .collect();
        }
        if c.has_stats {
            rel.stats = rel.columns.iter().map(|c| c.0).collect();
        }
        rel.indexes = builtin()
            .indexes_of
            .get(&oid)
            .map(|v| v.iter().filter_map(|i| catalog_index(*i)).collect())
            .unwrap_or_default();
        Some(rel)
    }

    /// The relation `name` in schema `nsp`.
    pub(crate) fn cr_relation_in(&self, nsp: i64, name: &str) -> Option<Rel> {
        let b = builtin();
        if nsp == PG_CATALOG {
            let oid = *b
                .relation_by_name
                .get(&("pg_catalog".to_string(), name.to_string()))?;
            return self.cr_catalog(oid);
        }
        if nsp == 13679 {
            let oid = *b
                .relation_by_name
                .get(&("information_schema".to_string(), name.to_string()))?;
            return self.cr_catalog(oid);
        }
        let schema = if nsp == PUBLIC {
            "public".to_string()
        } else {
            self.namespaces()
                .into_iter()
                .find(|(_, o)| *o == nsp)
                .map(|(n, _)| n)?
        };
        let key = if schema == "public" {
            name.to_string()
        } else {
            format!("{schema}.{name}")
        };
        if let Some(view) = self.cr_view(&key, name, nsp) {
            return Some(view);
        }
        let defs = self.all_table_defs().ok()?;
        if let Some(def) = defs.iter().find(|d| d.name == key) {
            let oid = self.relation_oid(&key)?;
            let mut columns = Vec::new();
            for (i, c) in def.columns.iter().enumerate() {
                let typ = self.cr_type_named(&c.pg_type);
                columns.push((i64::from(def.attnum(i)), c.name.clone(), typ));
            }
            let natts = (def.columns.len() + def.dropped_attnums().len()) as i64;
            let relstats = self.relstats(&key);
            let pages = relstats
                .as_ref()
                .and_then(|d| d.get("relpages").and_then(crate::bson_i64))
                .unwrap_or(0);
            let tuples = relstats
                .as_ref()
                .and_then(|d| match d.get("reltuples") {
                    Some(Bson::Double(f)) => Some(*f),
                    Some(v) => crate::bson_i64(v).map(|n| n as f64),
                    None => None,
                })
                .unwrap_or(-1.0);
            let stats: HashSet<i64> = relstats
                .as_ref()
                .and_then(|d| d.get_document("columns").ok())
                .map(|cols| {
                    columns
                        .iter()
                        .filter(|(_, n, _)| {
                            def.column(n).is_some_and(|c| cols.contains_key(c.field()))
                        })
                        .map(|c| c.0)
                        .collect()
                })
                .unwrap_or_default();
            let indexes = self
                .index_relations()
                .into_iter()
                .filter(|ix| ix.table.name == key && !ix.exclusion)
                .map(|ix| {
                    let keys: Vec<i64> = ix.keys.iter().map(|k| i64::from(*k)).collect();
                    let classes = ix
                        .keys
                        .iter()
                        .map(|k| {
                            let typ = columns
                                .iter()
                                .find(|c| c.0 == i64::from(*k))
                                .map_or(0, |c| c.2);
                            default_opclass(typ, BTREE_AM)
                        })
                        .collect();
                    Index {
                        oid: ix.oid,
                        natts: (ix.keys.len() + ix.include.len()) as i64,
                        keys,
                        classes,
                        am: BTREE_AM,
                        in_init_file: false,
                    }
                })
                .collect();
            let defaults = def
                .columns
                .iter()
                .filter(|c| c.default.is_some() || c.sequence.is_some())
                .count() as i64;
            return Some(Rel {
                oid,
                name: name.to_string(),
                nsp,
                kind: 'r',
                catalog: false,
                in_init_file: false,
                natts,
                columns,
                measured: pages > 0 && tuples >= 0.0,
                stats,
                indexes,
                tablespace: 0,
                defaults,
                checks: def.check_constraints.len() as i64,
                view: None,
            });
        }
        None
    }

    /// A user view: its rule's query, and its columns as that query
    /// yields them.
    fn cr_view(&self, key: &str, name: &str, nsp: i64) -> Option<Rel> {
        let views = self.views_shared().ok()?;
        let (_, sql) = views.iter().find(|(n, _)| n == key)?;
        let oid = self
            .relation_oid(key)
            .unwrap_or_else(|| self.view_relid(key));
        let mut be = Backend::default();
        let mut m = Model::new(
            self,
            &mut be,
            Counter {
                stats: None,
                store: 0,
            },
        );
        m.stored = true;
        m.silent = 1;
        let parsed = pg_query::parse(sql).ok()?;
        let cols = match parsed
            .protobuf
            .stmts
            .first()
            .and_then(|r| r.stmt.as_ref())
            .and_then(|s| s.node.as_ref())
        {
            Some(N::SelectStmt(sel)) => m.target_columns(sel),
            _ => Vec::new(),
        };
        let columns: Vec<(i64, String, i64)> = cols
            .into_iter()
            .enumerate()
            .map(|(i, (n, t))| (i as i64 + 1, n, t))
            .collect();
        Some(Rel {
            oid,
            name: name.to_string(),
            nsp,
            kind: 'v',
            natts: columns.len() as i64,
            columns,
            view: Some(sql.clone()),
            ..Rel::default()
        })
    }

    /// A column type's oid from its name.
    fn cr_type_named(&self, name: &str) -> i64 {
        let bare = name.split('(').next().unwrap_or(name).trim();
        let b = builtin();
        if let Some(o) = b.type_by_name.get(&(bare.to_string(), PG_CATALOG)) {
            return *o;
        }
        if let Some(o) = secantus_pgplan::pgtypes::oid_of_name(bare) {
            return o;
        }
        self.type_oid_by_name(bare).unwrap_or(0)
    }

    /// The type `name` in schema `nsp`.
    pub(crate) fn cr_type_in(&self, nsp: i64, name: &str) -> Option<i64> {
        if nsp == PG_CATALOG {
            return builtin()
                .type_by_name
                .get(&(name.to_string(), PG_CATALOG))
                .copied();
        }
        if nsp == PUBLIC {
            if let Some(o) = self.relation_oid(name) {
                return Some(o);
            }
            return self.type_oid_by_name(name).filter(|o| *o >= 16384);
        }
        None
    }

    pub(crate) fn cr_base_type(&self, t: i64) -> i64 {
        match builtin().types.get(&t) {
            Some(f) if f.typtype == 'd' && f.basetype != 0 => f.basetype,
            Some(_) => t,
            None => t,
        }
    }

    pub(crate) fn cr_type_category(&self, t: i64) -> (char, bool) {
        if let Some(f) = builtin().types.get(&t) {
            return (f.category, f.preferred);
        }
        ('U', false)
    }

    pub(crate) fn cr_cast(&self, s: i64, t: i64) -> Option<(i64, char, char)> {
        builtin().casts.get(&(s, t)).copied()
    }

    pub(crate) fn cr_operator(&self, oid: i64) -> Option<cf::OperFact> {
        builtin().opers.get(&oid).cloned()
    }

    pub(crate) fn cr_operators_named(&self, name: &str) -> Vec<cf::OperFact> {
        let b = builtin();
        b.opers_by_name
            .get(name)
            .map(|v| v.iter().filter_map(|o| b.opers.get(o).cloned()).collect())
            .unwrap_or_default()
    }

    /// Every function of the name: PostgreSQL's, then the user's.
    pub(crate) fn cr_functions_named(&self, name: &str) -> Vec<cf::ProcFact> {
        let b = builtin();
        let mut out: Vec<cf::ProcFact> = b
            .procs_by_name
            .get(name)
            .map(|v| v.iter().filter_map(|o| b.procs.get(o).cloned()).collect())
            .unwrap_or_default();
        if let Ok(fns) = self.functions() {
            for f in fns.into_iter().filter(|f| f.name == name) {
                let args: Vec<i64> = f
                    .param_types
                    .iter()
                    .map(|t| self.cr_type_named(t))
                    .collect();
                out.push(cf::ProcFact {
                    oid: self.function_oid(&f.key),
                    name: f.name.clone(),
                    nsp: self.cr_namespace_oid(&f.schema).unwrap_or(PUBLIC),
                    kind: if f.is_procedure { 'p' } else { 'f' },
                    nargs: args.len(),
                    args,
                    ret: self.cr_type_named(&f.return_type),
                    ..cf::ProcFact::default()
                });
            }
        }
        out
    }

    /// A user SQL function's body and parameter types.
    pub(crate) fn cr_sql_function_body(&self, oid: i64) -> Option<(String, Vec<i64>)> {
        if oid < 16384 {
            return None;
        }
        let docs = self.type_catalog_docs(Self::FUNCTION_COLLECTION).ok()?;
        let d = docs.iter().find(|d| {
            d.get("oid").and_then(crate::bson_i64) == Some(oid)
                || self.function_oid(d.get_str("_id").unwrap_or_default()) == oid
        })?;
        if d.get_str("language").unwrap_or_default() != "sql" {
            return None;
        }
        let params = d
            .get_array("param_types")
            .map(|a| {
                a.iter()
                    .map(|v| self.cr_type_named(v.as_str().unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        Some((d.get_str("body").unwrap_or_default().to_string(), params))
    }

    /// What the type cache finds for `typ`, without reading anything.
    pub(crate) fn cr_type_ops(&self, typ: i64) -> TypeOps {
        let b = builtin();
        let base = self.cr_base_type(typ);
        let pick = |am: i64| -> Option<(i64, i64)> {
            let classes = b.opclasses_by_am.get(&am)?;
            let all: Vec<&cf::OpclassFact> = classes
                .iter()
                .filter_map(|o| b.opclasses.get(o))
                .filter(|o| o.default)
                .collect();
            all.iter()
                .find(|o| o.intype == base)
                .or_else(|| {
                    all.iter().find(|o| {
                        b.casts
                            .get(&(base, o.intype))
                            .is_some_and(|(_, c, m)| *c == 'i' && *m == 'b')
                    })
                })
                .map(|o| (o.family, o.intype))
        };
        let btree = pick(BTREE_AM);
        let hash = pick(HASH_AM);
        let member = |fs: Option<(i64, i64)>, s: i64| {
            fs.and_then(|(f, t)| b.amop_strategy.get(&(f, t, t, s)).copied())
                .unwrap_or(0)
        };
        let mut eq = member(btree, 3);
        if eq == 0 {
            eq = member(hash, 1);
        }
        TypeOps {
            btree,
            hash,
            eq,
            lt: member(btree, 1),
            gt: member(btree, 5),
        }
    }

    pub(crate) fn cr_opclass(&self, oid: i64) -> Option<cf::OpclassFact> {
        builtin().opclasses.get(&oid).cloned()
    }

    /// How many extended statistics objects are on `rel`.
    pub(crate) fn cr_statext_count(&self, rel: i64) -> i64 {
        if rel < 16384 {
            return 0;
        }
        let Some(def) = Self::virtual_table("pg_statistic_ext") else {
            return 0;
        };
        let Some(rows) = self.virtual_rows_for("pg_statistic_ext") else {
            return 0;
        };
        let f = def.field_of("stxrelid").unwrap_or_default();
        rows.iter()
            .filter(|r| r.get(&f).and_then(crate::bson_i64) == Some(rel))
            .count() as i64
    }

    /// How many rows a catalog holds: what a sequential scan returns.
    pub(crate) fn cr_catalog_rows(&self, rel: &Rel) -> i64 {
        self.virtual_rows_for(&rel.name)
            .map_or(0, |rows| rows.len() as i64)
    }

    /// How many of a catalog's rows an index scan's equality on the index's
    /// leading columns returns.
    pub(crate) fn cr_catalog_matches(&self, rel: &Rel, ix: &Index, eq: &[(i64, Bson)]) -> i64 {
        let Some(rows) = self.virtual_rows_for(&rel.name) else {
            return 0;
        };
        let def = self.catalog_def(&rel.name);
        let mut conds = Vec::new();
        for k in &ix.keys {
            match eq.iter().find(|(a, _)| a == k) {
                Some((_, v)) => {
                    let col = rel.columns.iter().find(|c| c.0 == *k).map(|c| c.1.clone());
                    let field = col
                        .as_deref()
                        .and_then(|c| def.as_ref().and_then(|d| d.field_of(c)))
                        .unwrap_or_default();
                    conds.push((field, v.clone()));
                }
                None => break,
            }
        }
        rows.iter()
            .filter(|r| {
                conds.iter().all(|(f, v)| match (r.get(f), v) {
                    (Some(Bson::String(a)), Bson::String(b)) => a == b,
                    (Some(a), Bson::Int64(b)) => crate::bson_i64(a) == Some(*b),
                    _ => false,
                })
            })
            .count() as i64
    }
}

/// `GetDefaultOpClass`, for a column's index: the exact type's default,
/// else a binary-coercible one's.
fn default_opclass(typ: i64, am: i64) -> i64 {
    let b = builtin();
    let classes = b.opclasses_by_am.get(&am).cloned().unwrap_or_default();
    classes
        .iter()
        .filter_map(|c| b.opclasses.get(c))
        .find(|o| o.default && o.intype == typ)
        .or_else(|| {
            classes.iter().filter_map(|c| b.opclasses.get(c)).find(|o| {
                o.default
                    && b.casts
                        .get(&(typ, o.intype))
                        .is_some_and(|(_, ctx, m)| *ctx == 'i' && *m == 'b')
            })
        })
        .map_or(0, |o| o.oid)
}

// ------------------------------------------------------------ the session

/// Whether a statement's first word makes it one whose catalog changes this
/// model sends invalidations for: it is parsed every time, not remembered.
fn changes_catalogs(sql: &str) -> bool {
    let first = sql
        .trim_start()
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "create"
            | "alter"
            | "drop"
            | "vacuum"
            | "analyze"
            | "analyse"
            | "comment"
            | "grant"
            | "revoke"
            | "truncate"
            | "reindex"
            | "cluster"
    )
}

impl PgHandler {
    fn catcache_counter(&self) -> Counter {
        Counter {
            stats: self.tracking().then(|| self.stats()),
            store: std::sync::Arc::as_ptr(&self.storage) as usize,
        }
    }

    fn catcache_ids(&self) -> (usize, i32) {
        (
            std::sync::Arc::as_ptr(&self.storage) as usize,
            self.backend_pid.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// A session began: `InitPostgres`'s reads.
    pub(crate) fn catalog_reads_start(&self) {
        let (store, _) = self.catcache_ids();
        let ct = self.catcache_counter();
        let db_oid = self.stats().db;
        let user = self.session_user_name();
        let role_oid = self.role(&user).ok().flatten().map_or(10, |r| r.oid);
        let db = self.db().to_string();
        let mut be = self.catcache.lock().unwrap_or_else(|e| e.into_inner());
        be.attach(store);
        Model::new(self, &mut be, ct).backend_start(db_oid, &db, role_oid);
    }

    /// What a statement that changes the catalogs will leave if it
    /// succeeds, read from the catalogs as they stand before it runs.
    pub(crate) fn catalog_effects(&self, sql: &str) -> Effects {
        let mut effects = Effects::default();
        if self.catcache_ids().1 == 0 || !changes_catalogs(sql) {
            return effects;
        }
        let Ok(parsed) = pg_query::parse(sql) else {
            return effects;
        };
        for raw in &parsed.protobuf.stmts {
            if let Some(stmt) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
                let (invals, dead) = ddl_effects(self, stmt);
                effects.invals.extend(invals);
                effects.dead.extend(dead);
            }
        }
        effects
    }

    /// The catalog reads of one statement. Walked once it has run: the
    /// catalog facts are read in the session's state as the statement left
    /// it, never ahead of what the statement itself sets up (a catalog read
    /// before the statement ran once cached rows it then answered from).
    pub(crate) fn catalog_reads(&self, sql: &str) {
        let (store, pid) = self.catcache_ids();
        // A statement this long is not parsed a second time to be walked:
        // its catalog reads go uncounted (RP-290).
        if pid == 0 || sql.len() > MAX_WALKED_TEXT {
            return;
        }
        let ct = self.catcache_counter();
        let version = crate::catalog_cache()
            .version
            .load(std::sync::atomic::Ordering::SeqCst);
        // The statement as this session would resolve it: its text, under
        // the search path and the role (`recomputeNamespacePath`'s inputs).
        let path = self.cr_search_path_setting();
        let user = self.session_user_name();
        let key = text_key(sql, &path, &user);
        let ddl = changes_catalogs(sql);
        let mut be = self.catcache.lock().unwrap_or_else(|e| e.into_inner());
        // `AcceptInvalidationMessages`, at the statement's start.
        be.accept(store, pid);
        if !ddl {
            if let Some(m) = be.memo(key, version) {
                for s in m.scans.clone() {
                    ct.scan(s);
                }
                return;
            }
        }
        // The same statement with other literals of the same kinds reads
        // what it read, unless it scans a catalog by those values.
        let shape = if ddl {
            None
        } else {
            literal_shape(sql).map(|n| text_key(&n, &path, &user))
        };
        if let Some(shape) = shape {
            if be.memo(shape, version).is_some_and(|m| m.scans.is_empty()) {
                be.remember(key, version, Vec::new());
                return;
            }
        }
        let Ok(parsed) = pg_query::parse(sql) else {
            return;
        };
        let mut scans = Vec::new();
        for raw in &parsed.protobuf.stmts {
            let Some(stmt) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
                continue;
            };
            let mut m = Model::new(self, &mut be, self.catcache_counter());
            m.statement(stmt);
            scans.extend(m.exec);
        }
        for s in &scans {
            ct.scan(*s);
        }
        if !ddl {
            if let (Some(shape), true) = (shape, scans.is_empty()) {
                be.remember(shape, version, Vec::new());
            }
            be.remember(key, version, scans);
        }
    }

    /// The statement ended: a catalog change it made reaches this backend's
    /// caches now, the others' when its transaction commits.
    pub(crate) fn catalog_reads_done(&self, effects: Effects, ok: bool) {
        if !ok || (effects.invals.is_empty() && effects.dead.is_empty()) {
            return;
        }
        let mut be = self.catcache.lock().unwrap_or_else(|e| e.into_inner());
        be.local(effects.invals);
        be.left_dead(effects.dead);
    }

    /// The transaction ended.
    pub(crate) fn catcache_end_xact(&self, commit: bool) {
        let (store, pid) = self.catcache_ids();
        self.catcache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .end_xact(store, pid, commit);
    }
}

/// What a statement's catalog changes leave: the invalidations it sends,
/// and the catalog index entries of the rows it deleted or updated out of
/// place.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub(crate) invals: Vec<Inval>,
    pub(crate) dead: Vec<(i64, Key)>,
}

/// A statement's text with each literal replaced by a representative of
/// its kind -- a string, an int4, an int8, a numeric, a decimal -- for the
/// memo: what the walk reads depends on a literal's type, not its value.
/// `None` for a text whose literals this cannot place: a dollar quote, an
/// escape or bit string, a comment, a parameter, or ORDER BY / GROUP BY,
/// where a number names a column.
fn literal_shape(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    if lower.contains("order by")
        || lower.contains("group by")
        || sql.contains('$')
        || sql.contains("--")
        || sql.contains("/*")
        || sql.contains('"')
    {
        return None;
    }
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut changed = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' {
            // A string prefixed by E, B, X, U& or N is not plain.
            if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'&' || b[i - 1] == b'_') {
                return None;
            }
            let mut j = i + 1;
            loop {
                if j >= b.len() {
                    return None;
                }
                if b[j] == b'\'' {
                    if j + 1 < b.len() && b[j + 1] == b'\'' {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            out.push_str("'s'");
            i = j + 1;
            changed = true;
        } else if c.is_ascii_digit()
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.'))
        {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && (b[j] == b'.' || b[j] == b'e' || b[j] == b'E') {
                return None;
            }
            if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
                return None;
            }
            let digits = &sql[i..j];
            let kind = match digits.parse::<i64>() {
                Ok(v) if i32::try_from(v).is_ok() => "1",
                Ok(_) => "4294967296",
                Err(_) => "99999999999999999999",
            };
            out.push_str(kind);
            i = j;
            changed = true;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    (changed && sql.is_ascii()).then_some(out)
}

/// A statement's text, hashed, for the memo.
pub(crate) fn text_key(sql: &str, path: &str, user: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sql.hash(&mut h);
    path.hash(&mut h);
    user.hash(&mut h);
    h.finish()
}

/// The invalidations a DDL statement sends, from its parse tree, given the
/// relation it names as it stood before.
pub(crate) fn ddl_effects(h: &PgHandler, stmt: &N) -> (Vec<Inval>, Vec<(i64, Key)>) {
    let mut out = Vec::new();
    let mut dead: Vec<(i64, Key)> = Vec::new();
    let rel_of = |rv: &pb::RangeVar| -> Option<Rel> {
        let nsp = if rv.schemaname.is_empty() {
            PUBLIC
        } else {
            h.cr_namespace_oid(&rv.schemaname)?
        };
        h.cr_relation_in(nsp, &rv.relname)
    };
    let pg_class_row = |out: &mut Vec<Inval>, rel: &Rel| {
        out.push(Inval::Cat(Cache::RelOid, vec![K::O(rel.oid)]));
        out.push(Inval::Cat(
            Cache::RelNameNsp,
            vec![K::N(rel.name.clone()), K::O(rel.nsp)],
        ));
    };
    match stmt {
        N::AlterTableStmt(a) => {
            let Some(rv) = a.relation.as_ref() else {
                return (out, dead);
            };
            let Some(rel) = rel_of(rv) else {
                return (out, dead);
            };
            out.push(Inval::Rel(rel.oid));
            for c in &a.cmds {
                let Some(N::AlterTableCmd(cmd)) = c.node.as_ref() else {
                    continue;
                };
                if cmd.subtype == pb::AlterTableType::AtAddColumn as i32 {
                    // `relnatts` changes: the table's pg_class row; a new
                    // attribute row.
                    pg_class_row(&mut out, &rel);
                    out.push(Inval::Cat(
                        Cache::AttNum,
                        vec![K::O(rel.oid), K::O(rel.natts + 1)],
                    ));
                } else if cmd.subtype == pb::AlterTableType::AtDropColumn as i32 {
                    if let Some((att, _, _)) = rel.columns.iter().find(|c| c.1 == cmd.name) {
                        out.push(Inval::Cat(Cache::AttNum, vec![K::O(rel.oid), K::O(*att)]));
                        out.push(Inval::Cat(
                            Cache::AttName,
                            vec![K::O(rel.oid), K::N(cmd.name.clone())],
                        ));
                        // The attribute's row is updated with a new name, out
                        // of place: its old index entries are dead.
                        dead.push((2659, vec![K::O(rel.oid), K::O(*att)]));
                        dead.push((2658, vec![K::O(rel.oid), K::N(cmd.name.clone())]));
                    }
                } else {
                    pg_class_row(&mut out, &rel);
                    out.push(Inval::CatPrefix(Cache::AttNum, vec![K::O(rel.oid)]));
                }
            }
        }
        N::CreateStmt(c) => {
            if let Some(rv) = c.relation.as_ref() {
                let nsp = if rv.schemaname.is_empty() {
                    PUBLIC
                } else {
                    h.cr_namespace_oid(&rv.schemaname).unwrap_or(PUBLIC)
                };
                out.push(Inval::Cat(
                    Cache::RelNameNsp,
                    vec![K::N(rv.relname.clone()), K::O(nsp)],
                ));
                out.push(Inval::Cat(
                    Cache::TypeNameNsp,
                    vec![K::N(rv.relname.clone()), K::O(nsp)],
                ));
            }
        }
        N::DropStmt(d) => {
            for o in &d.objects {
                let Some(N::List(l)) = o.node.as_ref() else {
                    continue;
                };
                let names = names_of(&l.items);
                let (schema, name) = match names.as_slice() {
                    [n] => (String::new(), n.clone()),
                    [s, n] => (s.clone(), n.clone()),
                    _ => continue,
                };
                let rv = pb::RangeVar {
                    schemaname: schema,
                    relname: name,
                    ..Default::default()
                };
                if let Some(rel) = rel_of(&rv) {
                    out.push(Inval::Rel(rel.oid));
                    pg_class_row(&mut out, &rel);
                    out.push(Inval::CatPrefix(Cache::AttNum, vec![K::O(rel.oid)]));
                    out.push(Inval::CatPrefix(Cache::StatRelAttInh, vec![K::O(rel.oid)]));
                    // Its rows are deleted: their index entries are dead.
                    dead.push((2662, vec![K::O(rel.oid)]));
                    dead.push((2663, vec![K::N(rel.name.clone()), K::O(rel.nsp)]));
                    for k in 1..=rel.natts {
                        dead.push((2659, vec![K::O(rel.oid), K::O(k)]));
                    }
                    for (_, name, _) in &rel.columns {
                        dead.push((2658, vec![K::O(rel.oid), K::N(name.clone())]));
                    }
                    if rel.kind != 'v' || rel.view.is_some() {
                        dead.push((2704, vec![K::N(rel.name.clone()), K::O(rel.nsp)]));
                    }
                    if rel.kind == 'r' {
                        dead.push((2704, vec![K::N(format!("_{}", rel.name)), K::O(rel.nsp)]));
                    }
                    for ix in &rel.indexes {
                        out.push(Inval::Rel(ix.oid));
                        dead.push((2662, vec![K::O(ix.oid)]));
                        dead.push((2679, vec![K::O(ix.oid)]));
                        dead.push((2678, vec![K::O(rel.oid)]));
                        for k in 1..=ix.natts {
                            dead.push((2659, vec![K::O(ix.oid), K::O(k)]));
                        }
                    }
                }
            }
        }
        N::IndexStmt(i) => {
            if let Some(rel) = i.relation.as_ref().and_then(rel_of) {
                out.push(Inval::Rel(rel.oid));
                pg_class_row(&mut out, &rel);
            }
        }
        N::VacuumStmt(v) => {
            for r in &v.rels {
                let Some(N::VacuumRelation(vr)) = r.node.as_ref() else {
                    continue;
                };
                if let Some(rel) = vr.relation.as_ref().and_then(rel_of) {
                    out.push(Inval::Rel(rel.oid));
                    pg_class_row(&mut out, &rel);
                    out.push(Inval::CatPrefix(Cache::StatRelAttInh, vec![K::O(rel.oid)]));
                }
            }
        }
        N::CreateFunctionStmt(f) => {
            let names = names_of(&f.funcname);
            if let Some(name) = names.last() {
                out.push(Inval::Cat(Cache::ProcNameArgsNsp, vec![K::N(name.clone())]));
            }
        }
        _ => {}
    }
    (out, dead)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_s_kind_is_its_shape() {
        let a = literal_shape("select v from t where id = 7 and n = 'a''b'");
        let b = literal_shape("select v from t where id = 12 and n = 'zz'");
        assert_eq!(a, b);
        assert_eq!(
            a.as_deref(),
            Some("select v from t where id = 1 and n = 's'")
        );
        // int4, int8 and numeric literals are different types.
        assert_ne!(
            literal_shape("select 1"),
            literal_shape("select 5000000000")
        );
        assert_eq!(literal_shape("select t1.a from t1"), None);
        // A number naming a column, a decimal, an escape string: no shape.
        assert_eq!(literal_shape("select a from t order by 1"), None);
        assert_eq!(literal_shape("select 1.5"), None);
        assert_eq!(literal_shape("select E'x'"), None);
    }
}
