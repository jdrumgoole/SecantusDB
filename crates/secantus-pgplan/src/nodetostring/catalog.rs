//! The catalog rows parse analysis reads, and the lookups PostgreSQL makes on
//! them: `lsyscache.c`, `typcache.c`'s operator choice, `namespace.c`'s
//! name resolution. The rows are the server's own catalogs (`pg_type`,
//! `pg_proc`, `pg_operator`, ...), handed over by the server as data.

use super::nodes::Oid;
use std::collections::HashMap;

pub const BOOLOID: Oid = 16;
pub const BYTEAOID: Oid = 17;
pub const CHAROID: Oid = 18;
pub const NAMEOID: Oid = 19;
pub const INT8OID: Oid = 20;
pub const INT2OID: Oid = 21;
pub const INT2VECTOROID: Oid = 22;
pub const INT4OID: Oid = 23;
pub const TEXTOID: Oid = 25;
pub const OIDOID: Oid = 26;
pub const OIDVECTOROID: Oid = 30;
pub const FLOAT4OID: Oid = 700;
pub const FLOAT8OID: Oid = 701;
pub const UNKNOWNOID: Oid = 705;
pub const BPCHAROID: Oid = 1042;
pub const VARCHAROID: Oid = 1043;
pub const DATEOID: Oid = 1082;
pub const TIMEOID: Oid = 1083;
pub const TIMESTAMPOID: Oid = 1114;
pub const TIMESTAMPTZOID: Oid = 1184;
pub const UUIDOID: Oid = 2950;
pub const INTERVALOID: Oid = 1186;
pub const TIMETZOID: Oid = 1266;
pub const NUMERICOID: Oid = 1700;
pub const RECORDOID: Oid = 2249;
pub const RECORDARRAYOID: Oid = 2287;
pub const CSTRINGOID: Oid = 2275;
pub const ANYOID: Oid = 2276;
pub const ANYARRAYOID: Oid = 2277;
pub const VOIDOID: Oid = 2278;
pub const INTERNALOID: Oid = 2281;
pub const ANYELEMENTOID: Oid = 2283;
pub const ANYNONARRAYOID: Oid = 2776;
pub const ANYENUMOID: Oid = 3500;
pub const ANYRANGEOID: Oid = 3831;
pub const ANYMULTIRANGEOID: Oid = 4537;
pub const ANYCOMPATIBLEOID: Oid = 5077;
pub const ANYCOMPATIBLEARRAYOID: Oid = 5078;
pub const ANYCOMPATIBLENONARRAYOID: Oid = 5079;
pub const ANYCOMPATIBLERANGEOID: Oid = 5080;
pub const ANYCOMPATIBLEMULTIRANGEOID: Oid = 4538;
pub const INT2ARRAYOID: Oid = 1005;
pub const OIDARRAYOID: Oid = 1028;
pub const TEXTARRAYOID: Oid = 1009;
pub const REGPROCOID: Oid = 24;
pub const REGCLASSOID: Oid = 2205;
pub const REGTYPEOID: Oid = 2206;
pub const REGPROCEDUREOID: Oid = 2202;
pub const REGOPEROID: Oid = 2203;
pub const REGOPERATOROID: Oid = 2204;
pub const REGNAMESPACEOID: Oid = 4089;
pub const REGROLEOID: Oid = 4096;
pub const REGCONFIGOID: Oid = 3734;
pub const REGDICTIONARYOID: Oid = 3769;
pub const REGCOLLATIONOID: Oid = 4191;

pub const DEFAULT_COLLATION_OID: Oid = 100;
pub const C_COLLATION_OID: Oid = 950;
pub const PG_CATALOG_NAMESPACE: Oid = 11;
pub const BTREE_AM_OID: Oid = 403;
pub const HASH_AM_OID: Oid = 405;
pub const F_ARRAY_SUBSCRIPT_HANDLER: Oid = 6179;
pub const ARRAY_EQ_OP: Oid = 1070;
pub const ARRAY_LT_OP: Oid = 1072;
pub const ARRAY_GT_OP: Oid = 1073;
pub const RECORD_EQ_OP: Oid = 2988;
pub const RECORD_LT_OP: Oid = 2990;
pub const RECORD_GT_OP: Oid = 2991;
pub const F_BTARRAYCMP: Oid = 382;
pub const F_BTRECORDCMP: Oid = 2987;
pub const F_HASH_ARRAY: Oid = 626;
pub const F_HASH_RECORD: Oid = 6192;
pub const F_HASH_RANGE: Oid = 3902;
pub const F_HASH_MULTIRANGE: Oid = 4278;

#[derive(Clone, Debug)]
pub struct TypeRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub len: i16,
    pub byval: bool,
    pub align: char,
    pub typtype: char,
    pub category: char,
    pub preferred: bool,
    pub isdefined: bool,
    pub relid: Oid,
    pub elem: Oid,
    pub array: Oid,
    pub basetype: Oid,
    pub typmod: i32,
    pub collation: Oid,
    pub subscript: Oid,
}

#[derive(Clone, Debug)]
pub struct ProcRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub kind: char,
    pub retset: bool,
    pub rettype: Oid,
    pub nargs: i32,
    pub nargdefaults: i32,
    pub argtypes: Vec<Oid>,
    pub allargtypes: Option<Vec<Oid>>,
    pub argmodes: Option<Vec<char>>,
    pub argnames: Option<Vec<String>>,
    pub variadic: Oid,
    /// The defaults, as SQL expressions in parameter order (the server keeps
    /// them deparsed).
    pub argdefaults: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OperRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub kind: char,
    pub left: Oid,
    pub right: Oid,
    pub result: Oid,
    pub code: Oid,
    pub com: Oid,
    pub negate: Oid,
}

#[derive(Clone, Debug)]
pub struct CastRow {
    pub source: Oid,
    pub target: Oid,
    pub func: Oid,
    pub context: char,
    pub method: char,
}

#[derive(Clone, Debug)]
pub struct AggRow {
    pub fnoid: Oid,
    pub kind: char,
    pub numdirectargs: i32,
    pub transtype: Oid,
}

#[derive(Clone, Debug)]
pub struct OpclassRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub method: Oid,
    pub family: Oid,
    pub intype: Oid,
    pub default: bool,
}

#[derive(Clone, Debug)]
pub struct AmopRow {
    pub family: Oid,
    pub lefttype: Oid,
    pub righttype: Oid,
    pub strategy: i32,
    pub purpose: char,
    pub opr: Oid,
    pub method: Oid,
}

#[derive(Clone, Debug)]
pub struct AmprocRow {
    pub family: Oid,
    pub lefttype: Oid,
    pub righttype: Oid,
    pub num: i32,
    pub proc_: Oid,
}

#[derive(Clone, Debug)]
pub struct RelRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub kind: char,
    pub reltype: Oid,
    pub reloftype: Oid,
}

#[derive(Clone, Debug)]
pub struct AttRow {
    pub relid: Oid,
    pub num: i32,
    pub name: String,
    pub typid: Oid,
    pub typmod: i32,
    pub collation: Oid,
    pub dropped: bool,
}

#[derive(Clone, Debug)]
pub struct CollRow {
    pub oid: Oid,
    pub name: String,
    pub nsp: Oid,
    pub encoding: i32,
}

/// A `pg_constraint` row of a relation.
#[derive(Clone, Debug)]
pub struct ConRow {
    pub oid: Oid,
    pub relid: Oid,
    pub name: String,
    pub contype: char,
    pub conkey: Vec<i16>,
    pub deferrable: bool,
}

#[derive(Clone, Debug)]
pub struct RangeRow {
    pub typid: Oid,
    pub subtype: Oid,
    pub multitypid: Oid,
    pub collation: Oid,
}

/// The catalog rows, as the server hands them over.
#[derive(Clone, Debug, Default)]
pub struct CatalogData {
    pub namespaces: Vec<(Oid, String)>,
    pub relations: Vec<RelRow>,
    pub attributes: Vec<AttRow>,
    pub types: Vec<TypeRow>,
    pub procs: Vec<ProcRow>,
    pub operators: Vec<OperRow>,
    pub casts: Vec<CastRow>,
    pub aggregates: Vec<AggRow>,
    pub opclasses: Vec<OpclassRow>,
    pub amops: Vec<AmopRow>,
    pub amprocs: Vec<AmprocRow>,
    pub collations: Vec<CollRow>,
    pub ranges: Vec<RangeRow>,
    /// (child, parent).
    pub inherits: Vec<(Oid, Oid)>,
    /// Constraints on relations.
    pub constraints: Vec<ConRow>,
    /// The database's name (`current_database()`, a 4-part name's check).
    pub database: String,
    /// The server encoding's number, for collation lookup.
    pub encoding: i32,
}

/// The catalog, indexed for lookups.
pub struct Catalog {
    pub data: CatalogData,
    types: HashMap<Oid, usize>,
    types_by_name: HashMap<String, Vec<usize>>,
    procs: HashMap<Oid, usize>,
    procs_by_name: HashMap<String, Vec<usize>>,
    opers: HashMap<Oid, usize>,
    opers_by_name: HashMap<String, Vec<usize>>,
    casts: HashMap<(Oid, Oid), usize>,
    aggs: HashMap<Oid, usize>,
    rels: HashMap<Oid, usize>,
    rels_by_name: HashMap<String, Vec<usize>>,
    atts: HashMap<Oid, Vec<usize>>,
    nsp_by_name: HashMap<String, Oid>,
    nsp_by_oid: HashMap<Oid, String>,
    ranges: HashMap<Oid, usize>,
    multiranges: HashMap<Oid, usize>,
}

impl Catalog {
    pub fn new(data: CatalogData) -> Self {
        let mut c = Catalog {
            types: HashMap::new(),
            types_by_name: HashMap::new(),
            procs: HashMap::new(),
            procs_by_name: HashMap::new(),
            opers: HashMap::new(),
            opers_by_name: HashMap::new(),
            casts: HashMap::new(),
            aggs: HashMap::new(),
            rels: HashMap::new(),
            rels_by_name: HashMap::new(),
            atts: HashMap::new(),
            nsp_by_name: HashMap::new(),
            nsp_by_oid: HashMap::new(),
            ranges: HashMap::new(),
            multiranges: HashMap::new(),
            data,
        };
        for (i, t) in c.data.types.iter().enumerate() {
            c.types.insert(t.oid, i);
            c.types_by_name.entry(t.name.clone()).or_default().push(i);
        }
        // `pg_proc` scanned in its name index order (name, argument types,
        // namespace), as `SearchSysCacheList` returns it.
        let mut order: Vec<usize> = (0..c.data.procs.len()).collect();
        order.sort_by(|&a, &b| {
            let (x, y) = (&c.data.procs[a], &c.data.procs[b]);
            x.name
                .cmp(&y.name)
                .then_with(|| x.argtypes.cmp(&y.argtypes))
                .then_with(|| x.nsp.cmp(&y.nsp))
        });
        for i in order {
            let p = &c.data.procs[i];
            c.procs.insert(p.oid, i);
            c.procs_by_name.entry(p.name.clone()).or_default().push(i);
        }
        let mut order: Vec<usize> = (0..c.data.operators.len()).collect();
        order.sort_by(|&a, &b| {
            let (x, y) = (&c.data.operators[a], &c.data.operators[b]);
            x.name
                .cmp(&y.name)
                .then_with(|| x.left.cmp(&y.left))
                .then_with(|| x.right.cmp(&y.right))
                .then_with(|| x.nsp.cmp(&y.nsp))
        });
        for i in order {
            let o = &c.data.operators[i];
            c.opers.insert(o.oid, i);
            c.opers_by_name.entry(o.name.clone()).or_default().push(i);
        }
        for (i, k) in c.data.casts.iter().enumerate() {
            c.casts.insert((k.source, k.target), i);
        }
        for (i, a) in c.data.aggregates.iter().enumerate() {
            c.aggs.insert(a.fnoid, i);
        }
        for (i, r) in c.data.relations.iter().enumerate() {
            c.rels.insert(r.oid, i);
            c.rels_by_name.entry(r.name.clone()).or_default().push(i);
        }
        for (i, a) in c.data.attributes.iter().enumerate() {
            c.atts.entry(a.relid).or_default().push(i);
        }
        let atts = &c.data.attributes;
        for v in c.atts.values_mut() {
            v.sort_by_key(|&i| atts[i].num);
        }
        for (oid, name) in &c.data.namespaces {
            c.nsp_by_name.insert(name.clone(), *oid);
            c.nsp_by_oid.insert(*oid, name.clone());
        }
        for (i, r) in c.data.ranges.iter().enumerate() {
            c.ranges.insert(r.typid, i);
            c.multiranges.insert(r.multitypid, i);
        }
        c
    }

    /// `format_type_with_typemod`.
    pub fn format_type_with_typemod(&self, t: Oid, typmod: i32) -> String {
        let Some(row) = self.type_row(t) else {
            return "???".into();
        };
        if row.elem != 0 && row.len == -1 && row.subscript == F_ARRAY_SUBSCRIPT_HANDLER {
            return format!("{}[]", self.format_type_with_typemod(row.elem, typmod));
        }
        crate::scalar::format_type_text(&row.name, Some(typmod))
    }

    /// A collation's name, as error messages print it.
    pub fn collation_name(&self, c: Oid) -> String {
        self.data
            .collations
            .iter()
            .find(|r| r.oid == c)
            .map_or_else(|| "???".into(), |r| r.name.clone())
    }

    /// `format_type_be`: a type's name as error messages print it (an
    /// array as its element's name and `[]`; a type outside `pg_catalog`
    /// and `public` qualified by its schema).
    pub fn format_type_be(&self, t: Oid) -> String {
        let Some(row) = self.type_row(t) else {
            return "???".into();
        };
        if row.elem != 0 && row.len == -1 && row.subscript == F_ARRAY_SUBSCRIPT_HANDLER {
            return format!("{}[]", self.format_type_be(row.elem));
        }
        let name = crate::scalar::format_type_text(&row.name, None);
        if row.nsp == PG_CATALOG_NAMESPACE || self.namespace_name(row.nsp) == Some("public") {
            name
        } else {
            format!("{}.{name}", self.namespace_name(row.nsp).unwrap_or("???"))
        }
    }

    pub fn type_row(&self, oid: Oid) -> Option<&TypeRow> {
        self.types.get(&oid).map(|&i| &self.data.types[i])
    }

    pub fn proc_row(&self, oid: Oid) -> Option<&ProcRow> {
        self.procs.get(&oid).map(|&i| &self.data.procs[i])
    }

    pub fn oper_row(&self, oid: Oid) -> Option<&OperRow> {
        self.opers.get(&oid).map(|&i| &self.data.operators[i])
    }

    pub fn cast(&self, source: Oid, target: Oid) -> Option<&CastRow> {
        self.casts
            .get(&(source, target))
            .map(|&i| &self.data.casts[i])
    }

    pub fn aggregate(&self, fnoid: Oid) -> Option<&AggRow> {
        self.aggs.get(&fnoid).map(|&i| &self.data.aggregates[i])
    }

    pub fn rel(&self, oid: Oid) -> Option<&RelRow> {
        self.rels.get(&oid).map(|&i| &self.data.relations[i])
    }

    /// A relation's columns, attnum 1.. in order (dropped ones included).
    pub fn rel_columns(&self, relid: Oid) -> Vec<&AttRow> {
        self.atts
            .get(&relid)
            .map(|v| {
                v.iter()
                    .map(|&i| &self.data.attributes[i])
                    .filter(|a| a.num > 0)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn system_attribute(&self, relid: Oid, name: &str) -> Option<&AttRow> {
        self.atts.get(&relid).and_then(|v| {
            v.iter()
                .map(|&i| &self.data.attributes[i])
                .find(|a| a.num < 0 && a.name == name)
        })
    }

    pub fn namespace_oid(&self, name: &str) -> Option<Oid> {
        self.nsp_by_name.get(name).copied()
    }

    pub fn namespace_name(&self, oid: Oid) -> Option<&str> {
        self.nsp_by_oid.get(&oid).map(String::as_str)
    }

    pub fn procs_named(&self, name: &str) -> impl Iterator<Item = &ProcRow> {
        self.procs_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &self.data.procs[i])
    }

    pub fn opers_named(&self, name: &str) -> impl Iterator<Item = &OperRow> {
        self.opers_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &self.data.operators[i])
    }

    pub fn types_named(&self, name: &str) -> impl Iterator<Item = &TypeRow> {
        self.types_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &self.data.types[i])
    }

    pub fn rels_named(&self, name: &str) -> impl Iterator<Item = &RelRow> {
        self.rels_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&i| &self.data.relations[i])
    }

    // ---- lsyscache.c ----

    pub fn typlen(&self, t: Oid) -> i16 {
        self.type_row(t).map_or(-1, |r| r.len)
    }

    pub fn typbyval(&self, t: Oid) -> bool {
        self.type_row(t).is_some_and(|r| r.byval)
    }

    pub fn typtype(&self, t: Oid) -> char {
        self.type_row(t).map_or('\0', |r| r.typtype)
    }

    pub fn get_typcollation(&self, t: Oid) -> Oid {
        self.type_row(t).map_or(0, |r| r.collation)
    }

    pub fn type_is_collatable(&self, t: Oid) -> bool {
        self.get_typcollation(t) != 0
    }

    /// `getBaseTypeAndTypmod`.
    pub fn base_type_and_typmod(&self, mut t: Oid, typmod: &mut i32) -> Oid {
        loop {
            let Some(r) = self.type_row(t) else {
                return t;
            };
            if r.typtype != 'd' {
                return t;
            }
            t = r.basetype;
            *typmod = r.typmod;
        }
    }

    pub fn base_type(&self, t: Oid) -> Oid {
        let mut m = -1;
        self.base_type_and_typmod(t, &mut m)
    }

    /// `get_element_type`: the element of a true array type.
    pub fn element_type(&self, t: Oid) -> Oid {
        match self.type_row(t) {
            Some(r) if r.len == -1 && r.subscript == F_ARRAY_SUBSCRIPT_HANDLER => r.elem,
            _ => 0,
        }
    }

    pub fn array_type(&self, t: Oid) -> Oid {
        self.type_row(t).map_or(0, |r| r.array)
    }

    /// `get_promoted_array_type`.
    pub fn promoted_array_type(&self, t: Oid) -> Oid {
        let a = self.array_type(t);
        if a != 0 {
            return a;
        }
        if self.element_type(t) != 0 {
            return t;
        }
        0
    }

    /// `get_base_element_type`.
    pub fn base_element_type(&self, t: Oid) -> Oid {
        self.element_type(self.base_type(t))
    }

    pub fn type_is_array(&self, t: Oid) -> bool {
        self.element_type(t) != 0
    }

    /// `type_is_array_domain`.
    pub fn type_is_array_domain(&self, t: Oid) -> bool {
        self.element_type(self.base_type(t)) != 0
    }

    pub fn type_is_enum(&self, t: Oid) -> bool {
        self.typtype(t) == 'e'
    }

    pub fn type_is_range(&self, t: Oid) -> bool {
        self.typtype(t) == 'r'
    }

    pub fn type_is_multirange(&self, t: Oid) -> bool {
        self.typtype(t) == 'm'
    }

    /// `type_is_rowtype`.
    pub fn type_is_rowtype(&self, t: Oid) -> bool {
        if t == RECORDOID {
            return true;
        }
        match self.typtype(t) {
            'c' => true,
            'd' => self.typtype(self.base_type(t)) == 'c',
            _ => false,
        }
    }

    /// `ISCOMPLEX`: a named composite type.
    pub fn is_complex(&self, t: Oid) -> bool {
        self.type_row(t).is_some_and(|r| r.relid != 0)
    }

    pub fn typ_relid(&self, t: Oid) -> Oid {
        self.type_row(t).map_or(0, |r| r.relid)
    }

    /// `typeOrDomainTypeRelid`.
    pub fn type_or_domain_type_relid(&self, t: Oid) -> Oid {
        self.typ_relid(self.base_type(t))
    }

    pub fn range_subtype(&self, t: Oid) -> Oid {
        self.ranges
            .get(&t)
            .map_or(0, |&i| self.data.ranges[i].subtype)
    }

    pub fn range_multirange(&self, t: Oid) -> Oid {
        self.ranges
            .get(&t)
            .map_or(0, |&i| self.data.ranges[i].multitypid)
    }

    pub fn multirange_range(&self, t: Oid) -> Oid {
        self.multiranges
            .get(&t)
            .map_or(0, |&i| self.data.ranges[i].typid)
    }

    /// `get_type_category_preferred`.
    pub fn category_preferred(&self, t: Oid) -> (char, bool) {
        self.type_row(t)
            .map_or(('\0', false), |r| (r.category, r.preferred))
    }

    pub fn type_category(&self, t: Oid) -> char {
        self.category_preferred(t).0
    }

    /// `IsPreferredType`.
    pub fn is_preferred_type(&self, category: char, t: Oid) -> bool {
        let (c, p) = self.category_preferred(t);
        if category == c || category == '\0' {
            p
        } else {
            false
        }
    }

    pub fn func_retset(&self, f: Oid) -> bool {
        self.proc_row(f).is_some_and(|p| p.retset)
    }

    pub fn func_variadic_type(&self, f: Oid) -> Oid {
        self.proc_row(f).map_or(0, |p| p.variadic)
    }

    pub fn commutator(&self, op: Oid) -> Oid {
        self.oper_row(op).map_or(0, |o| o.com)
    }

    /// `typeInheritsFrom`: is `sub` a row type of a table inheriting,
    /// directly or not, from `sup`'s table?
    pub fn type_inherits_from(&self, sub: Oid, sup: Oid) -> bool {
        let subrel = self.type_or_domain_type_relid(sub);
        let suprel = self.typ_relid(sup);
        if subrel == 0 || suprel == 0 {
            return false;
        }
        let mut stack = vec![subrel];
        let mut seen = std::collections::HashSet::new();
        while let Some(r) = stack.pop() {
            if !seen.insert(r) {
                continue;
            }
            for (child, parent) in &self.data.inherits {
                if *child == r {
                    if *parent == suprel {
                        return true;
                    }
                    stack.push(*parent);
                }
            }
        }
        false
    }

    /// `typeIsOfTypedTable`.
    pub fn type_is_of_typed_table(&self, reltype: Oid, oftype: Oid) -> bool {
        let relid = self.type_or_domain_type_relid(reltype);
        relid != 0 && self.rel(relid).is_some_and(|r| r.reloftype == oftype)
    }

    // ---- typcache.c ----

    /// `GetDefaultOpClass`.
    pub fn default_opclass(&self, type_id: Oid, am: Oid) -> Option<&OpclassRow> {
        let type_id = self.base_type(type_id);
        let tcategory = self.type_category(type_id);
        let mut result = None;
        let (mut nexact, mut ncompatible, mut ncompatiblepreferred) = (0, 0, 0);
        for o in &self.data.opclasses {
            if o.method != am || !o.default {
                continue;
            }
            if o.intype == type_id {
                nexact += 1;
                result = Some(o);
            } else if nexact == 0 && super::coerce::is_binary_coercible(self, type_id, o.intype) {
                if self.is_preferred_type(tcategory, o.intype) {
                    ncompatiblepreferred += 1;
                    result = Some(o);
                } else if ncompatiblepreferred == 0 {
                    ncompatible += 1;
                    result = Some(o);
                }
            }
        }
        if nexact == 1
            || ncompatiblepreferred == 1
            || (ncompatiblepreferred == 0 && ncompatible == 1)
        {
            result
        } else {
            None
        }
    }

    /// `get_opfamily_member`.
    pub fn opfamily_member(&self, family: Oid, l: Oid, r: Oid, strategy: i32) -> Oid {
        self.data
            .amops
            .iter()
            .find(|a| {
                a.family == family && a.lefttype == l && a.righttype == r && a.strategy == strategy
            })
            .map_or(0, |a| a.opr)
    }

    /// `get_primary_key_attnos(relid, deferrableOk = false)`: the primary
    /// key's constraint and columns, none for a deferrable one.
    pub fn primary_key(&self, relid: Oid) -> Option<(Oid, &[i16])> {
        self.data
            .constraints
            .iter()
            .find(|c| c.relid == relid && c.contype == 'p')
            .filter(|c| !c.deferrable)
            .map(|c| (c.oid, c.conkey.as_slice()))
    }

    /// `get_relation_constraint_oid` / `_attnos`: a relation's constraint
    /// by name.
    pub fn relation_constraint(&self, relid: Oid, name: &str) -> Option<&ConRow> {
        self.data
            .constraints
            .iter()
            .find(|c| c.relid == relid && c.name == name)
    }

    /// `get_opfamily_proc`.
    pub fn opfamily_proc(&self, family: Oid, l: Oid, r: Oid, num: i32) -> Oid {
        self.data
            .amprocs
            .iter()
            .find(|a| a.family == family && a.lefttype == l && a.righttype == r && a.num == num)
            .map_or(0, |a| a.proc_)
    }

    /// The typcache entry's operators and support procs for `t`.
    pub fn typcache(&self, t: Oid) -> TypCache {
        self.typcache_depth(t, 0)
    }

    fn typcache_depth(&self, t: Oid, depth: u32) -> TypCache {
        let mut tc = TypCache::default();
        if depth > 8 {
            return tc;
        }
        let btree = self.default_opclass(t, BTREE_AM_OID);
        let (bopf, bin) = btree.map_or((0, 0), |o| (o.family, o.intype));
        let hash = self.default_opclass(t, HASH_AM_OID);
        let (hopf, hin) = hash.map_or((0, 0), |o| (o.family, o.intype));
        // eq_opr
        let mut eq = 0;
        if bopf != 0 {
            eq = self.opfamily_member(bopf, bin, bin, 3);
        }
        if eq == 0 && hopf != 0 {
            eq = self.opfamily_member(hopf, hin, hin, 1);
        }
        let elem_props = || {
            let e = self.base_element_type(t);
            if e == 0 {
                return TypCache::default();
            }
            self.typcache_depth(e, depth + 1)
        };
        let field_props = || -> (bool, bool, bool) {
            // (equality, compare, hashing)
            if t == RECORDOID {
                return (true, true, false);
            }
            let rt = self.typtype(t);
            let relid = match rt {
                'c' => self.typ_relid(t),
                'd' => {
                    let b = self.base_type(t);
                    if self.typtype(b) != 'c' {
                        return (false, false, false);
                    }
                    self.typ_relid(b)
                }
                _ => return (false, false, false),
            };
            let (mut e, mut c, mut h) = (true, true, true);
            for a in self.rel_columns(relid) {
                if a.dropped {
                    continue;
                }
                let f = self.typcache_depth(a.typid, depth + 1);
                e &= f.eq_opr != 0;
                c &= f.cmp_proc != 0;
                h &= f.hash_proc != 0;
            }
            (e, c, h)
        };
        if eq == ARRAY_EQ_OP && elem_props().eq_opr == 0 {
            eq = 0;
        } else if eq == RECORD_EQ_OP && !field_props().0 {
            eq = 0;
        }
        tc.eq_opr = eq;
        let mut lt = if bopf != 0 {
            self.opfamily_member(bopf, bin, bin, 1)
        } else {
            0
        };
        if lt == ARRAY_LT_OP && elem_props().cmp_proc == 0 {
            lt = 0;
        } else if lt == RECORD_LT_OP && !field_props().1 {
            lt = 0;
        }
        tc.lt_opr = lt;
        let mut gt = if bopf != 0 {
            self.opfamily_member(bopf, bin, bin, 5)
        } else {
            0
        };
        if gt == ARRAY_GT_OP && elem_props().cmp_proc == 0 {
            gt = 0;
        } else if gt == RECORD_GT_OP && !field_props().1 {
            gt = 0;
        }
        tc.gt_opr = gt;
        let mut cmp = if bopf != 0 {
            self.opfamily_proc(bopf, bin, bin, 1)
        } else {
            0
        };
        if cmp == F_BTARRAYCMP && elem_props().cmp_proc == 0 {
            cmp = 0;
        } else if cmp == F_BTRECORDCMP && !field_props().1 {
            cmp = 0;
        }
        tc.cmp_proc = cmp;
        let mut hp = 0;
        if hopf != 0 && (eq == 0 || eq == self.opfamily_member(hopf, hin, hin, 1)) {
            hp = self.opfamily_proc(hopf, hin, hin, 1);
        }
        if hp == F_HASH_ARRAY && elem_props().hash_proc == 0 {
            hp = 0;
        } else if hp == F_HASH_RECORD && !field_props().2 {
            hp = 0;
        } else if hp == F_HASH_RANGE {
            let sub = self.range_subtype(t);
            if sub == 0 || self.typcache_depth(sub, depth + 1).hash_proc == 0 {
                hp = 0;
            }
        } else if hp == F_HASH_MULTIRANGE {
            let r = self.multirange_range(t);
            let sub = self.range_subtype(r);
            if sub == 0 || self.typcache_depth(sub, depth + 1).hash_proc == 0 {
                hp = 0;
            }
        }
        tc.hash_proc = hp;
        tc
    }

    /// `get_sort_group_operators`: (lt, eq, gt, hashable).
    pub fn sort_group_operators(&self, t: Oid) -> (Oid, Oid, Oid, bool) {
        let tc = self.typcache(t);
        (tc.lt_opr, tc.eq_opr, tc.gt_opr, tc.hash_proc != 0)
    }

    /// `get_ordering_op_properties`: (opfamily, opcintype, strategy).
    pub fn ordering_op_properties(&self, op: Oid) -> Option<(Oid, Oid, i32)> {
        self.data
            .amops
            .iter()
            .find(|a| {
                a.opr == op
                    && a.method == BTREE_AM_OID
                    && (a.strategy == 1 || a.strategy == 5)
                    && a.lefttype == a.righttype
            })
            .map(|a| (a.family, a.lefttype, a.strategy))
    }

    /// `get_equality_op_for_ordering_op`: (eq op, reverse).
    pub fn equality_op_for_ordering_op(&self, op: Oid) -> Option<(Oid, bool)> {
        let (fam, intype, strategy) = self.ordering_op_properties(op)?;
        let eq = self.opfamily_member(fam, intype, intype, 3);
        (eq != 0).then_some((eq, strategy == 5))
    }

    /// `op_hashjoinable` for the operator's input type.
    pub fn op_hashjoinable(&self, op: Oid, t: Oid) -> bool {
        // An array or record equality is hashable only if its members are.
        if op == ARRAY_EQ_OP || op == RECORD_EQ_OP {
            return self.typcache(t).hash_proc != 0;
        }
        self.data
            .amops
            .iter()
            .any(|a| a.opr == op && a.method == HASH_AM_OID)
    }

    /// `get_op_btree_interpretation`: (opfamily, strategy) pairs.
    pub fn op_btree_interpretation(&self, op: Oid) -> Vec<(Oid, i32)> {
        let mut out: Vec<(Oid, i32)> = self
            .data
            .amops
            .iter()
            .filter(|a| a.opr == op && a.method == BTREE_AM_OID && a.purpose == 's')
            .map(|a| (a.family, a.strategy))
            .collect();
        if out.is_empty() {
            // `<>`: the negator of a btree equality.
            if let Some(neg) = self.negator(op) {
                for a in self.data.amops.iter() {
                    if a.opr == neg && a.method == BTREE_AM_OID && a.strategy == 3 {
                        out.push((a.family, 6));
                    }
                }
            }
        }
        out
    }

    fn negator(&self, op: Oid) -> Option<Oid> {
        self.oper_row(op).map(|o| o.negate).filter(|n| *n != 0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TypCache {
    pub eq_opr: Oid,
    pub lt_opr: Oid,
    pub gt_opr: Oid,
    pub cmp_proc: Oid,
    pub hash_proc: Oid,
}
