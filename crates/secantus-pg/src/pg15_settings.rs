//! PostgreSQL 15's configuration parameters, as `pg_settings` lists them.
//!
//! `pg15_settings.tsv` is a dump of a stock PostgreSQL 15 cluster's
//! `pg_settings` (machine-specific paths, the port and the zones left out):
//! `name, SHOW value, setting, unit, category, short_desc, context, vartype,
//! min_val, max_val, boot_val`, and for an enum its `enumvals` joined by
//! commas (dumped from PostgreSQL 15.19; `ssl_max_protocol_version`'s first
//! value is the empty string). Every parameter there is SHOW-able and
//! RESET-able here with PostgreSQL's default, so a client that reads one this
//! server does not obey (`work_mem`, `maintenance_work_mem`, ...) gets
//! PostgreSQL's answer rather than 42704.

/// One `pg_settings` row of PostgreSQL 15.
pub(crate) struct Pg15Setting {
    pub name: &'static str,
    /// What `SHOW` answers for the default (with its unit, `4MB`).
    pub show: &'static str,
    /// `pg_settings.setting` for the default (in `unit`s, `4096`).
    pub setting: &'static str,
    pub unit: &'static str,
    pub category: &'static str,
    pub short_desc: &'static str,
    pub context: &'static str,
    pub vartype: &'static str,
    pub min_val: &'static str,
    pub max_val: &'static str,
    pub boot_val: &'static str,
    /// `enumvals` joined by commas; empty for a type that is not an enum.
    pub enumvals: &'static str,
}

const TSV: &str = include_str!("pg15_settings.tsv");

/// Every row of the dump, parsed once.
pub(crate) fn pg15_settings() -> &'static [Pg15Setting] {
    static ROWS: std::sync::OnceLock<Vec<Pg15Setting>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        TSV.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let f: Vec<&'static str> = l.split('\t').collect();
                let g = |i: usize| f.get(i).copied().unwrap_or("");
                Pg15Setting {
                    name: g(0),
                    show: g(1),
                    setting: g(2),
                    unit: g(3),
                    category: g(4),
                    short_desc: g(5),
                    context: g(6),
                    vartype: g(7),
                    min_val: g(8),
                    max_val: g(9),
                    boot_val: g(10),
                    enumvals: g(11),
                }
            })
            .collect()
    })
}

/// The rows the dump leaves out because their values are the machine's
/// (the paths, the port and the log zone), as PostgreSQL 15.19 describes
/// them; their values are the server's own (measured on a fresh cluster).
const UNDUMPED: [Pg15Setting; 7] = [
    file_location("config_file", "Sets the server's main configuration file."),
    file_location("data_directory", "Sets the server's data directory."),
    file_location("hba_file", "Sets the server's \"hba\" configuration file."),
    file_location(
        "ident_file",
        "Sets the server's \"ident\" configuration file.",
    ),
    Pg15Setting {
        name: "log_timezone",
        show: "GMT",
        setting: "GMT",
        unit: "",
        category: "Reporting and Logging / What to Log",
        short_desc: "Sets the time zone to use in log messages.",
        context: "sighup",
        vartype: "string",
        min_val: "",
        max_val: "",
        boot_val: "GMT",
        enumvals: "",
    },
    Pg15Setting {
        name: "port",
        show: "5432",
        setting: "5432",
        unit: "",
        category: "Connections and Authentication / Connection Settings",
        short_desc: "Sets the TCP port the server listens on.",
        context: "postmaster",
        vartype: "integer",
        min_val: "1",
        max_val: "65535",
        boot_val: "5432",
        enumvals: "",
    },
    Pg15Setting {
        name: "unix_socket_directories",
        show: "/tmp",
        setting: "/tmp",
        unit: "",
        category: "Connections and Authentication / Connection Settings",
        short_desc: "Sets the directories where Unix-domain sockets will be created.",
        context: "postmaster",
        vartype: "string",
        min_val: "",
        max_val: "",
        boot_val: "/tmp",
        enumvals: "",
    },
];

/// A `File Locations` row: a postmaster string with no boot value.
const fn file_location(name: &'static str, short_desc: &'static str) -> Pg15Setting {
    Pg15Setting {
        name,
        show: "",
        setting: "",
        unit: "",
        category: "File Locations",
        short_desc,
        context: "postmaster",
        vartype: "string",
        min_val: "",
        max_val: "",
        boot_val: "",
        enumvals: "",
    }
}

/// The PostgreSQL 15 row for `name` (settings are keyed as `canonical_setting`
/// spells them; the dump's names compare case-insensitively).
pub(crate) fn pg15_setting(name: &str) -> Option<&'static Pg15Setting> {
    pg15_settings()
        .iter()
        .chain(UNDUMPED.iter())
        .find(|s| s.name.eq_ignore_ascii_case(name))
}

/// The parameters only a superuser or a member of `pg_read_all_settings`
/// may read (`GUC_SUPERUSER_ONLY`; measured on 15.19).
pub(crate) const SUPERUSER_ONLY: [&str; 21] = [
    "config_file",
    "data_directory",
    "dynamic_library_path",
    "external_pid_file",
    "hba_file",
    "ident_file",
    "jit_provider",
    "krb_server_keyfile",
    "log_directory",
    "log_filename",
    "output_plugin_libraries",
    "primary_conninfo",
    "session_preload_libraries",
    "shared_preload_libraries",
    "ssl_ciphers",
    "ssl_dh_params_file",
    "ssl_ecdh_curve",
    "ssl_max_protocol_version",
    "ssl_min_protocol_version",
    "ssl_passphrase_command",
    "unix_socket_directories",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dump_parses_and_carries_work_mem() {
        assert!(pg15_settings().len() > 300);
        assert!(pg15_settings().iter().all(|s| !s.name.is_empty()));
        let w = pg15_setting("work_mem").expect("work_mem");
        assert_eq!((w.show, w.setting, w.unit), ("4MB", "4096", "kB"));
        let enums: Vec<_> = pg15_settings()
            .iter()
            .filter(|s| s.vartype == "enum")
            .collect();
        assert_eq!(enums.len(), 36);
        assert!(enums.iter().all(|s| !s.enumvals.is_empty()));
        assert_eq!(
            pg15_setting("backslash_quote").unwrap().enumvals,
            "safe_encoding,on,off"
        );
    }
}
