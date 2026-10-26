//! A rule's node trees (RP-203): the catalog the analysis reads, handed to
//! `secantus_pgplan::nodetostring` from the server's own catalogs, and the
//! system views' trees, analysed once per process.

use super::*;
use secantus_pgplan::nodetostring::catalog::*;
use secantus_pgplan::nodetostring::{Catalog, CatalogData};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// The system rules' trees, by (relation oid, rule name): `(ev_qual,
/// ev_action)`, or nothing where the analysis refused.
type SystemTrees = HashMap<(i64, String), (String, String)>;

fn text_rows(h: &PgHandler, sql: &str) -> PgWireResult<Vec<Vec<Option<String>>>> {
    let (_, rows) = h.internal_query(sql)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|v| match v {
                    None | Some(Bson::Null) => None,
                    Some(Bson::String(s)) => Some(s),
                    Some(Bson::Boolean(b)) => Some(if b { "true" } else { "false" }.to_string()),
                    Some(Bson::Int32(i)) => Some(i.to_string()),
                    Some(Bson::Int64(i)) => Some(i.to_string()),
                    Some(other) => Some(other.to_string()),
                })
                .collect()
        })
        .collect())
}

fn oid(v: &Option<String>) -> u32 {
    v.as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map_or(0, |n| n as u32)
}

fn int(v: &Option<String>) -> i32 {
    v.as_deref()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

fn boolean(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("true" | "t"))
}

fn chr(v: &Option<String>) -> char {
    v.as_deref().and_then(|s| s.chars().next()).unwrap_or('\0')
}

fn string(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

/// A one-dimensional array's text form, element by element.
fn array_items(v: &Option<String>) -> Option<Vec<String>> {
    let s = v.as_deref()?;
    let inner = s.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut out = Vec::new();
    if inner.is_empty() {
        return Some(out);
    }
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    Some(out)
}

impl PgHandler {
    /// The catalog rows an analysis reads. `system_only` keeps PostgreSQL's
    /// own objects (oid below 16384), what `initdb` saw.
    pub(crate) fn rule_catalog_data(&self, system_only: bool) -> PgWireResult<CatalogData> {
        let keep = |o: u32| !system_only || o < 16384;
        let mut d = CatalogData::default();
        for r in text_rows(self, "select oid::text, nspname::text from pg_namespace")? {
            if keep(oid(&r[0])) {
                d.namespaces.push((oid(&r[0]), string(&r[1])));
            }
        }
        for r in text_rows(
            self,
            "select oid::text, relname::text, relnamespace::text, relkind::text, \
             reltype::text, reloftype::text from pg_class",
        )? {
            if keep(oid(&r[0])) {
                d.relations.push(RelRow {
                    oid: oid(&r[0]),
                    name: string(&r[1]),
                    nsp: oid(&r[2]),
                    kind: chr(&r[3]),
                    reltype: oid(&r[4]),
                    reloftype: oid(&r[5]),
                });
            }
        }
        for r in text_rows(
            self,
            "select attrelid::text, attnum::text, attname::text, atttypid::text, \
             atttypmod::text, attcollation::text, attisdropped::text from pg_attribute",
        )? {
            if keep(oid(&r[0])) {
                d.attributes.push(AttRow {
                    relid: oid(&r[0]),
                    num: int(&r[1]),
                    name: string(&r[2]),
                    typid: oid(&r[3]),
                    typmod: int(&r[4]),
                    collation: oid(&r[5]),
                    dropped: boolean(&r[6]),
                });
            }
        }
        for r in text_rows(
            self,
            "select oid::text, typname::text, typnamespace::text, typlen::text, \
             typbyval::text, typalign::text, typtype::text, typcategory::text, \
             typispreferred::text, typisdefined::text, typrelid::text, typelem::text, \
             typarray::text, typbasetype::text, typtypmod::text, typcollation::text, \
             typsubscript::oid::text from pg_type",
        )? {
            if keep(oid(&r[0])) {
                d.types.push(TypeRow {
                    oid: oid(&r[0]),
                    name: string(&r[1]),
                    nsp: oid(&r[2]),
                    len: int(&r[3]) as i16,
                    byval: boolean(&r[4]),
                    align: chr(&r[5]),
                    typtype: chr(&r[6]),
                    category: chr(&r[7]),
                    preferred: boolean(&r[8]),
                    isdefined: boolean(&r[9]),
                    relid: oid(&r[10]),
                    elem: oid(&r[11]),
                    array: oid(&r[12]),
                    basetype: oid(&r[13]),
                    typmod: int(&r[14]),
                    collation: oid(&r[15]),
                    subscript: oid(&r[16]),
                });
            }
        }
        for r in text_rows(
            self,
            "select oid::text, proname::text, pronamespace::text, prokind::text, \
             proretset::text, prorettype::text, pronargs::text, pronargdefaults::text, \
             proargtypes::text, proallargtypes::text, proargmodes::text, \
             proargnames::text, provariadic::text, proargdefaults::text from pg_proc",
        )? {
            if keep(oid(&r[0])) {
                let argtypes = r[8]
                    .as_deref()
                    .unwrap_or_default()
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                d.procs.push(ProcRow {
                    oid: oid(&r[0]),
                    name: string(&r[1]),
                    nsp: oid(&r[2]),
                    kind: chr(&r[3]),
                    retset: boolean(&r[4]),
                    rettype: oid(&r[5]),
                    nargs: int(&r[6]),
                    nargdefaults: int(&r[7]),
                    argtypes,
                    allargtypes: array_items(&r[9])
                        .map(|v| v.iter().filter_map(|x| x.parse().ok()).collect()),
                    argmodes: array_items(&r[10])
                        .map(|v| v.iter().filter_map(|x| x.chars().next()).collect()),
                    argnames: array_items(&r[11]),
                    variadic: oid(&r[12]),
                    argdefaults: r[13].clone(),
                });
            }
        }
        for r in text_rows(
            self,
            "select oid::text, oprname::text, oprnamespace::text, oprkind::text, \
             oprleft::text, oprright::text, oprresult::text, oprcode::oid::text, \
             oprcom::text, oprnegate::text from pg_operator",
        )? {
            if keep(oid(&r[0])) {
                d.operators.push(OperRow {
                    oid: oid(&r[0]),
                    name: string(&r[1]),
                    nsp: oid(&r[2]),
                    kind: chr(&r[3]),
                    left: oid(&r[4]),
                    right: oid(&r[5]),
                    result: oid(&r[6]),
                    code: oid(&r[7]),
                    com: oid(&r[8]),
                    negate: oid(&r[9]),
                });
            }
        }
        for r in text_rows(
            self,
            "select oid::text, castsource::text, casttarget::text, castfunc::text, \
             castcontext::text, castmethod::text from pg_cast",
        )? {
            if keep(oid(&r[0])) {
                d.casts.push(CastRow {
                    source: oid(&r[1]),
                    target: oid(&r[2]),
                    func: oid(&r[3]),
                    context: chr(&r[4]),
                    method: chr(&r[5]),
                });
            }
        }
        for r in text_rows(
            self,
            "select aggfnoid::oid::text, aggkind::text, aggnumdirectargs::text, \
             aggtranstype::text from pg_aggregate",
        )? {
            if keep(oid(&r[0])) {
                d.aggregates.push(AggRow {
                    fnoid: oid(&r[0]),
                    kind: chr(&r[1]),
                    numdirectargs: int(&r[2]),
                    transtype: oid(&r[3]),
                });
            }
        }
        for r in text_rows(
            self,
            "select oid::text, opcmethod::text, opcfamily::text, opcintype::text, \
             opcdefault::text from pg_opclass",
        )? {
            if keep(oid(&r[0])) {
                d.opclasses.push(OpclassRow {
                    oid: oid(&r[0]),
                    method: oid(&r[1]),
                    family: oid(&r[2]),
                    intype: oid(&r[3]),
                    default: boolean(&r[4]),
                });
            }
        }
        for r in text_rows(
            self,
            "select amopfamily::text, amoplefttype::text, amoprighttype::text, \
             amopstrategy::text, amoppurpose::text, amopopr::text, amopmethod::text \
             from pg_amop",
        )? {
            if keep(oid(&r[0])) {
                d.amops.push(AmopRow {
                    family: oid(&r[0]),
                    lefttype: oid(&r[1]),
                    righttype: oid(&r[2]),
                    strategy: int(&r[3]),
                    purpose: chr(&r[4]),
                    opr: oid(&r[5]),
                    method: oid(&r[6]),
                });
            }
        }
        for r in text_rows(
            self,
            "select amprocfamily::text, amproclefttype::text, amprocrighttype::text, \
             amprocnum::text, amproc::oid::text from pg_amproc",
        )? {
            if keep(oid(&r[0])) {
                d.amprocs.push(AmprocRow {
                    family: oid(&r[0]),
                    lefttype: oid(&r[1]),
                    righttype: oid(&r[2]),
                    num: int(&r[3]),
                    proc_: oid(&r[4]),
                });
            }
        }
        // Primary keys, for a grouped query's functional dependencies.
        for r in text_rows(
            self,
            "select conrelid::text, oid::text, conkey::text, condeferrable::text \
             from pg_constraint where contype = 'p'",
        )? {
            if keep(oid(&r[0])) {
                let cols = array_items(&r[2])
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| c.trim().parse::<i16>().ok())
                    .collect();
                d.pkeys.push((oid(&r[0]), oid(&r[1]), cols, boolean(&r[3])));
            }
        }
        for r in text_rows(
            self,
            "select oid::text, collname::text, collnamespace::text, collencoding::text \
             from pg_collation",
        )? {
            if keep(oid(&r[0])) {
                d.collations.push(CollRow {
                    oid: oid(&r[0]),
                    name: string(&r[1]),
                    nsp: oid(&r[2]),
                    encoding: int(&r[3]),
                });
            }
        }
        for r in text_rows(
            self,
            "select rngtypid::text, rngsubtype::text, rngmultitypid::text from pg_range",
        )? {
            if keep(oid(&r[0])) {
                d.ranges.push(RangeRow {
                    typid: oid(&r[0]),
                    subtype: oid(&r[1]),
                    multitypid: oid(&r[2]),
                });
            }
        }
        for r in text_rows(
            self,
            "select inhrelid::text, inhparent::text from pg_inherits",
        )? {
            if keep(oid(&r[0])) {
                d.inherits.push((oid(&r[0]), oid(&r[1])));
            }
        }
        d.database = self.db().to_string();
        d.encoding = 6;
        Ok(d)
    }

    /// The system rules' trees, analysed once per process from the
    /// catalog `initdb` leaves.
    pub(crate) fn system_rule_trees(&self) -> Arc<SystemTrees> {
        static TREES: OnceLock<Arc<SystemTrees>> = OnceLock::new();
        if let Some(t) = TREES.get() {
            return Arc::clone(t);
        }
        let data = match self.rule_catalog_data(true) {
            Ok(d) => d,
            Err(e) => {
                // Not cached: the next read of pg_rewrite tries again.
                eprintln!("secantusd-pg: the system rules' pg_rewrite trees were not built: {e:?}");
                return Arc::new(HashMap::new());
            }
        };
        let cat = Catalog::new(data);
        let mut map = HashMap::new();
        for (relid, name, trees) in secantus_pgplan::nodetostring::system::system_rule_trees(&cat) {
            match trees {
                Ok(t) => {
                    map.insert((i64::from(relid), name), (t.ev_qual, t.ev_action));
                }
                Err(e) => eprintln!(
                    "secantusd-pg: no pg_rewrite trees for system rule \"{name}\" of relation {relid}: {e}"
                ),
            }
        }
        Arc::clone(TREES.get_or_init(|| Arc::new(map)))
    }

    /// The active `search_path` as namespace oids (`"$user"` is the role's
    /// own schema when there is one).
    fn rule_search_path(&self, cat: &Catalog) -> Vec<u32> {
        let path = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .get("search_path")
            .cloned()
            .unwrap_or_else(|| "\"$user\", public".to_string());
        let role = self.current_role_name();
        path.split(',')
            .filter_map(|p| {
                let p = p.trim();
                let name = if p == "\"$user\"" || p == "$user" {
                    role.clone()
                } else if let Some(q) = p.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
                    q.replace("\"\"", "\"")
                } else {
                    p.to_ascii_lowercase()
                };
                cat.namespace_oid(&name)
            })
            .collect()
    }

    /// Remember the query string the statement now running came from, and
    /// its place in it: a rule's tree records source offsets into that
    /// string, as PostgreSQL's do.
    pub(crate) fn set_statement_source(&self, source: &str, index: usize) {
        *self
            .statement_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((source.to_string(), index));
    }

    /// Make `sql` the statement source, as SPI hands a PL/pgSQL
    /// statement's text (or an EXECUTE's string) to the parser on its own;
    /// returns the source around it for [`Self::restore_statement_source`].
    pub(crate) fn enter_statement_source(&self, sql: &str) -> Option<(String, usize)> {
        self.statement_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace((sql.to_string(), 0))
    }

    pub(crate) fn restore_statement_source(&self, outer: Option<(String, usize)>) {
        *self
            .statement_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = outer;
    }

    /// The trees `CREATE VIEW` (`kind` 'v'), `CREATE MATERIALIZED VIEW`
    /// ('m') or `CREATE RULE` ('r') stores for the
    /// relation `relid`, from the statement that is running. `None` when
    /// the statement cannot be found in its query string (a statement a
    /// PL/pgSQL function runs) or the analysis refuses something it does
    /// not transcribe: no tree rather than a wrong one.
    pub(crate) fn statement_rule_trees(
        &self,
        kind: char,
        relname: &str,
        rulename: &str,
        relid: i64,
    ) -> Option<(String, String)> {
        match self.statement_rule_trees_inner(kind, relname, rulename, relid) {
            Ok(t) => Some(t),
            Err(e) => {
                // A construct the transcription does not cover leaves the
                // trees NULL rather than wrong; say so, since pg_rewrite
                // will not.
                eprintln!(
                    "secantusd-pg: no pg_rewrite trees recorded for rule \"{rulename}\" of \"{relname}\": {e}"
                );
                None
            }
        }
    }

    fn statement_rule_trees_inner(
        &self,
        kind: char,
        relname: &str,
        rulename: &str,
        relid: i64,
    ) -> Result<(String, String), String> {
        let (source, index) = self
            .statement_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or("the statement's query string is not known")?;
        let parsed = pg_query::parse(&source).map_err(|e| e.to_string())?;
        let matches = |i: usize| -> bool {
            let Some(n) = parsed
                .protobuf
                .stmts
                .get(i)
                .and_then(|r| r.stmt.as_ref())
                .and_then(|s| s.node.as_ref())
            else {
                return false;
            };
            use pg_query::protobuf::node::Node as N;
            match n {
                N::ViewStmt(v) if kind == 'v' => {
                    v.view.as_ref().is_some_and(|r| r.relname == relname)
                }
                N::CreateTableAsStmt(c) if kind == 'm' => c
                    .into
                    .as_ref()
                    .and_then(|i| i.rel.as_ref())
                    .is_some_and(|r| r.relname == relname),
                N::RuleStmt(r) if kind == 'r' => {
                    r.rulename == rulename
                        && r.relation.as_ref().is_some_and(|x| x.relname == relname)
                }
                _ => false,
            }
        };
        let index = if matches(index) {
            index
        } else {
            (0..parsed.protobuf.stmts.len())
                .find(|i| matches(*i))
                .ok_or("the statement is not in its query string")?
        };
        let data = self
            .rule_catalog_data(false)
            .map_err(|e| format!("{e:?}"))?;
        let cat = Catalog::new(data);
        let ctx = secantus_pgplan::nodetostring::AnalysisContext {
            catalog: &cat,
            search_path: self.rule_search_path(&cat),
        };
        let oid = u32::try_from(relid).map_err(|e| e.to_string())?;
        let trees = match kind {
            'v' => secantus_pgplan::nodetostring::analyze_create_view(&ctx, &source, index, oid),
            'm' => secantus_pgplan::nodetostring::analyze_create_matview(&ctx, &source, index, oid),
            _ => secantus_pgplan::nodetostring::analyze_create_rule(&ctx, &source, index, oid),
        }
        .map_err(|e| e.to_string())?;
        Ok((trees.ev_qual, trees.ev_action))
    }
}
