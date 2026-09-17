//! The PostGIS `geometry` type, as far as a client that stores and reads
//! shapes needs it.
//!
//! A value is carried as its CANONICAL text: upper-case hex of the
//! little-endian EWKB PostGIS itself prints (`'POINT(1 2)'::geometry` is
//! `0101000000000000000000F03F0000000000000040`, and
//! `SRID=4326;POINT(1 2)` is `0101000020E6100000...`). Binary I/O is that
//! same EWKB, decoded. Three parsers feed it: hex EWKB (any byte order, any
//! nesting of SRID flags -- re-serialised the way PostGIS re-serialises
//! it), WKT / EWKT, and GeoJSON (`ST_GeomFromGeoJSON`, whose default SRID
//! is 4326 and which honours a `crs` naming `EPSG:<n>`).
//!
//! The seven simple-feature types (point, linestring, polygon, the three
//! multis and geometrycollection) with Z / M / ZM dimensions are covered;
//! the curve types are refused as unsupported rather than mis-parsed. Every
//! message here was measured against PostGIS 3.4.6 on PostgreSQL 16.

use crate::{Error, Result};

const Z_FLAG: u32 = 0x8000_0000;
const M_FLAG: u32 = 0x4000_0000;
const SRID_FLAG: u32 = 0x2000_0000;

/// A parsed geometry. `srid` 0 is "none".
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub srid: i32,
    pub z: bool,
    pub m: bool,
    pub shape: Shape,
}

/// A coordinate holds 2, 3 or 4 doubles as the dimension flags say.
pub type Coord = Vec<f64>;

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// `None` is `POINT EMPTY`, serialised as NaN coordinates.
    Point(Option<Coord>),
    LineString(Vec<Coord>),
    Polygon(Vec<Vec<Coord>>),
    MultiPoint(Vec<Shape>),
    MultiLineString(Vec<Shape>),
    MultiPolygon(Vec<Shape>),
    Collection(Vec<Shape>),
}

impl Shape {
    fn type_code(&self) -> u32 {
        match self {
            Shape::Point(_) => 1,
            Shape::LineString(_) => 2,
            Shape::Polygon(_) => 3,
            Shape::MultiPoint(_) => 4,
            Shape::MultiLineString(_) => 5,
            Shape::MultiPolygon(_) => 6,
            Shape::Collection(_) => 7,
        }
    }
}

fn internal(msg: &str) -> Error {
    Error::Internal(msg.to_string())
}

// ---------------------------------------------------------------- EWKB out

fn dims(z: bool, m: bool) -> usize {
    2 + usize::from(z) + usize::from(m)
}

fn put_coord(out: &mut Vec<u8>, c: &Coord, n: usize) {
    for i in 0..n {
        let v = c.get(i).copied().unwrap_or(0.0);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_shape(out: &mut Vec<u8>, shape: &Shape, z: bool, m: bool, srid: i32) {
    out.push(1);
    let mut ty = shape.type_code();
    if z {
        ty |= Z_FLAG;
    }
    if m {
        ty |= M_FLAG;
    }
    if srid != 0 {
        ty |= SRID_FLAG;
    }
    put_u32(out, ty);
    if srid != 0 {
        out.extend_from_slice(&srid.to_le_bytes());
    }
    let n = dims(z, m);
    match shape {
        Shape::Point(Some(c)) => put_coord(out, c, n),
        Shape::Point(None) => {
            for _ in 0..n {
                out.extend_from_slice(&f64::NAN.to_le_bytes());
            }
        }
        Shape::LineString(pts) => {
            put_u32(out, pts.len() as u32);
            for p in pts {
                put_coord(out, p, n);
            }
        }
        Shape::Polygon(rings) => {
            put_u32(out, rings.len() as u32);
            for ring in rings {
                put_u32(out, ring.len() as u32);
                for p in ring {
                    put_coord(out, p, n);
                }
            }
        }
        Shape::MultiPoint(items)
        | Shape::MultiLineString(items)
        | Shape::MultiPolygon(items)
        | Shape::Collection(items) => {
            put_u32(out, items.len() as u32);
            for item in items {
                write_shape(out, item, z, m, 0);
            }
        }
    }
}

/// The little-endian EWKB PostGIS prints.
pub fn to_ewkb(g: &Geometry) -> Vec<u8> {
    let mut out = Vec::new();
    write_shape(&mut out, &g.shape, g.z, g.m, g.srid);
    out
}

/// Upper-case hex EWKB: the type's text output.
pub fn to_hex(g: &Geometry) -> String {
    let mut s = String::with_capacity(0);
    for b in to_ewkb(g) {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

// ---------------------------------------------------------------- WKT out

/// One ordinate as `ST_AsText` prints it (PostGIS's `lwprint_double`,
/// measured on 3.4.6): the shortest round-trip digits, capped at 15 places
/// after the point, in plain form for `1e-7 <= |d| < 1e15` and `1.5e-10` /
/// `1e+15` style otherwise.
fn coord_text(d: f64) -> String {
    if d == 0.0 || !d.is_finite() {
        return if d.is_nan() {
            "NaN".into()
        } else if d.is_infinite() {
            (if d > 0.0 { "Infinity" } else { "-Infinity" }).into()
        } else {
            "0".into()
        };
    }
    let cap15 = |v: f64| -> String {
        let r = (v * 1e15).round() / 1e15;
        let t = format!("{r}");
        if t.contains('.') {
            t.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            t
        }
    };
    let a = d.abs();
    if (1e-7..1e15).contains(&a) {
        return cap15(d);
    }
    let sci = format!("{d:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let mantissa = cap15(mantissa.parse::<f64>().unwrap_or(0.0));
    let exp: i32 = exp.parse().unwrap_or(0);
    format!(
        "{mantissa}e{}{}",
        if exp < 0 { "-" } else { "+" },
        exp.abs()
    )
}

fn coord_wkt(c: &Coord) -> String {
    c.iter()
        .map(|v| coord_text(*v))
        .collect::<Vec<_>>()
        .join(" ")
}

fn coords_wkt(pts: &[Coord]) -> String {
    format!(
        "({})",
        pts.iter().map(coord_wkt).collect::<Vec<_>>().join(",")
    )
}

fn shape_wkt(shape: &Shape, dim: &str) -> String {
    let tag = |name: &str| {
        if dim.is_empty() {
            name.to_string()
        } else {
            format!("{name} {dim} ")
        }
    };
    // `POINT EMPTY` has a space even without a dimension word.
    let empty = |name: &str| {
        if dim.is_empty() {
            format!("{name} EMPTY")
        } else {
            format!("{}EMPTY", tag(name))
        }
    };
    match shape {
        Shape::Point(None) => empty("POINT"),
        Shape::Point(Some(c)) => format!("{}({})", tag("POINT"), coord_wkt(c)),
        Shape::LineString(pts) if pts.is_empty() => empty("LINESTRING"),
        Shape::LineString(pts) => format!("{}{}", tag("LINESTRING"), coords_wkt(pts)),
        Shape::Polygon(rings) if rings.is_empty() => empty("POLYGON"),
        Shape::Polygon(rings) => format!(
            "{}({})",
            tag("POLYGON"),
            rings
                .iter()
                .map(|r| coords_wkt(r))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Shape::MultiPoint(items)
        | Shape::MultiLineString(items)
        | Shape::MultiPolygon(items)
        | Shape::Collection(items) => {
            let name = match shape {
                Shape::MultiPoint(_) => "MULTIPOINT",
                Shape::MultiLineString(_) => "MULTILINESTRING",
                Shape::MultiPolygon(_) => "MULTIPOLYGON",
                _ => "GEOMETRYCOLLECTION",
            };
            if items.is_empty() {
                return empty(name);
            }
            // Members of a multi-shape drop their own type word (and the
            // dimension tag); collection members keep both.
            let inner: Vec<String> = items
                .iter()
                .map(|item| {
                    if matches!(shape, Shape::Collection(_)) {
                        shape_wkt(item, dim)
                    } else {
                        let full = shape_wkt(item, "");
                        let body = full
                            .split_once(|c: char| c == '(' || c == 'E')
                            .map(|(_, rest)| rest)
                            .unwrap_or("");
                        if full.ends_with("EMPTY") {
                            "EMPTY".to_string()
                        } else {
                            format!("({body}")
                        }
                    }
                })
                .collect();
            format!("{}({})", tag(name), inner.join(","))
        }
    }
}

/// `ST_AsText` (and `ST_AsEWKT`, which prefixes `SRID=n;` when set).
pub fn to_wkt(g: &Geometry, ewkt: bool) -> String {
    let dim = match (g.z, g.m) {
        (true, true) => "ZM",
        (true, false) => "Z",
        (false, true) => "M",
        (false, false) => "",
    };
    let body = shape_wkt(&g.shape, dim);
    if ewkt && g.srid != 0 {
        format!("SRID={};{body}", g.srid)
    } else {
        body
    }
}

// ---------------------------------------------------------------- EWKB in

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    le: bool,
}

impl Reader<'_> {
    fn short(&self) -> Error {
        internal("WKB structure does not match expected size!")
    }

    fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.pos).ok_or_else(|| self.short())?;
        self.pos += 1;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32> {
        let s = self
            .b
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.short())?;
        self.pos += 4;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }

    fn f64(&mut self) -> Result<f64> {
        let s = self
            .b
            .get(self.pos..self.pos + 8)
            .ok_or_else(|| self.short())?;
        self.pos += 8;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(if self.le {
            f64::from_le_bytes(a)
        } else {
            f64::from_be_bytes(a)
        })
    }

    fn coord(&mut self, n: usize) -> Result<Coord> {
        (0..n).map(|_| self.f64()).collect()
    }

    fn coords(&mut self, n: usize) -> Result<Vec<Coord>> {
        let count = self.u32()?;
        // Guard against a count that would outrun the buffer by orders of
        // magnitude before allocating for it.
        if (count as usize).saturating_mul(n * 8) > self.b.len() {
            return Err(self.short());
        }
        (0..count).map(|_| self.coord(n)).collect()
    }

    /// One geometry (with its own byte-order byte and type word).
    fn geometry(&mut self) -> Result<Geometry> {
        match self.u8()? {
            0 => self.le = false,
            1 => self.le = true,
            _ => return Err(internal("Invalid endian flag value encountered.")),
        }
        let ty = self.u32()?;
        let z = ty & Z_FLAG != 0;
        let m = ty & M_FLAG != 0;
        let srid = if ty & SRID_FLAG != 0 {
            self.u32()? as i32
        } else {
            0
        };
        let n = dims(z, m);
        let shape = match ty & 0x0000_FFFF {
            1 => {
                let c = self.coord(n)?;
                Shape::Point(if c.iter().all(|v| v.is_nan()) {
                    None
                } else {
                    Some(c)
                })
            }
            2 => Shape::LineString(self.coords(n)?),
            3 => {
                let rings = self.u32()?;
                if (rings as usize).saturating_mul(4) > self.b.len() {
                    return Err(self.short());
                }
                Shape::Polygon((0..rings).map(|_| self.coords(n)).collect::<Result<_>>()?)
            }
            code @ 4..=7 => {
                let count = self.u32()?;
                if (count as usize).saturating_mul(5) > self.b.len() {
                    return Err(self.short());
                }
                let items = (0..count)
                    .map(|_| self.geometry().map(|g| g.shape))
                    .collect::<Result<Vec<_>>>()?;
                match code {
                    4 => Shape::MultiPoint(items),
                    5 => Shape::MultiLineString(items),
                    6 => Shape::MultiPolygon(items),
                    _ => Shape::Collection(items),
                }
            }
            other => return Err(Error::Unsupported(format!("geometry type {other} in WKB"))),
        };
        Ok(Geometry { srid, z, m, shape })
    }
}

/// Parse (E)WKB bytes.
pub fn from_ewkb(bytes: &[u8]) -> Result<Geometry> {
    let mut r = Reader {
        b: bytes,
        pos: 0,
        le: true,
    };
    let g = r.geometry()?;
    if r.pos != bytes.len() {
        return Err(r.short());
    }
    Ok(g)
}

/// Hex text to bytes, `None` if not hex.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    if b.is_empty() || b.len() % 2 != 0 {
        return None;
    }
    let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    b.chunks(2)
        .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect()
}

// ---------------------------------------------------------------- WKT in

struct Wkt<'a> {
    s: &'a str,
    pos: usize,
}

impl Wkt<'_> {
    fn fail(&self) -> Error {
        Error::Internal(format!(
            "parse error - invalid geometry\nHint: \"{}\" <-- parse error at position {} within geometry",
            &self.s[..self.pos.min(self.s.len())],
            self.pos
        ))
    }

    fn ws(&mut self) {
        while self.s[self.pos..].starts_with(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.s[self.pos..].starts_with(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> String {
        self.ws();
        let start = self.pos;
        while self.s[self.pos..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        self.s[start..self.pos].to_ascii_uppercase()
    }

    fn number(&mut self) -> Result<f64> {
        self.ws();
        let start = self.pos;
        while self.s[self.pos..]
            .starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        {
            self.pos += 1;
        }
        let tok = &self.s[start..self.pos];
        if tok.is_empty() {
            self.pos += self.s[self.pos..].chars().next().map_or(0, char::len_utf8);
            return Err(self.fail());
        }
        tok.parse::<f64>().map_err(|_| self.fail())
    }

    fn coord(&mut self, want: &mut Option<usize>) -> Result<Coord> {
        let mut c = vec![self.number()?];
        loop {
            self.ws();
            if self.s[self.pos..]
                .starts_with(|ch: char| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.'))
            {
                c.push(self.number()?);
            } else {
                break;
            }
        }
        // A coordinate is X Y [Z [M]]: one ordinate, or five and more, is
        // PostGIS's parse error at the closing paren (measured on 3.4.6).
        let width_ok = (2..=4).contains(&c.len());
        match want {
            Some(n) if *n != c.len() => {
                self.pos += 1;
                return Err(self.fail());
            }
            Some(_) => {}
            None if width_ok => *want = Some(c.len()),
            None => {
                self.pos += 1;
                return Err(self.fail());
            }
        }
        Ok(c)
    }

    fn coord_list(&mut self, want: &mut Option<usize>) -> Result<Vec<Coord>> {
        if !self.eat('(') {
            self.pos += 1;
            return Err(self.fail());
        }
        let mut out = Vec::new();
        loop {
            out.push(self.coord(want)?);
            if self.eat(',') {
                continue;
            }
            if self.eat(')') {
                return Ok(out);
            }
            self.pos += 1;
            return Err(self.fail());
        }
    }

    fn empty(&mut self) -> bool {
        let save = self.pos;
        if self.word() == "EMPTY" {
            true
        } else {
            self.pos = save;
            false
        }
    }

    /// `TYPE [Z|M|ZM] (...)`. `want` pins the coordinate width once known.
    fn shape(&mut self, want: &mut Option<usize>, zm: &mut Option<(bool, bool)>) -> Result<Shape> {
        let name = self.word();
        if name.is_empty() {
            self.pos += 1;
            return Err(self.fail());
        }
        let (base, suffix) = match name.as_str() {
            n if n.ends_with("ZM") => (&n[..n.len() - 2], Some((true, true))),
            n if n.ends_with('Z') && n != "Z" => (&n[..n.len() - 1], Some((true, false))),
            n if n.ends_with('M') && n != "M" => (&n[..n.len() - 1], Some((false, true))),
            n => (n, None),
        };
        let base = base.to_string();
        // The dimension word may also stand alone: `POINT Z (1 2 3)`.
        let suffix = match suffix {
            Some(s) => Some(s),
            None => {
                let save = self.pos;
                match self.word().as_str() {
                    "ZM" => Some((true, true)),
                    "Z" => Some((true, false)),
                    "M" => Some((false, true)),
                    _ => {
                        self.pos = save;
                        None
                    }
                }
            }
        };
        if let Some((z, m)) = suffix {
            match zm {
                Some(existing) if *existing != (z, m) => return Err(self.fail()),
                _ => *zm = Some((z, m)),
            }
            let n = dims(z, m);
            if want.is_some_and(|w| w != n) {
                return Err(self.fail());
            }
            *want = Some(n);
        }
        if self.empty() {
            return Ok(match base.as_str() {
                "POINT" => Shape::Point(None),
                "LINESTRING" => Shape::LineString(vec![]),
                "POLYGON" => Shape::Polygon(vec![]),
                "MULTIPOINT" => Shape::MultiPoint(vec![]),
                "MULTILINESTRING" => Shape::MultiLineString(vec![]),
                "MULTIPOLYGON" => Shape::MultiPolygon(vec![]),
                "GEOMETRYCOLLECTION" => Shape::Collection(vec![]),
                _ => return Err(self.fail()),
            });
        }
        match base.as_str() {
            "POINT" => {
                let mut pts = self.coord_list(want)?;
                if pts.len() != 1 {
                    return Err(self.fail());
                }
                Ok(Shape::Point(pts.pop()))
            }
            "LINESTRING" => Ok(Shape::LineString(self.coord_list(want)?)),
            "POLYGON" => Ok(Shape::Polygon(self.rings(want)?)),
            "MULTIPOINT" => {
                if !self.eat('(') {
                    return Err(self.fail());
                }
                let mut items = Vec::new();
                loop {
                    // Each point may be bare `1 2` or parenthesised `(1 2)`.
                    let c = if self.eat('(') {
                        let c = self.coord(want)?;
                        if !self.eat(')') {
                            return Err(self.fail());
                        }
                        c
                    } else {
                        self.coord(want)?
                    };
                    items.push(Shape::Point(Some(c)));
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(')') {
                        return Ok(Shape::MultiPoint(items));
                    }
                    return Err(self.fail());
                }
            }
            "MULTILINESTRING" => {
                let lines = self.rings(want)?;
                Ok(Shape::MultiLineString(
                    lines.into_iter().map(Shape::LineString).collect(),
                ))
            }
            "MULTIPOLYGON" => {
                if !self.eat('(') {
                    return Err(self.fail());
                }
                let mut items = Vec::new();
                loop {
                    items.push(Shape::Polygon(self.rings(want)?));
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(')') {
                        return Ok(Shape::MultiPolygon(items));
                    }
                    return Err(self.fail());
                }
            }
            "GEOMETRYCOLLECTION" => {
                if !self.eat('(') {
                    return Err(self.fail());
                }
                let mut items = Vec::new();
                loop {
                    items.push(self.shape(want, zm)?);
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(')') {
                        return Ok(Shape::Collection(items));
                    }
                    return Err(self.fail());
                }
            }
            _ => {
                // Point past the word, as PostGIS does for an unknown type.
                Err(self.fail())
            }
        }
    }

    fn rings(&mut self, want: &mut Option<usize>) -> Result<Vec<Vec<Coord>>> {
        if !self.eat('(') {
            self.pos += 1;
            return Err(self.fail());
        }
        let mut out = Vec::new();
        loop {
            out.push(self.coord_list(want)?);
            if self.eat(',') {
                continue;
            }
            if self.eat(')') {
                return Ok(out);
            }
            self.pos += 1;
            return Err(self.fail());
        }
    }
}

/// Parse WKT or EWKT (`SRID=4326;POINT(1 2)`).
pub fn from_wkt(text: &str) -> Result<Geometry> {
    let mut srid = 0i32;
    let mut body = text.trim_start();
    if let Some(rest) = body
        .strip_prefix("SRID=")
        .or_else(|| body.strip_prefix("srid="))
    {
        let (n, tail) = rest.split_once(';').ok_or_else(|| {
            Error::Internal(format!(
                "parse error - invalid geometry\nHint: \"{text}\" <-- parse error at position {} within geometry",
                text.len()
            ))
        })?;
        srid = n
            .trim()
            .parse::<i32>()
            .map_err(|_| internal("parse error - invalid geometry"))?;
        body = tail;
    }
    let mut p = Wkt { s: body, pos: 0 };
    let mut want = None;
    let mut zm = None;
    let shape = p.shape(&mut want, &mut zm)?;
    p.ws();
    if p.pos != body.len() {
        p.pos += 1;
        return Err(p.fail());
    }
    let (z, m) = match zm {
        Some(zm) => zm,
        None => match want {
            Some(3) => (true, false),
            Some(4) => (true, true),
            _ => (false, false),
        },
    };
    Ok(Geometry { srid, z, m, shape })
}

/// The type's input function: hex EWKB, else WKT / EWKT.
pub fn from_text(text: &str) -> Result<Geometry> {
    if let Some(bytes) = unhex(text.trim()) {
        return from_ewkb(&bytes);
    }
    from_wkt(text)
}

/// Any accepted text form to canonical hex.
pub fn canonical(text: &str) -> Result<String> {
    from_text(text).map(|g| to_hex(&g))
}

// ---------------------------------------------------------------- GeoJSON

use crate::json::{self, Json};

fn coord_from_json(v: &Json) -> Result<Coord> {
    let Json::Array(items) = v else {
        return Err(internal("invalid GeoJson representation"));
    };
    let mut c = Vec::with_capacity(items.len().min(3));
    // PostGIS reads at most X, Y, Z; a fourth ordinate is dropped.
    for item in items.iter().take(3) {
        let n = match item {
            Json::Number(n) => n.parse::<f64>().ok(),
            Json::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        c.push(n.ok_or_else(|| internal("invalid GeoJson representation"))?);
    }
    if c.len() < 2 {
        return Err(internal("invalid GeoJson representation"));
    }
    Ok(c)
}

fn coords_from_json(v: &Json) -> Result<Vec<Coord>> {
    let Json::Array(items) = v else {
        return Err(internal("invalid GeoJson representation"));
    };
    items.iter().map(coord_from_json).collect()
}

fn rings_from_json(v: &Json) -> Result<Vec<Vec<Coord>>> {
    let Json::Array(items) = v else {
        return Err(internal("invalid GeoJson representation"));
    };
    items.iter().map(coords_from_json).collect()
}

fn has_z(shape: &Shape) -> bool {
    match shape {
        Shape::Point(c) => c.as_ref().is_some_and(|c| c.len() >= 3),
        Shape::LineString(pts) => pts.iter().any(|c| c.len() >= 3),
        Shape::Polygon(rings) => rings.iter().flatten().any(|c| c.len() >= 3),
        Shape::MultiPoint(items)
        | Shape::MultiLineString(items)
        | Shape::MultiPolygon(items)
        | Shape::Collection(items) => items.iter().any(has_z),
    }
}

fn shape_from_json(obj: &Json) -> Result<Shape> {
    let Json::Object(_) = obj else {
        return Err(internal("unknown GeoJSON type"));
    };
    let Some(Json::Str(ty)) = json::member(obj, "type") else {
        return Err(internal("unknown GeoJSON type"));
    };
    let coordinates = || {
        json::member(obj, "coordinates")
            .ok_or_else(|| internal("Unable to find 'coordinates' in GeoJSON string"))
    };
    Ok(match ty.as_str() {
        "Point" => {
            let c = coordinates()?;
            match c {
                Json::Array(items) if items.is_empty() => Shape::Point(None),
                other => Shape::Point(Some(coord_from_json(other)?)),
            }
        }
        "LineString" => Shape::LineString(coords_from_json(coordinates()?)?),
        "Polygon" => Shape::Polygon(rings_from_json(coordinates()?)?),
        "MultiPoint" => Shape::MultiPoint(
            coords_from_json(coordinates()?)?
                .into_iter()
                .map(|c| Shape::Point(Some(c)))
                .collect(),
        ),
        "MultiLineString" => Shape::MultiLineString(
            rings_from_json(coordinates()?)?
                .into_iter()
                .map(Shape::LineString)
                .collect(),
        ),
        "MultiPolygon" => {
            let Json::Array(polys) = coordinates()? else {
                return Err(internal("invalid GeoJson representation"));
            };
            Shape::MultiPolygon(
                polys
                    .iter()
                    .map(|p| rings_from_json(p).map(Shape::Polygon))
                    .collect::<Result<_>>()?,
            )
        }
        "GeometryCollection" => {
            let Some(Json::Array(items)) = json::member(obj, "geometries") else {
                return Err(internal("Unable to find 'geometries' in GeoJSON string"));
            };
            Shape::Collection(items.iter().map(shape_from_json).collect::<Result<_>>()?)
        }
        _ => return Err(internal("invalid GeoJson representation")),
    })
}

/// `ST_GeomFromGeoJSON(text)`: SRID 4326 unless a `crs` names `EPSG:<n>`
/// (any other crs name -- `urn:ogc:def:crs:OGC:1.3:CRS84`, an unknown EPSG
/// code -- resolves to no SRID, as PostGIS's spatial_ref_sys lookup does).
pub fn from_geojson(text: &str) -> Result<Geometry> {
    let parsed = json::parse(text).map_err(|_| {
        let offset = text.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        Error::Internal(format!("unexpected character (at offset {offset})"))
    })?;
    let shape = shape_from_json(&parsed)?;
    let srid = match json::member(&parsed, "crs") {
        None => 4326,
        Some(crs) => {
            let name = json::member(crs, "properties")
                .and_then(|p| json::member(p, "name"))
                .and_then(|n| match n {
                    Json::Str(s) => Some(s.as_str()),
                    _ => None,
                });
            match name {
                Some(n) => n
                    .strip_prefix("EPSG:")
                    .or_else(|| n.strip_prefix("urn:ogc:def:crs:EPSG::"))
                    .and_then(|code| code.parse::<i32>().ok())
                    .filter(|code| KNOWN_EPSG.contains(code))
                    .unwrap_or(0),
                None => 0,
            }
        }
    };
    let z = has_z(&shape);
    Ok(Geometry {
        srid,
        z,
        m: false,
        shape,
    })
}

/// The EPSG codes this server accepts as a GeoJSON `crs`. PostGIS looks the
/// name up in `spatial_ref_sys`, which holds several thousand rows; the
/// handful a client is likely to name are enough to answer the same way.
const KNOWN_EPSG: &[i32] = &[
    3857, 4326, 4269, 4267, 3395, 900913, 2154, 27700, 25832, 25833, 32632, 32633, 3035, 3005,
    2056, 28992, 31370, 3003, 3004, 2100, 3006, 4258, 4283, 3577, 7844, 4230,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        canonical(s).unwrap()
    }

    #[test]
    fn measured_on_postgis_3_4() {
        assert_eq!(
            c("POINT(1 2)"),
            "0101000000000000000000F03F0000000000000040"
        );
        assert_eq!(
            c("0101000000000000000000f03f0000000000000040"),
            "0101000000000000000000F03F0000000000000040"
        );
        assert_eq!(
            c("SRID=4326;POINT(1 2)"),
            "0101000020E6100000000000000000F03F0000000000000040"
        );
        assert_eq!(
            c("POINT Z (1 2 3)"),
            "0101000080000000000000F03F00000000000000400000000000000840"
        );
        assert_eq!(
            c("POINT(1 2 3)"),
            "0101000080000000000000F03F00000000000000400000000000000840"
        );
        assert_eq!(
            c("POINT EMPTY"),
            "0101000000000000000000F87F000000000000F87F"
        );
        assert_eq!(
            c("LINESTRING(0 0, 1 1)"),
            "01020000000200000000000000000000000000000000000000000000000000F03F000000000000F03F"
        );
    }

    #[test]
    fn errors() {
        let e = canonical("zz").unwrap_err();
        assert_eq!(e.sqlstate(), "XX000");
        assert_eq!(
            e.to_string(),
            "parse error - invalid geometry\nHint: \"zz\" <-- parse error at position 2 within geometry"
        );
        assert_eq!(
            canonical("0101000000000000000000F03F")
                .unwrap_err()
                .to_string(),
            "WKB structure does not match expected size!"
        );
        // One ordinate, or five: a parse error, not a point (3.4.6).
        assert!(canonical("POINT(1)").is_err());
        assert!(canonical("POINT(1 2 3 4 5)").is_err());
        assert!(canonical("POINT(1 2 3 4)").is_ok());
    }

    #[test]
    fn geojson_measured() {
        let g = from_geojson(r#"{"type":"Point","coordinates":[1.2, 3.4]}"#).unwrap();
        assert_eq!(
            to_hex(&g),
            "0101000020E6100000333333333333F33F3333333333330B40"
        );
        let g = from_geojson(
            r#"{"type":"Point","coordinates":[1,2],"crs":{"type":"name","properties":{"name":"EPSG:3857"}}}"#,
        )
        .unwrap();
        assert_eq!(g.srid, 3857);
        let g = from_geojson(
            r#"{"type":"Point","coordinates":[1,2],"crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:OGC:1.3:CRS84"}}}"#,
        )
        .unwrap();
        assert_eq!(g.srid, 0);
        let g = from_geojson(r#"{"type":"Point","coordinates":[1,2,3,4]}"#).unwrap();
        assert!(to_hex(&g).starts_with("01010000A0"));
        assert_eq!(
            from_geojson("zz").unwrap_err().to_string(),
            "unexpected character (at offset 0)"
        );
        assert_eq!(
            from_geojson(r#"{"type":"Point"}"#).unwrap_err().to_string(),
            "Unable to find 'coordinates' in GeoJSON string"
        );
        assert_eq!(
            from_geojson(r#"{"type":"Blob","coordinates":[1,2]}"#)
                .unwrap_err()
                .to_string(),
            "invalid GeoJson representation"
        );
        assert_eq!(
            from_geojson(r#"{"coordinates":[1,2]}"#)
                .unwrap_err()
                .to_string(),
            "unknown GeoJSON type"
        );
        assert_eq!(
            from_geojson("[1,2]").unwrap_err().to_string(),
            "unknown GeoJSON type"
        );
    }

    #[test]
    fn wkt_out_measured_on_postgis_3_4() {
        let t = |s: &str| to_wkt(&from_text(s).unwrap(), false);
        assert_eq!(t("POINT(1.2 3.4)"), "POINT(1.2 3.4)");
        assert_eq!(t("POINT(0.30000000000000004 1e-7)"), "POINT(0.3 0.0000001)");
        assert_eq!(t("POINT Z (1 2 3)"), "POINT Z (1 2 3)");
        assert_eq!(
            t("MULTIPOLYGON(((0 0,1 0,1 1,0 0)))"),
            "MULTIPOLYGON(((0 0,1 0,1 1,0 0)))"
        );
        assert_eq!(t("POINT EMPTY"), "POINT EMPTY");
        assert_eq!(t("MULTIPOINT(1 2,3 4)"), "MULTIPOINT((1 2),(3 4))");
        assert_eq!(
            t("GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(0 0,1 1))"),
            "GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(0 0,1 1))"
        );
        assert_eq!(
            t("POINT(123456789012345678 0.000000000000001234)"),
            "POINT(1.234567890123457e+17 1.234e-15)"
        );
        assert_eq!(t("POINT(1e15 1e16)"), "POINT(1e+15 1e+16)");
        assert_eq!(t("POINT(-0 1.5e-10)"), "POINT(0 1.5e-10)");
        assert_eq!(
            t("POINT(1234567.891 12345678901234567)"),
            "POINT(1234567.891 1.234567890123457e+16)"
        );
        assert_eq!(t("POINT(1e-9 1e-8)"), "POINT(1e-9 1e-8)");
        assert_eq!(
            to_wkt(
                &from_text("SRID=4326;POINT(0.1 100000000000000000)").unwrap(),
                true
            ),
            "SRID=4326;POINT(0.1 1e+17)"
        );
    }

    #[test]
    fn ewkb_round_trip_multipolygon() {
        let text = "MULTIPOLYGON(((0 0,1 0,1 1,0 0)),((2 2,3 2,3 3,2 2),(2.2 2.2,2.5 2.2,2.5 2.5,2.2 2.2)))";
        let g = from_wkt(text).unwrap();
        let hex = to_hex(&g);
        let back = from_ewkb(&unhex(&hex).unwrap()).unwrap();
        assert_eq!(back, g);
    }
}
