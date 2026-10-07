//! `EXPLAIN`: the SHAPE of a statement's plan, in PostgreSQL's layout.
//!
//! This server has no cost model, and inventing one would be the kind of
//! plausible-but-wrong answer the project refuses: a client comparing costs
//! would be comparing fiction. So the node TYPES and their nesting are real --
//! a scan is an `Index Scan` exactly when the storage planner would use the
//! index -- and the costs print as zeros (`COSTS OFF` omits them, which is
//! what a test comparing plans uses anyway).

use bson::Document;
use secantus_pgplan::joins::{JoinKind, JoinNode};
use secantus_pgplan::{ExplainOptions, Statement};

/// One plan node: its header, its detail lines, its children.
pub(crate) struct PlanNode {
    pub name: String,
    /// `(label, value)`: `Sort Key: id`, `Group Key: g`, `Index Name: ...`.
    pub details: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
    /// JSON-only facts (`Relation Name`, `Index Name`).
    pub props: Vec<(String, String)>,
}

impl PlanNode {
    fn new(name: impl Into<String>) -> Self {
        PlanNode {
            name: name.into(),
            details: Vec::new(),
            children: Vec::new(),
            props: Vec::new(),
        }
    }

    fn over(name: impl Into<String>, child: PlanNode) -> Self {
        let mut n = PlanNode::new(name);
        n.children.push(child);
        n
    }
}

/// How a table scan will run: `Some((index, key))` for an index scan.
pub(crate) type ScanChooser<'a> = dyn Fn(&str, &Document) -> Option<String> + 'a;

/// The plan tree of a planned statement.
pub(crate) fn plan_tree(stmt: &Statement, scan: &ScanChooser<'_>) -> PlanNode {
    match stmt {
        Statement::Select(sel) => {
            let mut node = if let Some(sub) = &sel.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => {
                        let mut n = PlanNode::over(
                            format!("Subquery Scan on {}", display_alias(&sub.alias)),
                            plan_tree(inner, scan),
                        );
                        n.props.push(("Alias".into(), display_alias(&sub.alias)));
                        n
                    }
                }
            } else if sel.series.is_some() {
                PlanNode::new("Function Scan on generate_series")
            } else if sel.join.is_some() {
                let mut n = PlanNode::new("Nested Loop");
                n.children.push(PlanNode::new("Seq Scan"));
                n.children.push(PlanNode::new("Seq Scan"));
                n
            } else if sel.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&sel.table, &sel.filter, scan)
            };
            if sel.residual.is_some() && node.details.is_empty() && !sel.table.is_empty() {
                node.details.push(("Filter".into(), "(...)".into()));
            }
            if !sel.windows.is_empty() {
                node = PlanNode::over("WindowAgg", node);
            }
            if !sel.order.is_empty() {
                let mut sort = PlanNode::over("Sort", node);
                let keys: Vec<String> = sel
                    .order
                    .iter()
                    .map(|k| {
                        let name = sel
                            .columns
                            .iter()
                            .find(|(_, f)| *f == k.field)
                            .map_or_else(|| display_alias(&k.field), |(o, _)| o.clone());
                        if k.ascending {
                            name
                        } else {
                            format!("{name} DESC")
                        }
                    })
                    .collect();
                sort.details.push(("Sort Key".into(), keys.join(", ")));
                node = sort;
            }
            if !matches!(sel.distinct, secantus_pgplan::Distinct::None) {
                node = PlanNode::over("Unique", node);
            }
            if sel.limit.is_some() || sel.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::Aggregate(agg) => {
            let source = if let Some(sub) = &agg.sub {
                match sub.plan.as_ref() {
                    Statement::JoinRows(j) => join_tree(&j.tree, scan),
                    inner => PlanNode::over(
                        format!("Subquery Scan on {}", display_alias(&sub.alias)),
                        plan_tree(inner, scan),
                    ),
                }
            } else if agg.table.is_empty() {
                PlanNode::new("Result")
            } else {
                table_scan(&agg.table, &agg.filter, scan)
            };
            let mut node = if agg.group_by.is_empty() {
                PlanNode::over("Aggregate", source)
            } else {
                let mut n = PlanNode::over("HashAggregate", source);
                let keys: Vec<String> = agg.group_by.iter().map(|k| k.name.clone()).collect();
                n.details.push(("Group Key".into(), keys.join(", ")));
                n
            };
            if !agg.order.is_empty() {
                node = PlanNode::over("Sort", node);
            }
            if agg.limit.is_some() || agg.offset > 0 {
                node = PlanNode::over("Limit", node);
            }
            node
        }
        Statement::SetOp(set) => {
            let mut n = PlanNode::new("Append");
            n.children.push(plan_tree(&set.left, scan));
            n.children.push(plan_tree(&set.right, scan));
            n
        }
        Statement::SelectConstant(_) => PlanNode::new("Result"),
        Statement::ValuesConstant(_) => PlanNode::new("Values Scan on \"*VALUES*\""),
        Statement::Insert(ins) => {
            let child = match &ins.source {
                Some(src) => plan_tree(src, scan),
                None if ins.rows.len() > 1 => PlanNode::new("Values Scan on \"*VALUES*\""),
                None => PlanNode::new("Result"),
            };
            PlanNode::over(format!("Insert on {}", ins.table), child)
        }
        Statement::Update(u) => PlanNode::over(
            format!("Update on {}", u.table),
            table_scan(&u.table, &u.filter, scan),
        ),
        Statement::Delete(d) => PlanNode::over(
            format!("Delete on {}", d.table),
            table_scan(&d.table, &d.filter, scan),
        ),
        _ => PlanNode::new("Result"),
    }
}

/// A join's key names are `alias<US>column`; a plan names only the alias.
fn display_alias(name: &str) -> String {
    name.split(secantus_pgplan::joins::SEP)
        .next()
        .unwrap_or(name)
        .to_string()
}

fn table_scan(table: &str, filter: &Document, scan: &ScanChooser<'_>) -> PlanNode {
    let mut n = match scan(table, filter) {
        Some(index) => {
            let mut n = PlanNode::new(format!("Index Scan using {index} on {table}"));
            n.props.push(("Index Name".into(), index));
            n
        }
        None => PlanNode::new(format!("Seq Scan on {table}")),
    };
    n.props.push(("Relation Name".into(), table.to_string()));
    n
}

fn join_tree(node: &JoinNode, scan: &ScanChooser<'_>) -> PlanNode {
    match node {
        JoinNode::Lateral { .. } => PlanNode::new("Function Scan"),
        JoinNode::Leaf { plan, def, columns } => {
            let mut n = plan_tree(plan, scan);
            if n.name == "Result" && !def.name.is_empty() {
                n.name = format!("Subquery Scan on {}", def.name);
            }
            // A scan of an aliased relation names the alias too.
            let alias = columns
                .first()
                .and_then(|(key, _)| key.split(secantus_pgplan::joins::SEP).next())
                .map(str::to_string);
            let relation = n
                .props
                .iter()
                .find(|(k, _)| k == "Relation Name")
                .map(|(_, v)| v.clone());
            if let (Some(alias), Some(rel)) = (alias, relation) {
                if alias != rel && !alias.is_empty() {
                    n.name = format!("{} {alias}", n.name);
                    n.props.push(("Alias".into(), alias));
                }
            }
            n
        }
        JoinNode::Join {
            kind,
            left,
            right,
            equi,
            ..
        } => {
            let side = match kind {
                JoinKind::Inner => "",
                JoinKind::Left => " Left",
                JoinKind::Right => " Right",
                JoinKind::Full => " Full",
            };
            let name = if equi.is_empty() {
                format!("Nested Loop{side} Join").replace("Nested Loop Join", "Nested Loop")
            } else {
                format!("Hash{side} Join")
            };
            let mut n = PlanNode::new(name);
            n.children.push(join_tree(left, scan));
            let right = join_tree(right, scan);
            n.children.push(if equi.is_empty() {
                right
            } else {
                PlanNode::over("Hash", right)
            });
            n
        }
    }
}

const ZERO_COSTS: &str = "  (cost=0.00..0.00 rows=0 width=0)";

/// The `QUERY PLAN` text rows, PostgreSQL's layout: a child at depth d is
/// `2 + 6(d-1)` spaces and `->  `, and a node's details sit at `2 + 6d`.
pub(crate) fn render_text(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(
        n: &PlanNode,
        depth: usize,
        options: &ExplainOptions,
        actual: Option<usize>,
        out: &mut Vec<String>,
    ) {
        let mut header = if depth == 0 {
            n.name.clone()
        } else {
            format!("{}->  {}", " ".repeat(2 + 6 * (depth - 1)), n.name)
        };
        if options.costs {
            header.push_str(ZERO_COSTS);
        }
        if let Some(rows) = actual {
            header.push_str(&format!(" (actual time=0.000..0.000 rows={rows} loops=1)"));
        }
        out.push(header);
        for (label, value) in &n.details {
            out.push(format!("{}{label}: {value}", " ".repeat(2 + 6 * depth)));
        }
        for c in &n.children {
            walk(c, depth + 1, options, None, out);
        }
    }
    walk(root, 0, options, actual_rows, &mut out);
    if actual_rows.is_some() {
        out.push("Planning Time: 0.000 ms".into());
        out.push("Execution Time: 0.000 ms".into());
    }
    out
}

/// The `QUERY PLAN` as PostgreSQL's `FORMAT JSON`: one array holding one
/// object whose `Plan` is the tree.
/// The plan as the structured formats (JSON / YAML / XML) share it: the
/// `[{"Plan": {...}}]` value tree, keys in PostgreSQL's order.
fn structured(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> serde_json_lite::Value {
    use serde_json_lite::Value;
    fn node(
        n: &PlanNode,
        options: &ExplainOptions,
        actual: Option<usize>,
        relationship: Option<&str>,
    ) -> Value {
        let mut fields: Vec<(String, Value)> = Vec::new();
        let node_type = n
            .name
            .split(" on ")
            .next()
            .unwrap_or(&n.name)
            .split(" using ")
            .next()
            .unwrap_or(&n.name)
            .to_string();
        fields.push(("Node Type".into(), Value::Str(node_type.clone())));
        if let Some(r) = relationship {
            fields.push(("Parent Relationship".into(), Value::Str(r.into())));
        }
        fields.push(("Parallel Aware".into(), Value::Num("false".into())));
        fields.push(("Async Capable".into(), Value::Num("false".into())));
        for (k, v) in &n.props {
            fields.push((k.clone(), Value::Str(v.clone())));
        }
        // A scanned relation's alias is its name unless the plan named one.
        if n.props.iter().any(|(k, _)| k == "Relation Name")
            && !n.props.iter().any(|(k, _)| k == "Alias")
        {
            let rel = n
                .props
                .iter()
                .find(|(k, _)| k == "Relation Name")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            fields.push(("Alias".into(), Value::Str(rel)));
        }
        for (k, v) in &n.details {
            let value = if k.ends_with("Key") {
                Value::Arr(v.split(", ").map(|s| Value::Str(s.to_string())).collect())
            } else {
                Value::Str(v.clone())
            };
            fields.push((k.clone(), value));
        }
        if options.costs {
            for k in ["Startup Cost", "Total Cost"] {
                fields.push((k.into(), Value::Num("0.00".into())));
            }
            for k in ["Plan Rows", "Plan Width"] {
                fields.push((k.into(), Value::Num("0".into())));
            }
        }
        if let Some(rows) = actual {
            fields.push(("Actual Rows".into(), Value::Num(rows.to_string())));
            fields.push(("Actual Loops".into(), Value::Num("1".into())));
        }
        if !n.children.is_empty() {
            let kids = n
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let rel = if node_type == "Append" {
                        "Member"
                    } else if node_type == "Subquery Scan" {
                        "Subquery"
                    } else if i == 0 {
                        "Outer"
                    } else {
                        "Inner"
                    };
                    node(c, options, None, Some(rel))
                })
                .collect();
            fields.push(("Plans".into(), Value::Arr(kids)));
        }
        Value::Obj(fields)
    }
    let mut top = vec![("Plan".to_string(), node(root, options, actual_rows, None))];
    if actual_rows.is_some() {
        top.push(("Planning Time".into(), Value::Num("0.000".into())));
        top.push(("Execution Time".into(), Value::Num("0.000".into())));
    }
    Value::Arr(vec![Value::Obj(top)])
}

/// `FORMAT JSON`.
pub(crate) fn render_json(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    structured(root, options, actual_rows).pretty(0)
}

/// `FORMAT YAML`: PostgreSQL's layout -- a list of one mapping, a nested
/// mapping or list opening on a line of its own after `key: ` (with that
/// trailing blank), strings double-quoted, numbers and booleans bare.
pub(crate) fn render_yaml(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    use serde_json_lite::Value;
    fn scalar(v: &Value) -> Option<String> {
        match v {
            Value::Str(s) => Some(serde_json_lite::quote(s)),
            Value::Num(n) => Some(n.clone()),
            _ => None,
        }
    }
    fn field(key: &str, v: &Value, col: usize, lead: &str, out: &mut Vec<String>) {
        match v {
            Value::Obj(fs) => {
                out.push(format!("{lead}{key}: "));
                for (k, v) in fs {
                    field(k, v, col + 2, &" ".repeat(col + 2), out);
                }
            }
            Value::Arr(items) => {
                out.push(format!("{lead}{key}: "));
                for item in items {
                    list_item(item, col + 2, out);
                }
            }
            other => out.push(format!(
                "{lead}{key}: {}",
                scalar(other).unwrap_or_default()
            )),
        }
    }
    fn list_item(item: &Value, col: usize, out: &mut Vec<String>) {
        let dash = format!("{}- ", " ".repeat(col));
        match item {
            Value::Obj(fs) => {
                for (i, (k, v)) in fs.iter().enumerate() {
                    let lead = if i == 0 {
                        dash.clone()
                    } else {
                        " ".repeat(col + 2)
                    };
                    field(k, v, col + 2, &lead, out);
                }
            }
            other => out.push(format!("{dash}{}", scalar(other).unwrap_or_default())),
        }
    }
    let mut out = Vec::new();
    if let Value::Arr(items) = structured(root, options, actual_rows) {
        for item in &items {
            list_item(item, 0, &mut out);
        }
    }
    out.join("\n")
}

/// `FORMAT XML`: the `explain` document PostgreSQL writes, keys with their
/// blanks as hyphens, a list's items as `<Item>` (plans as `<Plan>`).
pub(crate) fn render_xml(
    root: &PlanNode,
    options: &ExplainOptions,
    actual_rows: Option<usize>,
) -> String {
    use serde_json_lite::Value;
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    fn element(tag: &str, v: &Value, depth: usize, out: &mut Vec<String>) {
        let pad = " ".repeat(depth * 2);
        let tag = tag.replace(' ', "-");
        match v {
            Value::Str(s) => out.push(format!("{pad}<{tag}>{}</{tag}>", escape(s))),
            Value::Num(n) => out.push(format!("{pad}<{tag}>{n}</{tag}>")),
            Value::Obj(fs) => {
                out.push(format!("{pad}<{tag}>"));
                for (k, v) in fs {
                    element(k, v, depth + 1, out);
                }
                out.push(format!("{pad}</{tag}>"));
            }
            Value::Arr(items) => {
                out.push(format!("{pad}<{tag}>"));
                let item_tag = if tag == "Plans" { "Plan" } else { "Item" };
                for item in items {
                    element(item_tag, item, depth + 1, out);
                }
                out.push(format!("{pad}</{tag}>"));
            }
        }
    }
    let mut out = vec!["<explain xmlns=\"http://www.postgresql.org/2009/explain\">".to_string()];
    if let Value::Arr(items) = structured(root, options, actual_rows) {
        for item in &items {
            element("Query", item, 1, &mut out);
        }
    }
    out.push("</explain>".into());
    out.join("\n")
}

/// The little JSON writer `FORMAT JSON` needs, laid out as PostgreSQL lays it
/// out (two-space indent, `"key": value`).
mod serde_json_lite {
    pub enum Value {
        Str(String),
        Num(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    pub fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    impl Value {
        pub fn pretty(&self, indent: usize) -> String {
            let pad = " ".repeat(indent);
            let inner = " ".repeat(indent + 2);
            match self {
                Value::Str(s) => quote(s),
                Value::Num(n) => n.clone(),
                Value::Arr(items) if items.is_empty() => "[]".into(),
                Value::Arr(items)
                    if items
                        .iter()
                        .all(|v| matches!(v, Value::Str(_) | Value::Num(_))) =>
                {
                    let parts: Vec<String> = items.iter().map(|v| v.pretty(indent)).collect();
                    format!("[{}]", parts.join(", "))
                }
                Value::Arr(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .map(|v| format!("{inner}{}", v.pretty(indent + 2)))
                        .collect();
                    format!("[\n{}\n{pad}]", parts.join(",\n"))
                }
                Value::Obj(fields) => {
                    let parts: Vec<String> = fields
                        .iter()
                        .map(|(k, v)| format!("{inner}{}: {}", quote(k), v.pretty(indent + 2)))
                        .collect();
                    format!("{{\n{}\n{pad}}}", parts.join(",\n"))
                }
            }
        }
    }
}

/// Put a single-relation statement's WHERE on its scan, as PostgreSQL shows
/// it: the conjuncts an index scan's key constrains as `Index Cond`, the
/// rest as `Filter`. A plan with more than one scan is left alone.
pub(crate) fn attach_scan_quals(
    root: &mut PlanNode,
    quals: &[(String, Option<String>)],
    index_columns: &dyn Fn(&str) -> Vec<String>,
) {
    if quals.is_empty() {
        return;
    }
    fn scans<'a>(n: &'a mut PlanNode, out: &mut Vec<&'a mut PlanNode>) {
        if n.name.starts_with("Seq Scan on ") || n.name.starts_with("Index Scan using ") {
            out.push(n);
            return;
        }
        for c in &mut n.children {
            scans(c, out);
        }
    }
    let mut found = Vec::new();
    scans(root, &mut found);
    let [scan] = found.as_mut_slice() else {
        return;
    };
    let keys = scan
        .props
        .iter()
        .find(|(k, _)| k == "Index Name")
        .map(|(_, v)| index_columns(v))
        .unwrap_or_default();
    let (cond, filter): (Vec<&String>, Vec<&String>) = {
        let mut cond = Vec::new();
        let mut filter = Vec::new();
        for (text, col) in quals {
            if col.as_ref().is_some_and(|c| keys.contains(c)) {
                cond.push(text);
            } else {
                filter.push(text);
            }
        }
        (cond, filter)
    };
    let join = |parts: &[&String]| -> String {
        match parts {
            [one] => (*one).clone(),
            many => format!(
                "({})",
                many.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
        }
    };
    if !cond.is_empty() {
        scan.details.push(("Index Cond".into(), join(&cond)));
    }
    if !filter.is_empty() {
        scan.details.push(("Filter".into(), join(&filter)));
    }
}

/// Put each join's ON condition on its join node (post-order, as
/// `join_conditions` lists them): `Hash Cond` on a hash join, `Join Filter`
/// on a nested loop.
pub(crate) fn attach_join_conds(root: &mut PlanNode, conds: &[String]) {
    fn walk(n: &mut PlanNode, conds: &[String], next: &mut usize) {
        for c in &mut n.children {
            walk(c, conds, next);
        }
        let label = if n.name.starts_with("Hash") && n.name.ends_with("Join") {
            "Hash Cond"
        } else if n.name.starts_with("Nested Loop") {
            "Join Filter"
        } else {
            return;
        };
        if let Some(c) = conds.get(*next) {
            if !c.is_empty() {
                n.details.insert(0, (label.into(), c.clone()));
            }
        }
        *next += 1;
    }
    if conds.is_empty() {
        return;
    }
    let mut next = 0;
    walk(root, conds, &mut next);
}

/// Place a joined SELECT's WHERE conjuncts: one naming a single relation
/// that no outer join null-extends filters that relation's scan; any other
/// filters the top join.
pub(crate) fn attach_join_where(root: &mut PlanNode, quals: &[(String, Option<String>)]) {
    if quals.is_empty() {
        return;
    }
    fn is_scan(n: &PlanNode) -> bool {
        n.name.starts_with("Seq Scan on ") || n.name.starts_with("Index Scan using ")
    }
    fn scan_for<'a>(n: &'a mut PlanNode, rel: &str) -> Option<&'a mut PlanNode> {
        if is_scan(n) {
            let name = n
                .props
                .iter()
                .find(|(k, _)| k == "Alias")
                .or_else(|| n.props.iter().find(|(k, _)| k == "Relation Name"))
                .map(|(_, v)| v.clone());
            return (name.as_deref() == Some(rel)).then_some(n);
        }
        n.children.iter_mut().find_map(|c| scan_for(c, rel))
    }
    fn top_join(n: &mut PlanNode) -> Option<&mut PlanNode> {
        if n.name.contains("Join") || n.name.starts_with("Nested Loop") {
            return Some(n);
        }
        n.children.iter_mut().find_map(top_join)
    }
    let combine = |parts: &[&String]| -> String {
        match parts {
            [one] => (*one).clone(),
            many => format!(
                "({})",
                many.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
        }
    };
    let mut homes: Vec<(Option<String>, Vec<&String>)> = Vec::new();
    for (text, home) in quals {
        match homes.iter_mut().find(|(h, _)| h == home) {
            Some((_, v)) => v.push(text),
            None => homes.push((home.clone(), vec![text])),
        }
    }
    for (home, parts) in homes {
        let target = match &home {
            Some(rel) => scan_for(root, rel),
            None => top_join(root),
        };
        if let Some(n) = target {
            // Over an inner join a WHERE conjunct is a join qual.
            let inner = home.is_none()
                && (n.name == "Hash Join" || n.name == "Nested Loop" || n.name == "Merge Join");
            let label = if inner { "Join Filter" } else { "Filter" };
            n.details.push((label.into(), combine(&parts)));
        }
    }
}
