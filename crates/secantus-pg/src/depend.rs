//! `pg_depend`'s rows between objects other than relations' sequences,
//! defaults and partitions (those are `catalog_fill::pg_depend_rows`): what
//! PostgreSQL's `recordDependencyOn` calls leave behind as each object is
//! created (measured on 15.19).
//!
//! Every row is derived from the catalog rows this server already serves --
//! `pg_class`, `pg_type`, `pg_proc`, `pg_constraint`, `pg_index`,
//! `pg_trigger`, `pg_rewrite`, `pg_policy`, `pg_attribute` -- so its oids
//! are the ones a client joins those catalogs on. The columns an expression
//! reads are found by parsing it, as `find_expr_references` walks its tree.
//!
//! Not recorded (a remainder of RP-105): the type of a constant an
//! expression holds (`b = 'a'` against an enum column records the enum),
//! range types' constructor functions and casts, collations and operator
//! classes, and extension membership.

use super::*;

const PG_CLASS: i64 = 1259;
const PG_TYPE: i64 = 1247;
const PG_PROC: i64 = 1255;
const PG_NAMESPACE: i64 = 2615;
const PG_LANGUAGE: i64 = 2612;
const PG_CONSTRAINT: i64 = 2606;
const PG_TRIGGER: i64 = 2620;
const PG_REWRITE: i64 = 2618;
const PG_POLICY: i64 = 3256;

/// PostgreSQL's `IsPinnedObject`: an object `initdb` made below
/// `FirstUnpinnedObjectId` is depended on by no row, except the `public`
/// schema, which can be dropped.
fn pinned(class: i64, oid: i64) -> bool {
    oid < 12000 && !(class == PG_NAMESPACE && oid == 2200)
}

/// One system catalog's rows, read by column name.
struct Rows {
    def: TableDef,
    rows: Vec<Document>,
}

impl Rows {
    fn get<'a>(&self, row: &'a Document, column: &str) -> Option<&'a Bson> {
        row.get(self.def.field_of(column)?)
    }

    fn int(&self, row: &Document, column: &str) -> i64 {
        match self.get(row, column) {
            Some(Bson::Int64(v)) => *v,
            Some(Bson::Int32(v)) => i64::from(*v),
            _ => 0,
        }
    }

    fn text(&self, row: &Document, column: &str) -> String {
        match self.get(row, column) {
            Some(Bson::String(s)) => s.clone(),
            _ => String::new(),
        }
    }

    fn truth(&self, row: &Document, column: &str) -> bool {
        matches!(self.get(row, column), Some(Bson::Boolean(true)))
    }

    /// An array, int2vector or oidvector column's numbers.
    fn ints(&self, row: &Document, column: &str) -> Vec<i64> {
        let Some(v) = self.get(row, column) else {
            return Vec::new();
        };
        match secantus_pgplan::arrays::strip(v) {
            Bson::Array(items) => items
                .iter()
                .filter_map(|b| match b {
                    Bson::Int64(v) => Some(*v),
                    Bson::Int32(v) => Some(i64::from(*v)),
                    _ => None,
                })
                .collect(),
            Bson::String(s) => s
                .split_ascii_whitespace()
                .filter_map(|w| w.parse().ok())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// The names an expression reads -- `(qualifier, column)` per column
/// reference, `*` for a star -- and the types it casts to.
#[derive(Default)]
struct References {
    columns: Vec<(Option<String>, String)>,
    casts: Vec<String>,
}

fn references(nodes: Vec<(pg_query::NodeRef<'_>, i32, pg_query::Context, bool)>) -> References {
    let mut out = References::default();
    let string = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
        Some(pg_query::protobuf::node::Node::String(s)) => Some(s.sval.clone()),
        Some(pg_query::protobuf::node::Node::AStar(_)) => Some("*".to_string()),
        _ => None,
    };
    for (n, _, _, _) in nodes {
        match n {
            pg_query::NodeRef::ColumnRef(c) => {
                let parts: Vec<String> = c.fields.iter().filter_map(string).collect();
                match parts.as_slice() {
                    [col] => out.columns.push((None, col.clone())),
                    [.., q, col] => out.columns.push((Some(q.clone()), col.clone())),
                    [] => {}
                }
            }
            pg_query::NodeRef::TypeName(t) => {
                let names: Vec<String> = t.names.iter().filter_map(string).collect();
                if let Some(last) = names.last() {
                    out.casts.push(last.clone());
                }
            }
            _ => {}
        }
    }
    out
}

/// The references of an expression, as the text a catalog column holds it.
fn expression_references(expr: &str) -> References {
    match pg_query::parse(&format!("SELECT {expr}")) {
        Ok(tree) => references(tree.protobuf.nodes()),
        Err(_) => References::default(),
    }
}

impl PgHandler {
    fn depend_catalog(&self, name: &str) -> Rows {
        Rows {
            def: Self::virtual_table(name).unwrap_or_else(|| TableDef::new(name, Vec::new())),
            rows: self
                .virtual_rows(name, &Document::new())
                .unwrap_or_default(),
        }
    }

    /// The rows; see the module comment.
    pub(crate) fn object_depend_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |name: &str| def.field_of(name).expect("column");
        let mut out: Vec<Document> = Vec::new();
        let mut push = |class: i64,
                        obj: i64,
                        sub: i32,
                        refclass: i64,
                        refobj: i64,
                        refsub: i32,
                        deptype: &str| {
            if refobj == 0 || pinned(refclass, refobj) {
                return;
            }
            let mut d = Document::new();
            d.insert(f("classid"), Bson::Int64(class));
            d.insert(f("objid"), Bson::Int64(obj));
            d.insert(f("objsubid"), Bson::Int32(sub));
            d.insert(f("refclassid"), Bson::Int64(refclass));
            d.insert(f("refobjid"), Bson::Int64(refobj));
            d.insert(f("refobjsubid"), Bson::Int32(refsub));
            d.insert(f("deptype"), deptype);
            if !out.contains(&d) {
                out.push(d);
            }
        };
        let classes = self.depend_catalog("pg_class");
        let types = self.depend_catalog("pg_type");
        let procs = self.depend_catalog("pg_proc");
        let constraints = self.depend_catalog("pg_constraint");
        let indexes = self.depend_catalog("pg_index");
        let triggers = self.depend_catalog("pg_trigger");
        let rules = self.depend_catalog("pg_rewrite");
        let policies = self.depend_catalog("pg_policy");
        let attributes = self.depend_catalog("pg_attribute");

        // Each user relation: (oid, relkind, its columns' names by attnum).
        let mut relkind: HashMap<i64, String> = HashMap::new();
        let mut relname: HashMap<i64, String> = HashMap::new();
        for r in &classes.rows {
            let oid = classes.int(r, "oid");
            if oid < 16384 {
                continue;
            }
            let kind = classes.text(r, "relkind");
            // A relation lives in its schema; an index or a standalone
            // composite type does not record it.
            if matches!(kind.as_str(), "r" | "p" | "v" | "m" | "S" | "f") {
                push(
                    PG_CLASS,
                    oid,
                    0,
                    PG_NAMESPACE,
                    classes.int(r, "relnamespace"),
                    0,
                    "n",
                );
            }
            relname.insert(oid, classes.text(r, "relname"));
            relkind.insert(oid, kind);
        }
        let mut columns: HashMap<i64, Vec<(i32, String)>> = HashMap::new();
        for a in &attributes.rows {
            let rel = attributes.int(a, "attrelid");
            let num = attributes.int(a, "attnum") as i32;
            if rel < 16384 || num <= 0 || attributes.truth(a, "attisdropped") {
                continue;
            }
            columns
                .entry(rel)
                .or_default()
                .push((num, attributes.text(a, "attname")));
            // A column of a type someone made depends on the type
            // (`AddNewAttributeTuples`) -- not an index's.
            if relkind.get(&rel).is_some_and(|k| k != "i" && k != "I") {
                push(
                    PG_CLASS,
                    rel,
                    num,
                    PG_TYPE,
                    attributes.int(a, "atttypid"),
                    0,
                    "n",
                );
            }
        }
        let attnum = |rel: i64, name: &str| -> Option<i32> {
            columns
                .get(&rel)?
                .iter()
                .find(|(_, n)| n == name)
                .map(|(i, _)| *i)
        };
        let cast_type = |name: &str| self.type_oid_by_name(name).filter(|o| *o >= 12000);

        // Types.
        for t in &types.rows {
            let oid = types.int(t, "oid");
            if oid < 16384 {
                continue;
            }
            let kind = types.text(t, "typtype");
            let typrelid = types.int(t, "typrelid");
            let elem = types.int(t, "typelem");
            let category = types.text(t, "typcategory");
            if elem != 0 && category == "A" {
                // An array type is part of its element type.
                push(PG_TYPE, oid, 0, PG_TYPE, elem, 0, "i");
                continue;
            }
            match relkind.get(&typrelid).map(String::as_str) {
                // A standalone composite: the type owns the relation.
                Some("c") => {
                    push(
                        PG_TYPE,
                        oid,
                        0,
                        PG_NAMESPACE,
                        types.int(t, "typnamespace"),
                        0,
                        "n",
                    );
                    push(PG_CLASS, typrelid, 0, PG_TYPE, oid, 0, "i");
                }
                // A relation's row type is part of the relation.
                Some(_) => push(PG_TYPE, oid, 0, PG_CLASS, typrelid, 0, "i"),
                None => {
                    push(
                        PG_TYPE,
                        oid,
                        0,
                        PG_NAMESPACE,
                        types.int(t, "typnamespace"),
                        0,
                        "n",
                    );
                    if kind == "d" {
                        push(
                            PG_TYPE,
                            oid,
                            0,
                            PG_TYPE,
                            types.int(t, "typbasetype"),
                            0,
                            "n",
                        );
                    }
                }
            }
        }

        // A range depends on its subtype; its multirange is part of it.
        let ranges = self.depend_catalog("pg_range");
        for r in &ranges.rows {
            let range = ranges.int(r, "rngtypid");
            if range < 16384 {
                continue;
            }
            push(
                PG_TYPE,
                range,
                0,
                PG_TYPE,
                ranges.int(r, "rngsubtype"),
                0,
                "n",
            );
            let multi = ranges.int(r, "rngmultitypid");
            if multi != 0 {
                push(PG_TYPE, multi, 0, PG_TYPE, range, 0, "i");
            }
        }

        // Functions: their schema, language, and the types they take and
        // return.
        for p in &procs.rows {
            let oid = procs.int(p, "oid");
            if oid < 16384 {
                continue;
            }
            push(
                PG_PROC,
                oid,
                0,
                PG_NAMESPACE,
                procs.int(p, "pronamespace"),
                0,
                "n",
            );
            push(
                PG_PROC,
                oid,
                0,
                PG_LANGUAGE,
                procs.int(p, "prolang"),
                0,
                "n",
            );
            push(PG_PROC, oid, 0, PG_TYPE, procs.int(p, "prorettype"), 0, "n");
            for t in procs.ints(p, "proargtypes") {
                push(PG_PROC, oid, 0, PG_TYPE, t, 0, "n");
            }
        }

        // Constraints.
        let mut backing: HashMap<i64, i64> = HashMap::new();
        for c in &constraints.rows {
            let oid = constraints.int(c, "oid");
            let kind = constraints.text(c, "contype");
            let rel = constraints.int(c, "conrelid");
            let domain = constraints.int(c, "contypid");
            if rel == 0 && domain >= 16384 {
                push(PG_CONSTRAINT, oid, 0, PG_TYPE, domain, 0, "a");
                continue;
            }
            if rel < 16384 {
                continue;
            }
            for k in constraints.ints(c, "conkey") {
                push(PG_CONSTRAINT, oid, 0, PG_CLASS, rel, k as i32, "a");
            }
            match kind.as_str() {
                "c" => {
                    let refs = expression_references(&constraints.text(c, "conbin"));
                    for (_, col) in &refs.columns {
                        if let Some(n) = attnum(rel, col) {
                            push(PG_CONSTRAINT, oid, 0, PG_CLASS, rel, n, "n");
                        }
                    }
                    for t in refs.casts.iter().filter_map(|t| cast_type(t)) {
                        push(PG_CONSTRAINT, oid, 0, PG_TYPE, t, 0, "n");
                    }
                }
                "f" => {
                    let other = constraints.int(c, "confrelid");
                    for k in constraints.ints(c, "confkey") {
                        push(PG_CONSTRAINT, oid, 0, PG_CLASS, other, k as i32, "n");
                    }
                    push(
                        PG_CONSTRAINT,
                        oid,
                        0,
                        PG_CLASS,
                        constraints.int(c, "conindid"),
                        0,
                        "n",
                    );
                }
                "p" | "u" | "x" => {
                    backing.insert(constraints.int(c, "conindid"), oid);
                }
                _ => {}
            }
        }

        // Indexes: one backing a constraint is part of it; any other
        // depends on its columns -- on the table as a whole when no key is a
        // plain column (`index_create`) -- and on what its expressions and
        // predicate read.
        for ix in &indexes.rows {
            let oid = indexes.int(ix, "indexrelid");
            let rel = indexes.int(ix, "indrelid");
            if oid < 16384 {
                continue;
            }
            if let Some(con) = backing.get(&oid) {
                push(PG_CLASS, oid, 0, PG_CONSTRAINT, *con, 0, "i");
                continue;
            }
            let keys: Vec<i64> = indexes
                .ints(ix, "indkey")
                .into_iter()
                .filter(|k| *k != 0)
                .collect();
            if keys.is_empty() {
                push(PG_CLASS, oid, 0, PG_CLASS, rel, 0, "a");
            }
            for k in keys {
                push(PG_CLASS, oid, 0, PG_CLASS, rel, k as i32, "a");
            }
            for column in ["indexprs", "indpred"] {
                let refs = expression_references(&indexes.text(ix, column));
                for (_, col) in &refs.columns {
                    if let Some(n) = attnum(rel, col) {
                        push(PG_CLASS, oid, 0, PG_CLASS, rel, n, "a");
                    }
                }
                for t in refs.casts.iter().filter_map(|t| cast_type(t)) {
                    push(PG_CLASS, oid, 0, PG_TYPE, t, 0, "n");
                }
            }
        }

        // Triggers: their table and function, an UPDATE OF's columns; an
        // internal one is part of its constraint.
        for t in &triggers.rows {
            let oid = triggers.int(t, "oid");
            if triggers.truth(t, "tgisinternal") {
                push(
                    PG_TRIGGER,
                    oid,
                    0,
                    PG_CONSTRAINT,
                    triggers.int(t, "tgconstraint"),
                    0,
                    "i",
                );
                continue;
            }
            let rel = triggers.int(t, "tgrelid");
            push(
                PG_TRIGGER,
                oid,
                0,
                PG_PROC,
                triggers.int(t, "tgfoid"),
                0,
                "n",
            );
            push(PG_TRIGGER, oid, 0, PG_CLASS, rel, 0, "a");
            for k in triggers.ints(t, "tgattr") {
                push(PG_TRIGGER, oid, 0, PG_CLASS, rel, k as i32, "n");
            }
        }

        // Policies: their table, and the columns their quals read.
        for p in &policies.rows {
            let oid = policies.int(p, "oid");
            let rel = policies.int(p, "polrelid");
            push(PG_POLICY, oid, 0, PG_CLASS, rel, 0, "a");
            for column in ["polqual", "polwithcheck"] {
                let refs = expression_references(&policies.text(p, column));
                for (_, col) in &refs.columns {
                    if let Some(n) = attnum(rel, col) {
                        push(PG_POLICY, oid, 0, PG_CLASS, rel, n, "n");
                    }
                }
            }
        }

        // Rules. A view's `_RETURN` is part of the view and reads what its
        // query reads; a rule a user made depends on its table.
        let queries: HashMap<i64, String> = self
            .views()
            .unwrap_or_default()
            .into_iter()
            .map(|(n, sql)| (Self::view_oid(&n), sql))
            .chain(
                self.matviews()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(n, sql, _)| Some((self.relation_oid(&n)?, sql))),
            )
            .collect();
        let by_name: HashMap<String, i64> = relname
            .iter()
            .filter(|(o, _)| relkind.get(o).is_some_and(|k| k != "i" && k != "I"))
            .map(|(o, n)| (n.clone(), *o))
            .collect();
        for r in &rules.rows {
            let oid = rules.int(r, "oid");
            let rel = rules.int(r, "ev_class");
            if rel < 16384 {
                continue;
            }
            if rules.text(r, "rulename") != "_RETURN" {
                push(PG_REWRITE, oid, 0, PG_CLASS, rel, 0, "a");
                push(PG_REWRITE, oid, 0, PG_CLASS, rel, 0, "n");
                continue;
            }
            push(PG_REWRITE, oid, 0, PG_CLASS, rel, 0, "i");
            push(PG_REWRITE, oid, 0, PG_CLASS, rel, 0, "n");
            let Some(sql) = queries.get(&rel) else {
                continue;
            };
            let Ok(tree) = pg_query::parse(sql) else {
                continue;
            };
            let nodes = tree.protobuf.nodes();
            // The relations the query reads, by the name each is known by.
            let mut scope: Vec<(String, i64)> = Vec::new();
            for (n, _, _, _) in &nodes {
                if let pg_query::NodeRef::RangeVar(v) = n {
                    let key = if v.schemaname.is_empty() || v.schemaname == "public" {
                        v.relname.clone()
                    } else {
                        format!("{}.{}", v.schemaname, v.relname)
                    };
                    let Some(oid) = by_name.get(&key).copied() else {
                        continue;
                    };
                    let known = v
                        .alias
                        .as_ref()
                        .map(|a| a.aliasname.clone())
                        .filter(|a| !a.is_empty())
                        .unwrap_or_else(|| v.relname.clone());
                    scope.push((known, oid));
                }
            }
            let refs = references(nodes);
            for (qualifier, col) in &refs.columns {
                let targets: Vec<i64> = match qualifier {
                    Some(q) => scope
                        .iter()
                        .filter(|(n, _)| n == q)
                        .map(|(_, o)| *o)
                        .collect(),
                    None => scope.iter().map(|(_, o)| *o).collect(),
                };
                for target in targets {
                    if col == "*" {
                        for (n, _) in columns.get(&target).cloned().unwrap_or_default() {
                            push(PG_REWRITE, oid, 0, PG_CLASS, target, n, "n");
                        }
                    } else if let Some(n) = attnum(target, col) {
                        push(PG_REWRITE, oid, 0, PG_CLASS, target, n, "n");
                        // A bare name is the one relation that has it.
                        if qualifier.is_none() {
                            break;
                        }
                    }
                }
            }
            for t in refs.casts.iter().filter_map(|t| cast_type(t)) {
                push(PG_REWRITE, oid, 0, PG_TYPE, t, 0, "n");
            }
        }
        out
    }
}
