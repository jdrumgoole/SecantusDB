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

/// PostgreSQL 15's built-in casts: `(oid, source, target, function oid,
/// context, method)`.
const BUILTIN_CASTS: &[(i64, i64, i64, i64, u8, u8)] = &[
    (10000, 20, 21, 714, b'a', b'f'),
    (10001, 20, 23, 480, b'a', b'f'),
    (10002, 20, 700, 652, b'i', b'f'),
    (10003, 20, 701, 482, b'i', b'f'),
    (10004, 20, 1700, 1781, b'i', b'f'),
    (10005, 21, 20, 754, b'i', b'f'),
    (10006, 21, 23, 313, b'i', b'f'),
    (10007, 21, 700, 236, b'i', b'f'),
    (10008, 21, 701, 235, b'i', b'f'),
    (10009, 21, 1700, 1782, b'i', b'f'),
    (10010, 23, 20, 481, b'i', b'f'),
    (10011, 23, 21, 314, b'a', b'f'),
    (10012, 23, 700, 318, b'i', b'f'),
    (10013, 23, 701, 316, b'i', b'f'),
    (10014, 23, 1700, 1740, b'i', b'f'),
    (10015, 700, 20, 653, b'a', b'f'),
    (10016, 700, 21, 238, b'a', b'f'),
    (10017, 700, 23, 319, b'a', b'f'),
    (10018, 700, 701, 311, b'i', b'f'),
    (10019, 700, 1700, 1742, b'a', b'f'),
    (10020, 701, 20, 483, b'a', b'f'),
    (10021, 701, 21, 237, b'a', b'f'),
    (10022, 701, 23, 317, b'a', b'f'),
    (10023, 701, 700, 312, b'a', b'f'),
    (10024, 701, 1700, 1743, b'a', b'f'),
    (10025, 1700, 20, 1779, b'a', b'f'),
    (10026, 1700, 21, 1783, b'a', b'f'),
    (10027, 1700, 23, 1744, b'a', b'f'),
    (10028, 1700, 700, 1745, b'i', b'f'),
    (10029, 1700, 701, 1746, b'i', b'f'),
    (10030, 790, 1700, 3823, b'a', b'f'),
    (10031, 1700, 790, 3824, b'a', b'f'),
    (10032, 23, 790, 3811, b'a', b'f'),
    (10033, 20, 790, 3812, b'a', b'f'),
    (10034, 23, 16, 2557, b'e', b'f'),
    (10035, 16, 23, 2558, b'e', b'f'),
    (10036, 5069, 28, 5071, b'e', b'f'),
    (10037, 20, 26, 1287, b'i', b'f'),
    (10038, 21, 26, 313, b'i', b'f'),
    (10039, 23, 26, 0, b'i', b'b'),
    (10040, 26, 20, 1288, b'a', b'f'),
    (10041, 26, 23, 0, b'a', b'b'),
    (10042, 26, 24, 0, b'i', b'b'),
    (10043, 24, 26, 0, b'i', b'b'),
    (10044, 20, 24, 1287, b'i', b'f'),
    (10045, 21, 24, 313, b'i', b'f'),
    (10046, 23, 24, 0, b'i', b'b'),
    (10047, 24, 20, 1288, b'a', b'f'),
    (10048, 24, 23, 0, b'a', b'b'),
    (10049, 24, 2202, 0, b'i', b'b'),
    (10050, 2202, 24, 0, b'i', b'b'),
    (10051, 26, 2202, 0, b'i', b'b'),
    (10052, 2202, 26, 0, b'i', b'b'),
    (10053, 20, 2202, 1287, b'i', b'f'),
    (10054, 21, 2202, 313, b'i', b'f'),
    (10055, 23, 2202, 0, b'i', b'b'),
    (10056, 2202, 20, 1288, b'a', b'f'),
    (10057, 2202, 23, 0, b'a', b'b'),
    (10058, 26, 2203, 0, b'i', b'b'),
    (10059, 2203, 26, 0, b'i', b'b'),
    (10060, 20, 2203, 1287, b'i', b'f'),
    (10061, 21, 2203, 313, b'i', b'f'),
    (10062, 23, 2203, 0, b'i', b'b'),
    (10063, 2203, 20, 1288, b'a', b'f'),
    (10064, 2203, 23, 0, b'a', b'b'),
    (10065, 2203, 2204, 0, b'i', b'b'),
    (10066, 2204, 2203, 0, b'i', b'b'),
    (10067, 26, 2204, 0, b'i', b'b'),
    (10068, 2204, 26, 0, b'i', b'b'),
    (10069, 20, 2204, 1287, b'i', b'f'),
    (10070, 21, 2204, 313, b'i', b'f'),
    (10071, 23, 2204, 0, b'i', b'b'),
    (10072, 2204, 20, 1288, b'a', b'f'),
    (10073, 2204, 23, 0, b'a', b'b'),
    (10074, 26, 2205, 0, b'i', b'b'),
    (10075, 2205, 26, 0, b'i', b'b'),
    (10076, 20, 2205, 1287, b'i', b'f'),
    (10077, 21, 2205, 313, b'i', b'f'),
    (10078, 23, 2205, 0, b'i', b'b'),
    (10079, 2205, 20, 1288, b'a', b'f'),
    (10080, 2205, 23, 0, b'a', b'b'),
    (10081, 26, 4191, 0, b'i', b'b'),
    (10082, 4191, 26, 0, b'i', b'b'),
    (10083, 20, 4191, 1287, b'i', b'f'),
    (10084, 21, 4191, 313, b'i', b'f'),
    (10085, 23, 4191, 0, b'i', b'b'),
    (10086, 4191, 20, 1288, b'a', b'f'),
    (10087, 4191, 23, 0, b'a', b'b'),
    (10088, 26, 2206, 0, b'i', b'b'),
    (10089, 2206, 26, 0, b'i', b'b'),
    (10090, 20, 2206, 1287, b'i', b'f'),
    (10091, 21, 2206, 313, b'i', b'f'),
    (10092, 23, 2206, 0, b'i', b'b'),
    (10093, 2206, 20, 1288, b'a', b'f'),
    (10094, 2206, 23, 0, b'a', b'b'),
    (10095, 26, 3734, 0, b'i', b'b'),
    (10096, 3734, 26, 0, b'i', b'b'),
    (10097, 20, 3734, 1287, b'i', b'f'),
    (10098, 21, 3734, 313, b'i', b'f'),
    (10099, 23, 3734, 0, b'i', b'b'),
    (10100, 3734, 20, 1288, b'a', b'f'),
    (10101, 3734, 23, 0, b'a', b'b'),
    (10102, 26, 3769, 0, b'i', b'b'),
    (10103, 3769, 26, 0, b'i', b'b'),
    (10104, 20, 3769, 1287, b'i', b'f'),
    (10105, 21, 3769, 313, b'i', b'f'),
    (10106, 23, 3769, 0, b'i', b'b'),
    (10107, 3769, 20, 1288, b'a', b'f'),
    (10108, 3769, 23, 0, b'a', b'b'),
    (10109, 25, 2205, 1079, b'i', b'f'),
    (10110, 1043, 2205, 1079, b'i', b'f'),
    (10111, 26, 4096, 0, b'i', b'b'),
    (10112, 4096, 26, 0, b'i', b'b'),
    (10113, 20, 4096, 1287, b'i', b'f'),
    (10114, 21, 4096, 313, b'i', b'f'),
    (10115, 23, 4096, 0, b'i', b'b'),
    (10116, 4096, 20, 1288, b'a', b'f'),
    (10117, 4096, 23, 0, b'a', b'b'),
    (10118, 26, 4089, 0, b'i', b'b'),
    (10119, 4089, 26, 0, b'i', b'b'),
    (10120, 20, 4089, 1287, b'i', b'f'),
    (10121, 21, 4089, 313, b'i', b'f'),
    (10122, 23, 4089, 0, b'i', b'b'),
    (10123, 4089, 20, 1288, b'a', b'f'),
    (10124, 4089, 23, 0, b'a', b'b'),
    (10125, 25, 1042, 0, b'i', b'b'),
    (10126, 25, 1043, 0, b'i', b'b'),
    (10127, 1042, 25, 401, b'i', b'f'),
    (10128, 1042, 1043, 401, b'i', b'f'),
    (10129, 1043, 25, 0, b'i', b'b'),
    (10130, 1043, 1042, 0, b'i', b'b'),
    (10131, 18, 25, 946, b'i', b'f'),
    (10132, 18, 1042, 860, b'a', b'f'),
    (10133, 18, 1043, 946, b'a', b'f'),
    (10134, 19, 25, 406, b'i', b'f'),
    (10135, 19, 1042, 408, b'a', b'f'),
    (10136, 19, 1043, 1401, b'a', b'f'),
    (10137, 25, 18, 944, b'a', b'f'),
    (10138, 1042, 18, 944, b'a', b'f'),
    (10139, 1043, 18, 944, b'a', b'f'),
    (10140, 25, 19, 407, b'i', b'f'),
    (10141, 1042, 19, 409, b'i', b'f'),
    (10142, 1043, 19, 1400, b'i', b'f'),
    (10143, 18, 23, 77, b'e', b'f'),
    (10144, 23, 18, 78, b'e', b'f'),
    (10145, 194, 25, 0, b'i', b'b'),
    (10146, 3361, 17, 0, b'i', b'b'),
    (10147, 3361, 25, 0, b'i', b'i'),
    (10148, 3402, 17, 0, b'i', b'b'),
    (10149, 3402, 25, 0, b'i', b'i'),
    (10150, 5017, 17, 0, b'i', b'b'),
    (10151, 5017, 25, 0, b'i', b'i'),
    (10152, 1082, 1114, 2024, b'i', b'f'),
    (10153, 1082, 1184, 1174, b'i', b'f'),
    (10154, 1083, 1186, 1370, b'i', b'f'),
    (10155, 1083, 1266, 2047, b'i', b'f'),
    (10156, 1114, 1082, 2029, b'a', b'f'),
    (10157, 1114, 1083, 1316, b'a', b'f'),
    (10158, 1114, 1184, 2028, b'i', b'f'),
    (10159, 1184, 1082, 1178, b'a', b'f'),
    (10160, 1184, 1083, 2019, b'a', b'f'),
    (10161, 1184, 1114, 2027, b'a', b'f'),
    (10162, 1184, 1266, 1388, b'a', b'f'),
    (10163, 1186, 1083, 1419, b'a', b'f'),
    (10164, 1266, 1083, 2046, b'a', b'f'),
    (10165, 600, 603, 4091, b'a', b'f'),
    (10166, 601, 600, 1532, b'e', b'f'),
    (10167, 602, 604, 1449, b'a', b'f'),
    (10168, 603, 600, 1534, b'e', b'f'),
    (10169, 603, 601, 1541, b'e', b'f'),
    (10170, 603, 604, 1448, b'a', b'f'),
    (10171, 603, 718, 1479, b'e', b'f'),
    (10172, 604, 600, 1540, b'e', b'f'),
    (10173, 604, 602, 1447, b'a', b'f'),
    (10174, 604, 603, 1446, b'e', b'f'),
    (10175, 604, 718, 1474, b'e', b'f'),
    (10176, 718, 600, 1416, b'e', b'f'),
    (10177, 718, 603, 1480, b'e', b'f'),
    (10178, 718, 604, 1544, b'e', b'f'),
    (10179, 829, 774, 4123, b'i', b'f'),
    (10180, 774, 829, 4124, b'i', b'f'),
    (10181, 650, 869, 0, b'i', b'b'),
    (10182, 869, 650, 1715, b'a', b'f'),
    (10183, 1560, 1562, 0, b'i', b'b'),
    (10184, 1562, 1560, 0, b'i', b'b'),
    (10185, 20, 1560, 2075, b'e', b'f'),
    (10186, 23, 1560, 1683, b'e', b'f'),
    (10187, 1560, 20, 2076, b'e', b'f'),
    (10188, 1560, 23, 1684, b'e', b'f'),
    (10189, 650, 25, 730, b'a', b'f'),
    (10190, 869, 25, 730, b'a', b'f'),
    (10191, 16, 25, 2971, b'a', b'f'),
    (10192, 142, 25, 0, b'a', b'b'),
    (10193, 25, 142, 2896, b'e', b'f'),
    (10194, 650, 1043, 730, b'a', b'f'),
    (10195, 869, 1043, 730, b'a', b'f'),
    (10196, 16, 1043, 2971, b'a', b'f'),
    (10197, 142, 1043, 0, b'a', b'b'),
    (10198, 1043, 142, 2896, b'e', b'f'),
    (10199, 650, 1042, 730, b'a', b'f'),
    (10200, 869, 1042, 730, b'a', b'f'),
    (10201, 16, 1042, 2971, b'a', b'f'),
    (10202, 142, 1042, 0, b'a', b'b'),
    (10203, 1042, 142, 2896, b'e', b'f'),
    (10204, 1042, 1042, 668, b'i', b'f'),
    (10205, 1043, 1043, 669, b'i', b'f'),
    (10206, 1083, 1083, 1968, b'i', b'f'),
    (10207, 1114, 1114, 1961, b'i', b'f'),
    (10208, 1184, 1184, 1967, b'i', b'f'),
    (10209, 1186, 1186, 1200, b'i', b'f'),
    (10210, 1266, 1266, 1969, b'i', b'f'),
    (10211, 1560, 1560, 1685, b'i', b'f'),
    (10212, 1562, 1562, 1687, b'i', b'f'),
    (10213, 1700, 1700, 1703, b'i', b'f'),
    (10214, 114, 3802, 0, b'a', b'i'),
    (10215, 3802, 114, 0, b'a', b'i'),
    (10216, 3802, 16, 3556, b'e', b'f'),
    (10217, 3802, 1700, 3449, b'e', b'f'),
    (10218, 3802, 21, 3450, b'e', b'f'),
    (10219, 3802, 23, 3451, b'e', b'f'),
    (10220, 3802, 20, 3452, b'e', b'f'),
    (10221, 3802, 700, 3453, b'e', b'f'),
    (10222, 3802, 701, 2580, b'e', b'f'),
    (10223, 3904, 4451, 4281, b'e', b'f'),
    (10224, 3926, 4536, 4296, b'e', b'f'),
    (10225, 3906, 4532, 4284, b'e', b'f'),
    (10226, 3912, 4535, 4293, b'e', b'f'),
    (10227, 3908, 4533, 4287, b'e', b'f'),
    (10228, 3910, 4534, 4290, b'e', b'f'),
];

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
