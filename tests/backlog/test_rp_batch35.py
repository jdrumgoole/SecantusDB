"""Batch 35 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-162 (every parameter answers SHOW, from the server's
own files and port) and RP-170 (the parameters only a superuser may read),
found while fixing it.

The scenarios run as `test_rp_batch34.py`'s do, with two more steps:
`@include name text` writes `text` to `name` in the server's data
directory and has the main configuration file include it (creating the
file where the server has none), and `@restore` puts the main
configuration file back and removes what `@include` wrote. They read the
data directory from `SHOW data_directory`, so the test runs on the
server's machine. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch35.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made and leaves the files as they were.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario as run_steps


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    """`test_rp_batch34.run_scenario`, with `@include` and `@restore`
    done between its runs."""
    with psycopg.connect(dsn, autocommit=True) as conn:
        data = Path(conn.execute("show data_directory").fetchone()[0])
    main = data / "postgresql.conf"
    original = main.read_text() if main.exists() else None
    written: list[Path] = []
    out: list[Any] = []
    batch: list[str] = []

    def flush() -> None:
        if batch:
            out.extend(run_steps(dsn, batch))
            batch.clear()

    try:
        for s in statements:
            if s.startswith("@include "):
                flush()
                _, name, text = s.split(" ", 2)
                (data / name).write_text(text)
                written.append(data / name)
                lines = main.read_text() if main.exists() else ""
                main.write_text(lines + f"include '{name}'\n")
            elif s == "@restore":
                flush()
                if original is None:
                    main.unlink(missing_ok=True)
                else:
                    main.write_text(original)
                for f in written:
                    f.unlink(missing_ok=True)
            else:
                batch.append(s)
        flush()
    finally:
        if original is None:
            main.unlink(missing_ok=True)
        else:
            main.write_text(original)
        for f in written:
            f.unlink(missing_ok=True)
    return out


_UNDUMPED = (
    "'config_file', 'data_directory', 'hba_file', 'ident_file', 'port', "
    "'unix_socket_directories', 'log_timezone'"
)

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------- RP-162 the server's own settings
    "the server's own files and port": (
        "RP-162",
        False,
        [
            "select name, category, short_desc, context, vartype, min_val, max_val, "
            f"boot_val from pg_settings where name in ({_UNDUMPED}) order by name",
            "select name, source from pg_settings where name in ('config_file', "
            "'data_directory', 'hba_file', 'ident_file', 'port', 'unix_socket_directories') "
            "order by name",
            "select current_setting('data_directory') like '/%', "
            "current_setting('config_file') = current_setting('data_directory') "
            "|| '/postgresql.conf', "
            "current_setting('hba_file') = current_setting('data_directory') || '/pg_hba.conf', "
            "current_setting('ident_file') = current_setting('data_directory') "
            "|| '/pg_ident.conf', "
            "current_setting('port')::int = inet_server_port()",
            "show is_superuser",
            "show default_with_oids",
            "show ssl_renegotiation_limit",
            "set datestyle = 'German'",
            "reset all",
            "select current_setting('data_directory') like '/%', current_setting('port') <> ''",
            "create role rp35_q login",
            "@open q user=rp35_q",
            "@q show is_superuser",
            "@q select current_setting('port')::int = inet_server_port()",
            "drop role rp35_q",
        ],
    ),
    # ------------------------- RP-170 what only a superuser may read
    "the parameters only a superuser may read": (
        "RP-170",
        False,
        [
            "create role rp35_r login",
            "@open r user=rp35_r",
            *(
                f"@r show {n}"
                for n in (
                    "config_file",
                    "data_directory",
                    "hba_file",
                    "ssl_ciphers",
                    "shared_preload_libraries",
                    "unix_socket_directories",
                )
            ),
            "@r select current_setting('log_directory', true)",
            "@r select count(*) from pg_settings where name in ('config_file', 'data_directory', "
            "'dynamic_library_path', 'external_pid_file', 'hba_file', 'ident_file', "
            "'jit_provider', 'krb_server_keyfile', 'log_directory', 'log_filename', "
            "'output_plugin_libraries', 'primary_conninfo', 'session_preload_libraries', "
            "'shared_preload_libraries', 'ssl_ciphers', 'ssl_dh_params_file', 'ssl_ecdh_curve', "
            "'ssl_max_protocol_version', 'ssl_min_protocol_version', 'ssl_passphrase_command', "
            "'unix_socket_directories')",
            "@r select count(*) from pg_settings where name in ('port', 'work_mem')",
            "grant pg_read_all_settings to rp35_r",
            "@open r2 user=rp35_r",
            "@r2 select count(*) > 0 from pg_settings where name = 'hba_file'",
            "@r2 select current_setting('hba_file') like '%pg_hba.conf'",
            "drop role rp35_r",
        ],
    ),
    # ------------------------------- RP-164 GRANT ... ON PARAMETER, chains
    "parameter grants, grant options and their revocation": (
        "RP-164",
        False,
        [
            "create role rp35_p login",
            "create role rp35_n login",
            "@open p user=rp35_p",
            "@open n user=rp35_n",
            "@p grant set on parameter work_mem to rp35_n",
            "grant alter system on parameter work_mem to rp35_p with grant option",
            "@p grant alter system on parameter work_mem to rp35_n",
            "@p ~grant set on parameter work_mem to rp35_n",
            "@p grant all on parameter work_mem to rp35_n",
            "select parname, paracl::text from pg_parameter_acl order by parname",
            "!revoke alter system on parameter work_mem from rp35_p",
            "revoke grant option for alter system on parameter work_mem from rp35_p cascade",
            "select parname, paracl::text from pg_parameter_acl order by parname",
            "grant usage on parameter work_mem to rp35_p",
            "grant set on parameter rp35_nosuch to rp35_p",
            "grant set on parameter rp35x.custom, Work_Mem to rp35_n",
            "select parname, paracl::text from pg_parameter_acl order by parname",
            "select has_parameter_privilege('rp35_p', 'work_mem', 'ALTER SYSTEM'), "
            "has_parameter_privilege('rp35_p', 'work_mem', 'SET'), "
            "has_parameter_privilege('rp35_n', 'work_mem', 'set'), "
            "has_parameter_privilege('work_mem', 'SET'), "
            "has_parameter_privilege('rp35_p', 'rp35_nosuch', 'SET')",
            "select has_parameter_privilege('rp35_p', 'work_mem', 'USAGE')",
            "@p alter system set work_mem = '2MB'",
            "@p alter system reset work_mem",
            "@p alter system set temp_buffers = '2MB'",
            "!drop role rp35_n",
            "drop owned by rp35_n",
            "select count(*) from pg_parameter_acl where paracl::text like '%rp35_n%'",
            "revoke all on parameter work_mem, rp35x.custom from rp35_p, rp35_n cascade",
            "select count(*) from pg_parameter_acl",
            "drop role rp35_n",
            "drop role rp35_p",
        ],
    ),
    # --------------- RP-171 a superuser parameter needs the SET privilege
    "a superuser parameter needs the SET privilege": (
        "RP-171",
        False,
        [
            "create role rp35_s login",
            "create role rp35_t",
            "@open s user=rp35_s",
            "@s set log_min_duration_statement = 5",
            "@s set lc_messages = 'C'",
            "@s set session_replication_role = replica",
            "@s reset log_min_duration_statement",
            "@s select set_config('log_min_duration_statement', '5', false)",
            "@s ~set local log_min_duration_statement = 5",
            "@s set log_min_duration_statement to default",
            "@s set work_mem = '5MB'",
            "grant set on parameter log_min_duration_statement to rp35_s",
            "@s set log_min_duration_statement = 7",
            "@s show log_min_duration_statement",
            "@s reset log_min_duration_statement",
            "set role rp35_t",
            "set log_min_duration_statement = 7",
            "reset role",
            "set role rp35_s",
            "set log_min_duration_statement = 7",
            "reset role",
            "reset log_min_duration_statement",
            "revoke set on parameter log_min_duration_statement from rp35_s",
            "drop role rp35_t",
            "drop role rp35_s",
        ],
    ),
    # --------------------------------- RP-162 the main configuration file
    "the main configuration file and what it includes": (
        "RP-162",
        False,
        [
            "@include rp35_a.conf work_mem = '3MB'\nrp35_bad line here\n"
            "include_if_exists 'rp35_none.conf'\ntemp_buffers = 2000\n",
            "select regexp_replace(sourcefile, '.*/', ''), sourceline, name, setting, applied, "
            "error from pg_file_settings where sourcefile like '%rp35%' order by seqno",
            "select pg_reload_conf()",
            "@sleep",
            "@open b",
            "@b show work_mem",
            "@b select regexp_replace(sourcefile, '.*/', ''), sourceline, source from pg_settings "
            "where name = 'work_mem'",
            "@b show temp_buffers",
            "@restore",
            "select pg_reload_conf()",
            "@sleep",
            "@open c",
            "@c show work_mem",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a superuser parameter needs the SET privilege": [
        "CREATE ROLE",
        "CREATE ROLE",
        ("ERR", "42501", 'permission denied to set parameter "log_min_duration_statement"', None),
        ("ERR", "42501", 'permission denied to set parameter "lc_messages"', None),
        ("ERR", "42501", 'permission denied to set parameter "session_replication_role"', None),
        ("ERR", "42501", 'permission denied to set parameter "log_min_duration_statement"', None),
        ("ERR", "42501", 'permission denied to set parameter "log_min_duration_statement"', None),
        (
            (
                "ERR",
                "42501",
                'permission denied to set parameter "log_min_duration_statement"',
                None,
            ),
            ["SET LOCAL can only be used in transaction blocks"],
        ),
        ("ERR", "42501", 'permission denied to set parameter "log_min_duration_statement"', None),
        "SET",
        "GRANT",
        "SET",
        [("7ms",)],
        "RESET",
        "SET",
        ("ERR", "42501", 'permission denied to set parameter "log_min_duration_statement"', None),
        "RESET",
        "SET",
        "SET",
        "RESET",
        "RESET",
        "REVOKE",
        "DROP ROLE",
        "DROP ROLE",
    ],
    "parameter grants, grant options and their revocation": [
        "CREATE ROLE",
        "CREATE ROLE",
        ("ERR", "42501", "permission denied for parameter work_mem", None),
        "GRANT",
        "GRANT",
        ("GRANT", ['no privileges were granted for "work_mem"']),
        "GRANT",
        [("work_mem", "{postgres=sA/postgres,rp35_p=A*/postgres,rp35_n=A/rp35_p}")],
        ("ERR", "2BP01", "dependent privileges exist", "Use CASCADE to revoke them too.", None),
        "REVOKE",
        [("work_mem", "{postgres=sA/postgres,rp35_p=A/postgres}")],
        ("ERR", "0LP01", "invalid privilege type USAGE for parameter", None),
        ("ERR", "42602", 'invalid parameter name "rp35_nosuch"', None),
        "GRANT",
        [
            ("rp35x.custom", "{postgres=sA/postgres,rp35_n=s/postgres}"),
            ("work_mem", "{postgres=sA/postgres,rp35_p=A/postgres,rp35_n=s/postgres}"),
        ],
        [("True", "False", "True", "True", "False")],
        ("ERR", "22023", 'unrecognized privilege type: "USAGE"', None),
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        ("ERR", "42501", 'permission denied to set parameter "temp_buffers"', None),
        (
            "ERR",
            "2BP01",
            'role "rp35_n" cannot be dropped because some objects depend on it',
            None,
            "privileges for parameter work_mem\nprivileges for parameter rp35x.custom",
        ),
        "DROP OWNED",
        [("0",)],
        "REVOKE",
        [("0",)],
        "DROP ROLE",
        "DROP ROLE",
    ],
    "the main configuration file and what it includes": [
        [
            ("rp35_a.conf", "1", "work_mem", "3MB", "False", "None"),
            ("rp35_a.conf", "2", "None", "None", "False", "syntax error"),
        ],
        [("True",)],
        [("4MB",)],
        [("None", "None", "default")],
        [("8MB",)],
        [("True",)],
        [("4MB",)],
    ],
    "the parameters only a superuser may read": [
        "CREATE ROLE",
        (
            "ERR",
            "42501",
            'must be superuser or have privileges of pg_read_all_settings to examine "config_file"',
            None,
        ),
        (
            "ERR",
            "42501",
            "must be superuser or have privileges of "
            'pg_read_all_settings to examine "data_directory"',
            None,
        ),
        (
            "ERR",
            "42501",
            'must be superuser or have privileges of pg_read_all_settings to examine "hba_file"',
            None,
        ),
        (
            "ERR",
            "42501",
            'must be superuser or have privileges of pg_read_all_settings to examine "ssl_ciphers"',
            None,
        ),
        (
            "ERR",
            "42501",
            "must be superuser or have privileges of "
            "pg_read_all_settings to examine "
            '"shared_preload_libraries"',
            None,
        ),
        (
            "ERR",
            "42501",
            "must be superuser or have privileges of "
            "pg_read_all_settings to examine "
            '"unix_socket_directories"',
            None,
        ),
        (
            "ERR",
            "42501",
            "must be superuser or have privileges of "
            'pg_read_all_settings to examine "log_directory"',
            None,
        ),
        [("0",)],
        [("2",)],
        "GRANT ROLE",
        [("True",)],
        [("True",)],
        "DROP ROLE",
    ],
    "the server's own files and port": [
        [
            (
                "config_file",
                "File Locations",
                "Sets the server's main configuration file.",
                "postmaster",
                "string",
                "None",
                "None",
                "None",
            ),
            (
                "data_directory",
                "File Locations",
                "Sets the server's data directory.",
                "postmaster",
                "string",
                "None",
                "None",
                "None",
            ),
            (
                "hba_file",
                "File Locations",
                'Sets the server\'s "hba" configuration file.',
                "postmaster",
                "string",
                "None",
                "None",
                "None",
            ),
            (
                "ident_file",
                "File Locations",
                'Sets the server\'s "ident" configuration file.',
                "postmaster",
                "string",
                "None",
                "None",
                "None",
            ),
            (
                "log_timezone",
                "Reporting and Logging / What to Log",
                "Sets the time zone to use in log messages.",
                "sighup",
                "string",
                "None",
                "None",
                "GMT",
            ),
            (
                "port",
                "Connections and Authentication / Connection Settings",
                "Sets the TCP port the server listens on.",
                "postmaster",
                "integer",
                "1",
                "65535",
                "5432",
            ),
            (
                "unix_socket_directories",
                "Connections and Authentication / Connection Settings",
                "Sets the directories where Unix-domain sockets will be created.",
                "postmaster",
                "string",
                "None",
                "None",
                "/tmp",
            ),
        ],
        [
            ("config_file", "override"),
            ("data_directory", "override"),
            ("hba_file", "override"),
            ("ident_file", "override"),
            ("port", "command line"),
            ("unix_socket_directories", "command line"),
        ],
        [("True", "True", "True", "True", "True")],
        [("on",)],
        [("off",)],
        [("0",)],
        "SET",
        "RESET",
        [("True", "True")],
        "CREATE ROLE",
        [("off",)],
        [("True",)],
        "DROP ROLE",
    ],
}


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


@pytest.mark.backlog_fixed("RP-162")
def test_a_store_s_pg_hba_conf_decides_logins(tmp_path: Path) -> None:
    """Without `--hba-file`, the data directory's `pg_hba.conf` decides
    every login and is the one `hba_file` names, as PostgreSQL 15.19 reads
    its default file (Joe, 2026-10-23). PostgreSQL refuses a `reject`
    line's user with 28000 `pg_hba.conf rejects connection for host
    "127.0.0.1", user "rp35_no", database "postgres", no encryption`."""
    from tests.test_rust_pgserver_slice import _Server

    store = tmp_path / "store"
    store.mkdir()
    (store / "pg_hba.conf").write_text(
        "host all rp35_no 127.0.0.1/32 reject\nhost all all 127.0.0.1/32 trust\n"
    )
    with _Server(store, args=("--refuse-unknown-users",)) as server:
        with psycopg.connect(
            f"host=127.0.0.1 port={server.port} user=postgres dbname=postgres", autocommit=True
        ) as conn:
            hba = conn.execute("show hba_file").fetchone()[0]
            data = conn.execute("show data_directory").fetchone()[0]
            conn.execute("create role rp35_no login")
        try:
            psycopg.connect(f"host=127.0.0.1 port={server.port} user=rp35_no dbname=postgres")
            refused = None
        except psycopg.OperationalError as e:
            refused = str(e).splitlines()[-1].split("FATAL:  ", 1)[-1]
    assert hba == os.path.join(data, "pg_hba.conf")
    assert refused == (
        'pg_hba.conf rejects connection for host "127.0.0.1", user "rp35_no", '
        'database "postgres", no encryption'
    )


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
