//! The configuration-file views (RP-101, as decided by Joe on 2026-10-22):
//! `pg_hba_file_rules` and `pg_ident_file_mappings` read the files the
//! server was started with again, as PostgreSQL's do, and show each line's
//! fields as the tokenizer the logins use splits them (`hba::file_lines`); `pg_file_settings` has no
//! rows, as no `postgresql.conf` is read; `pg_config` reports this server's
//! own build.

use super::*;

/// A prefix length as the netmask PostgreSQL prints beside the address.
fn netmask(address: &std::net::IpAddr, bits: u32) -> Option<String> {
    Some(match address {
        std::net::IpAddr::V4(_) => {
            let mask = u32::MAX.checked_shl(32 - bits.min(32)).unwrap_or(0);
            std::net::Ipv4Addr::from(if bits == 0 { 0 } else { mask }).to_string()
        }
        std::net::IpAddr::V6(_) => {
            let mask = u128::MAX.checked_shl(128 - bits.min(128)).unwrap_or(0);
            std::net::Ipv6Addr::from(if bits == 0 { 0 } else { mask }).to_string()
        }
    })
}

fn text_array(items: &[String]) -> Bson {
    Bson::Array(items.iter().map(|s| Bson::String(s.clone())).collect())
}

impl PgHandler {
    /// `pg_hba_file_rules`: one row per rule line of the hba file.
    pub(crate) fn hba_file_rule_rows(&self, def: &TableDef) -> Vec<Document> {
        let Some(path) = self.hba.as_ref().map(|h| h.files.hba_file.clone()) else {
            return Vec::new();
        };
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out = Vec::new();
        for (number, fs) in hba::file_lines(&path) {
            let one = |k: usize| fs.get(k).and_then(|f| f.first()).cloned();
            let kind = one(0).unwrap_or_default();
            let mut d = Document::new();
            d.insert(f("line_number"), Bson::Int32(number as i32));
            d.insert(f("type"), kind.as_str());
            d.insert(f("database"), text_array(fs.get(1).map_or(&[][..], |v| v)));
            d.insert(f("user_name"), text_array(fs.get(2).map_or(&[][..], |v| v)));
            // `local` has no address; an address with a prefix length is
            // shown as the address and its netmask; a bare address is
            // followed by its mask; a keyword or host name stands alone.
            let (address, mask, method_at) = if kind == "local" {
                (None, None, 3)
            } else {
                let written = one(3).unwrap_or_default();
                match written.split_once('/') {
                    Some((ip, bits)) => match (ip.parse::<std::net::IpAddr>(), bits.parse::<u32>())
                    {
                        (Ok(a), Ok(b)) => (Some(a.to_string()), netmask(&a, b), 4),
                        _ => (Some(written.clone()), None, 4),
                    },
                    None => match written.parse::<std::net::IpAddr>() {
                        Ok(a) => (Some(a.to_string()), one(4), 5),
                        Err(_) => (Some(written.clone()), None, 4),
                    },
                }
            };
            d.insert(f("address"), address.map_or(Bson::Null, Bson::String));
            d.insert(f("netmask"), mask.map_or(Bson::Null, Bson::String));
            d.insert(
                f("auth_method"),
                one(method_at).map_or(Bson::Null, Bson::String),
            );
            let options: Vec<String> = fs.iter().skip(method_at + 1).flatten().cloned().collect();
            d.insert(
                f("options"),
                if options.is_empty() {
                    Bson::Null
                } else {
                    text_array(&options)
                },
            );
            d.insert(f("error"), Bson::Null);
            out.push(d);
        }
        out
    }

    /// `pg_ident_file_mappings`: one row per mapping line of the ident file.
    pub(crate) fn ident_file_mapping_rows(&self, def: &TableDef) -> Vec<Document> {
        let Some(path) = self.hba.as_ref().and_then(|h| h.files.ident_file.clone()) else {
            return Vec::new();
        };
        let f = |n: &str| def.field_of(n).expect("column");
        let mut out = Vec::new();
        for (number, fs) in hba::file_lines(&path) {
            let one = |k: usize| {
                fs.get(k)
                    .and_then(|f| f.first())
                    .cloned()
                    .map_or(Bson::Null, Bson::String)
            };
            let mut d = Document::new();
            d.insert(f("line_number"), Bson::Int32(number as i32));
            d.insert(f("map_name"), one(0));
            d.insert(f("sys_name"), one(1));
            d.insert(f("pg_username"), one(2));
            d.insert(f("error"), Bson::Null);
            out.push(d);
        }
        out
    }

    /// `pg_config`: PostgreSQL's names, answered by this server's own build
    /// -- its version, where it runs from, how Rust built it -- and empty
    /// where no PostgreSQL installation stands behind the name.
    pub(crate) fn pg_config_rows(&self, def: &TableDef) -> Vec<Document> {
        let f = |n: &str| def.field_of(n).expect("column");
        let bindir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default();
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        // As `server_version` reports it (the release this server's answers
        // are measured against).
        let version = "PostgreSQL 15.19".to_string();
        let rows: [(&str, String); 23] = [
            ("BINDIR", bindir),
            ("DOCDIR", String::new()),
            ("HTMLDIR", String::new()),
            ("INCLUDEDIR", String::new()),
            ("PKGINCLUDEDIR", String::new()),
            ("INCLUDEDIR-SERVER", String::new()),
            ("LIBDIR", String::new()),
            ("PKGLIBDIR", String::new()),
            ("LOCALEDIR", String::new()),
            ("MANDIR", String::new()),
            ("SHAREDIR", String::new()),
            ("SYSCONFDIR", String::new()),
            ("PGXS", String::new()),
            (
                "CONFIGURE",
                format!(
                    "cargo build --{profile} (secantus-pg {})",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            ("CC", "rustc".to_string()),
            ("CPPFLAGS", String::new()),
            ("CFLAGS", format!("{profile} profile")),
            ("CFLAGS_SL", String::new()),
            ("LDFLAGS", String::new()),
            ("LDFLAGS_EX", String::new()),
            ("LDFLAGS_SL", String::new()),
            ("LIBS", String::new()),
            ("VERSION", version),
        ];
        rows.into_iter()
            .map(|(name, setting)| {
                let mut d = Document::new();
                d.insert(f("name"), name);
                d.insert(f("setting"), setting);
                d
            })
            .collect()
    }
}
