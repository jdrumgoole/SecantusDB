//! `pg_hba.conf`: which connections may log in, and how each must prove who
//! it is. Read as PostgreSQL 15 reads it:
//!
//! ```text
//! # TYPE     DATABASE  USER       ADDRESS        METHOD  [OPTIONS]
//! hostssl    all       +staff     10.0.0.0/8     scram-sha-256
//! host       appdb     app        127.0.0.1/32   trust
//! host       all       all        all            reject
//! ```
//!
//! The first line whose type, database, user and address all match the
//! connection decides it; later lines are not consulted, whatever the
//! outcome. A connection no line matches is refused.
//!
//! What is read: the `host`, `hostssl` and `hostnossl` types; the `trust`,
//! `reject`, `scram-sha-256`, `md5`, `password` and `cert` methods; `all`,
//! `sameuser`, `samerole` and names for the database; `all`, names and
//! `+role` for the user; `all`, `samehost` and an address with a prefix
//! length or a mask; and the `clientcert`, `clientname` and `map` options.
//!
//! What is refused when the file is read, by name, so that a rule this
//! server cannot apply is never silently skipped: a method that needs a
//! service outside the server (`peer`, `ident`, `gss`, `sspi`, `ldap`,
//! `radius`, `pam`, `bsd`). A host name for the address is matched as
//! PostgreSQL's `check_hostname` matches it. `@file` inclusion is read as PostgreSQL 15
//! reads it. `local`
//! lines are for Unix-domain sockets, which this server does not listen on:
//! they are read and never match, as are the GSSAPI types.

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::ident::UserMap;
use crate::tls::ClientName;

/// The transport a line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// `host`, and `hostnogssenc`: TCP, TLS or not.
    Any,
    /// `hostssl`.
    Tls,
    /// `hostnossl`.
    Plain,
    /// `local`, `hostgssenc`: nothing this server accepts.
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Database {
    All,
    /// The database named for the user.
    SameUser,
    /// A database named for a role the user is a member of.
    SameRole,
    /// A replication connection, which this server never has.
    Replication,
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum User {
    All,
    Name(String),
    /// `+role`: the role, or any member of it.
    MemberOf(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Address {
    All,
    /// Any of the server's own addresses.
    SameHost,
    /// `samenet`: the network of one of this host's interfaces.
    SameNet,
    Network(IpAddr, u8),
    /// A host name, or a `.suffix` of one (`check_hostname`).
    Host(String),
}

/// How a connection a line matches must prove who it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Trust,
    Reject,
    /// SCRAM-SHA-256, and nothing else.
    Scram,
    /// PostgreSQL's `md5`: an md5 challenge for a role whose stored
    /// password is an md5 hash, SCRAM for one whose is a SCRAM verifier.
    Md5,
    /// The password in the clear, which only TLS then protects.
    Password,
    /// A client certificate that names the user, and no password.
    Cert,
}

/// What a line asks of a client certificate beside its method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CertCheck {
    None,
    /// `clientcert=verify-ca`: one signed by the CA.
    VerifyCa,
    /// `clientcert=verify-full`, and the `cert` method: one that names the
    /// user.
    VerifyFull,
}

/// One line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    transport: Transport,
    databases: Vec<Database>,
    users: Vec<User>,
    address: Address,
    pub method: Method,
    pub cert_check: CertCheck,
    pub client_name: ClientName,
    /// The user name map `map=` names.
    pub map: Option<String>,
    /// The line it came from, for a message about it.
    pub line: usize,
}

/// A connection, as a line is matched against it.
pub(crate) struct Connection<'a> {
    pub tls: bool,
    pub address: IpAddr,
    /// This host's interfaces, as `(address, netmask)`, for `samehost` and
    /// `samenet`.
    pub host: Vec<(IpAddr, Option<IpAddr>)>,
    pub user: &'a str,
    pub database: &'a str,
}

/// The file's lines, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Hba {
    rules: Vec<Rule>,
}

impl Hba {
    /// Read the file at `path`. An error names the file and the line.
    pub(crate) fn read(path: &Path) -> io::Result<Hba> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Hba::parse_in(&text, path.parent()).map_err(|(line, message)| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}, line {line}: {message}", path.display()),
            )
        })
    }

    pub(crate) fn parse(text: &str) -> Result<Hba, (usize, String)> {
        Hba::parse_in(text, None)
    }

    /// [`Hba::parse`], an `@file` read relative to `dir` (the hba file's).
    pub(crate) fn parse_in(text: &str, dir: Option<&Path>) -> Result<Hba, (usize, String)> {
        let mut rules = Vec::new();
        for (number, line) in logical_lines(text) {
            let fields = fields(&line, dir, 0).map_err(|message| (number, message))?;
            if fields.is_empty() {
                continue;
            }
            rules.push(rule(&fields, number).map_err(|message| (number, message))?);
        }
        Ok(Hba { rules })
    }

    /// The line that decides `connection`: the first that matches it.
    /// `member_of(user, role)` answers whether a user is a member of a role,
    /// a role being a member of itself.
    pub(crate) fn decide(
        &self,
        connection: &Connection<'_>,
        member_of: &dyn Fn(&str, &str) -> bool,
    ) -> Option<&Rule> {
        self.rules.iter().find(|rule| {
            let transport = match rule.transport {
                Transport::Any => true,
                Transport::Tls => connection.tls,
                Transport::Plain => !connection.tls,
                Transport::Never => false,
            };
            transport
                && rule.address.holds(connection)
                && rule.databases.iter().any(|database| match database {
                    Database::All => true,
                    Database::SameUser => connection.database == connection.user,
                    Database::SameRole => member_of(connection.user, connection.database),
                    Database::Replication => false,
                    Database::Name(name) => name == connection.database,
                })
                && rule.users.iter().any(|user| match user {
                    User::All => true,
                    User::Name(name) => name == connection.user,
                    User::MemberOf(role) => member_of(connection.user, role),
                })
        })
    }

    /// Every user name map a line names.
    pub(crate) fn maps(&self) -> impl Iterator<Item = (&str, usize)> {
        self.rules
            .iter()
            .filter_map(|rule| rule.map.as_deref().map(|map| (map, rule.line)))
    }

    /// Does any line look at a client certificate?
    pub(crate) fn asks_for_certificates(&self) -> Option<usize> {
        self.rules
            .iter()
            .find(|rule| rule.cert_check != CertCheck::None)
            .map(|rule| rule.line)
    }
}

impl Address {
    fn holds(&self, connection: &Connection<'_>) -> bool {
        let client = connection.address.to_canonical();
        match self {
            Address::All => true,
            // `check_same_host_or_net`: any address of any of this host's
            // interfaces, the loopback ones among them.
            Address::SameHost => {
                client.is_loopback()
                    || connection
                        .host
                        .iter()
                        .any(|(a, _)| a.to_canonical() == client)
            }
            // The client on the network of one of this host's interfaces.
            Address::SameNet => connection.host.iter().any(|(a, mask)| {
                let Some(mask) = mask else { return false };
                match (a.to_canonical(), mask, client) {
                    (IpAddr::V4(a), IpAddr::V4(m), IpAddr::V4(c)) => {
                        u32::from(a) & u32::from(*m) == u32::from(c) & u32::from(*m)
                    }
                    (IpAddr::V6(a), IpAddr::V6(m), IpAddr::V6(c)) => {
                        u128::from(a) & u128::from(*m) == u128::from(c) & u128::from(*m)
                    }
                    _ => false,
                }
            }),
            // `check_hostname`: the client's address reverse-resolved, the
            // name matched, and the match confirmed by resolving the name
            // forward to the client's address.
            Address::Host(pattern) => client_host_name(client)
                .is_some_and(|name| hostname_match(pattern, &name) && resolves_to(&name, client)),
            Address::Network(network, prefix) => match (network, client) {
                (IpAddr::V4(network), IpAddr::V4(client)) => {
                    let mask = u32::MAX.checked_shl(32 - u32::from(*prefix)).unwrap_or(0);
                    u32::from(*network) & mask == u32::from(client) & mask
                }
                (IpAddr::V6(network), IpAddr::V6(client)) => {
                    let mask = u128::MAX.checked_shl(128 - u32::from(*prefix)).unwrap_or(0);
                    u128::from(*network) & mask == u128::from(client) & mask
                }
                // PostgreSQL does not match an IPv4 line against an IPv6
                // client or the reverse.
                _ => false,
            },
        }
    }
}

/// A `pg_hba.conf` and the user name maps its lines name.
#[derive(Debug)]
pub(crate) struct HbaPolicy {
    pub hba: Hba,
    maps: HashMap<String, UserMap>,
}

/// Where a policy is read from.
#[derive(Debug, Clone)]
pub(crate) struct HbaFiles {
    pub hba_file: PathBuf,
    /// The `pg_ident.conf` a line's `map=` is looked up in.
    pub ident_file: Option<PathBuf>,
    /// Whether the server has a CA file to check a client certificate
    /// against.
    pub has_ca: bool,
}

impl HbaPolicy {
    /// Read the files. A line the server could not honour is an error here,
    /// when the server starts or reloads, not a login that fails later.
    pub(crate) fn read(files: &HbaFiles) -> io::Result<HbaPolicy> {
        let hba = Hba::read(&files.hba_file)?;
        let at = |line: usize, message: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}, line {line}: {message}", files.hba_file.display()),
            )
        };
        if let Some(line) = hba.asks_for_certificates().filter(|_| !files.has_ca) {
            return Err(at(
                line,
                "a client certificate can only be checked with a CA file (--tls-ca-file)",
            ));
        }
        let mut maps = HashMap::new();
        for (map, line) in hba.maps() {
            let Some(ident_file) = &files.ident_file else {
                return Err(at(line, "map= needs a user name map file (--ident-file)"));
            };
            if !maps.contains_key(map) {
                maps.insert(map.to_string(), crate::ident::read(ident_file, map)?);
            }
        }
        Ok(HbaPolicy { hba, maps })
    }

    /// The user name map a line names.
    pub(crate) fn map(&self, name: &str) -> Option<&UserMap> {
        self.maps.get(name)
    }
}

/// The policy a running server holds, which a reload replaces.
pub(crate) struct LiveHba {
    files: HbaFiles,
    current: RwLock<Arc<HbaPolicy>>,
}

impl LiveHba {
    pub(crate) fn new(files: HbaFiles) -> io::Result<Self> {
        let current = RwLock::new(Arc::new(HbaPolicy::read(&files)?));
        Ok(LiveHba { files, current })
    }

    pub(crate) fn current(&self) -> Arc<HbaPolicy> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn reload(&self) -> io::Result<()> {
        let fresh = Arc::new(HbaPolicy::read(&self.files)?);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        Ok(())
    }
}

/// One token: its text, and whether it was quoted, which makes a keyword an
/// ordinary name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    text: String,
    quoted: bool,
}

impl Token {
    fn keyword(&self, word: &str) -> bool {
        !self.quoted && self.text == word
    }
}

/// The file's lines, a line ending in a backslash joined to the next, each
/// with the number of its first physical line.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (index, line) in text.lines().enumerate() {
        let (number, mut whole) = open.take().unwrap_or((index + 1, String::new()));
        match line.strip_suffix('\\') {
            Some(head) => {
                whole.push_str(head);
                open = Some((number, whole));
            }
            None => {
                whole.push_str(line);
                out.push((number, whole));
            }
        }
    }
    out.extend(open);
    out
}

/// One line's fields, each a comma-separated list of tokens. Blanks separate
/// fields, a `#` outside quotes starts a comment, and double quotes keep
/// blanks, commas and `#` in a token (`""` inside them is one quote).
fn fields(line: &str, dir: Option<&Path>, depth: usize) -> Result<Vec<Vec<Token>>, String> {
    let mut out: Vec<Vec<Token>> = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.peek() {
            None | Some('#') => return Ok(out),
            Some(_) => {}
        }
        let mut field: Vec<Token> = Vec::new();
        loop {
            let mut text = String::new();
            let mut quoted = false;
            let mut was_quoted = false;
            let mut more = false;
            while let Some(&c) = chars.peek() {
                if !quoted && (c.is_whitespace() || c == '#') {
                    break;
                }
                chars.next();
                match c {
                    '"' if quoted && chars.peek() == Some(&'"') => {
                        chars.next();
                        text.push('"');
                    }
                    '"' => {
                        quoted = !quoted;
                        was_quoted = true;
                    }
                    ',' if !quoted => {
                        more = true;
                        break;
                    }
                    _ => text.push(c),
                }
            }
            if quoted {
                return Err("unterminated quoted string".into());
            }
            // `@file` (unquoted): the tokens that file holds, in its place
            // (`tokenize_inc_file`), relative to the hba file's directory.
            if !was_quoted && text.len() > 1 && text.starts_with('@') {
                field.extend(included(&text[1..], dir, depth)?);
                if !more {
                    break;
                }
                continue;
            }
            if !text.is_empty() || was_quoted {
                field.push(Token {
                    text,
                    quoted: was_quoted,
                });
            }
            if !more {
                break;
            }
        }
        if !field.is_empty() {
            out.push(field);
        }
    }
}

/// The tokens of an `@file`: every token of every line, comments dropped.
fn included(name: &str, dir: Option<&Path>, depth: usize) -> Result<Vec<Token>, String> {
    if depth >= 10 {
        return Err(format!(
            "could not open file \"{name}\": too many levels of inclusion"
        ));
    }
    let path = match dir {
        Some(dir) if Path::new(name).is_relative() => dir.join(name),
        _ => Path::new(name).to_path_buf(),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "could not open secondary authentication file \"@{name}\" as \"{}\": {e}",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for (_, line) in logical_lines(&text) {
        for field in fields(&line, path.parent(), depth + 1)? {
            out.extend(field);
        }
    }
    Ok(out)
}

/// A field that must be one token.
fn single<'a>(field: Option<&'a Vec<Token>>, what: &str) -> Result<&'a Token, String> {
    match field.map(Vec::as_slice) {
        None => Err(format!("end-of-line before {what}")),
        Some([token]) => Ok(token),
        Some(_) => Err(format!("multiple values specified for {what}")),
    }
}

fn rule(fields: &[Vec<Token>], line: usize) -> Result<Rule, String> {
    let kind = single(fields.first(), "connection type")?;
    let transport = match kind.text.as_str() {
        "host" | "hostnogssenc" => Transport::Any,
        "hostssl" => Transport::Tls,
        "hostnossl" => Transport::Plain,
        "hostgssenc" => Transport::Never,
        "local" => Transport::Never,
        other => return Err(format!("invalid connection type \"{other}\"")),
    };
    let local = kind.text == "local";
    let databases = fields
        .get(1)
        .ok_or("end-of-line before database specification")?
        .iter()
        .map(|token| {
            if token.keyword("all") {
                Database::All
            } else if token.keyword("sameuser") {
                Database::SameUser
            } else if token.keyword("samerole") || token.keyword("samegroup") {
                Database::SameRole
            } else if token.keyword("replication") {
                Database::Replication
            } else {
                Database::Name(token.text.clone())
            }
        })
        .collect();
    let users = fields
        .get(2)
        .ok_or("end-of-line before role specification")?
        .iter()
        .map(|token| {
            if token.keyword("all") {
                User::All
            } else if let Some(role) = token.text.strip_prefix('+').filter(|_| !token.quoted) {
                User::MemberOf(role.to_string())
            } else {
                User::Name(token.text.clone())
            }
        })
        .collect();
    // A `local` line has no address; every other type has one, and an
    // address without a prefix length takes its mask from the next field.
    let mut next = 3;
    let address = if local {
        Address::All
    } else {
        let token = single(fields.get(next), "IP address specification")?;
        next += 1;
        if token.keyword("all") {
            Address::All
        } else if token.keyword("samehost") {
            Address::SameHost
        } else if token.keyword("samenet") {
            Address::SameNet
        } else if let Some((ip, prefix)) = token.text.split_once('/') {
            let ip: IpAddr = ip
                .parse()
                .map_err(|_| format!("invalid IP address \"{ip}\""))?;
            let bits = if ip.is_ipv4() { 32 } else { 128 };
            let prefix: u8 = prefix
                .parse()
                .ok()
                .filter(|p| *p <= bits)
                .ok_or_else(|| format!("invalid CIDR mask in address \"{}\"", token.text))?;
            Address::Network(ip, prefix)
        } else if let Ok(ip) = token.text.parse::<IpAddr>() {
            let mask = single(fields.get(next), "netmask specification")?;
            next += 1;
            let mask: IpAddr = mask
                .text
                .parse()
                .map_err(|_| format!("invalid IP mask \"{}\"", mask.text))?;
            let prefix = match (ip, mask) {
                (IpAddr::V4(_), IpAddr::V4(mask)) => contiguous(u128::from(u32::from(mask)), 32),
                (IpAddr::V6(_), IpAddr::V6(mask)) => contiguous(u128::from(mask), 128),
                _ => return Err("IP address and mask do not match".into()),
            }
            .ok_or_else(|| format!("invalid IP mask \"{mask}\": not a run of leading ones"))?;
            Address::Network(ip, prefix)
        } else {
            Address::Host(token.text.clone())
        }
    };
    let method_token = single(fields.get(next), "authentication method")?;
    next += 1;
    let method = match method_token.text.as_str() {
        "trust" => Method::Trust,
        "reject" => Method::Reject,
        "scram-sha-256" => Method::Scram,
        "md5" => Method::Md5,
        "password" => Method::Password,
        "cert" => Method::Cert,
        "peer" | "ident" | "gss" | "sspi" | "ldap" | "radius" | "pam" | "bsd" => {
            return Err(format!(
                "authentication method \"{}\" is not supported",
                method_token.text
            ))
        }
        other => return Err(format!("invalid authentication method \"{other}\"")),
    };
    if method == Method::Cert && transport != Transport::Tls {
        return Err("cert authentication is only supported on hostssl connections".into());
    }
    let mut cert_check = if method == Method::Cert {
        CertCheck::VerifyFull
    } else {
        CertCheck::None
    };
    let mut client_name = None;
    let mut map = None;
    for field in &fields[next..] {
        let token = single(Some(field), "authentication option")?;
        let (name, value) = token.text.split_once('=').ok_or_else(|| {
            format!(
                "authentication option not in name=value format: {}",
                token.text
            )
        })?;
        match name {
            "clientcert" => {
                if transport != Transport::Tls {
                    return Err("clientcert can only be configured for \"hostssl\" rows".into());
                }
                cert_check = match value {
                    "verify-full" => CertCheck::VerifyFull,
                    "verify-ca" if method == Method::Cert => {
                        return Err(
                            "clientcert only accepts \"verify-full\" when using \"cert\" authentication"
                                .into(),
                        )
                    }
                    "verify-ca" => CertCheck::VerifyCa,
                    other => return Err(format!("invalid value for clientcert: \"{other}\"")),
                };
            }
            "clientname" => {
                if transport != Transport::Tls {
                    return Err("clientname can only be configured for \"hostssl\" rows".into());
                }
                client_name = Some(match value {
                    "CN" => ClientName::CommonName,
                    "DN" => ClientName::DistinguishedName,
                    other => return Err(format!("invalid value for clientname: \"{other}\"")),
                });
            }
            "map" => {
                if method != Method::Cert {
                    return Err(
                        "map can only be configured for the cert authentication method here".into(),
                    );
                }
                map = Some(value.to_string());
            }
            other => {
                return Err(format!(
                    "unrecognized authentication option name: \"{other}\""
                ))
            }
        }
    }
    Ok(Rule {
        transport,
        databases,
        users,
        address,
        method,
        cert_check,
        client_name: client_name.unwrap_or_default(),
        map,
        line,
    })
}

/// The prefix length of a mask that is a run of ones and then zeros.
fn contiguous(mask: u128, bits: u32) -> Option<u8> {
    let ones = (mask << (128 - bits)).leading_ones();
    let expected = if ones == 0 {
        0
    } else {
        (u128::MAX << (128 - ones)) >> (128 - bits)
    };
    (mask == expected).then_some(ones as u8)
}

/// `hostname_match`: a pattern starting with `.` matches a name ending in
/// it, any other the name itself; case is not significant.
fn hostname_match(pattern: &str, name: &str) -> bool {
    if pattern.starts_with('.') {
        name.len() > pattern.len()
            && name[name.len() - pattern.len()..].eq_ignore_ascii_case(pattern)
    } else {
        name.eq_ignore_ascii_case(pattern)
    }
}

/// Does `name` resolve (forward) to `client`?
fn resolves_to(name: &str, client: IpAddr) -> bool {
    use std::net::ToSocketAddrs;
    (name, 0)
        .to_socket_addrs()
        .is_ok_and(|mut addrs| addrs.any(|a| a.ip().to_canonical() == client))
}

/// The client address's host name, from a reverse lookup that must find
/// one (`getnameinfo` with `NI_NAMEREQD`).
#[cfg(unix)]
fn client_host_name(client: IpAddr) -> Option<String> {
    let mut host = [0 as libc::c_char; 1025];
    // SAFETY: `getnameinfo` reads the sockaddr built here and writes a
    // NUL-terminated name into `host`, within its length.
    let rc = unsafe {
        match client {
            IpAddr::V4(v4) => {
                let mut sa: libc::sockaddr_in = std::mem::zeroed();
                sa.sin_family = libc::AF_INET as libc::sa_family_t;
                sa.sin_addr.s_addr = u32::from(v4).to_be();
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                {
                    sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
                }
                libc::getnameinfo(
                    std::ptr::addr_of!(sa).cast(),
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    host.as_mut_ptr(),
                    host.len() as libc::socklen_t,
                    std::ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
            IpAddr::V6(v6) => {
                let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
                sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                sa.sin6_addr.s6_addr = v6.octets();
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                {
                    sa.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
                }
                libc::getnameinfo(
                    std::ptr::addr_of!(sa).cast(),
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                    host.as_mut_ptr(),
                    host.len() as libc::socklen_t,
                    std::ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
        }
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: `getnameinfo` NUL-terminated it.
    let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) };
    name.to_str().ok().map(str::to_string)
}

#[cfg(not(unix))]
fn client_host_name(_client: IpAddr) -> Option<String> {
    None
}

/// This host's interfaces as `(address, netmask)`, as `pg_foreach_ifaddr`
/// lists them (`getifaddrs`).
#[cfg(unix)]
pub(crate) fn host_addresses() -> Vec<(IpAddr, Option<IpAddr>)> {
    // SAFETY (both): `sa` is null or a sockaddr whose family says its type.
    unsafe fn ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
        if sa.is_null() {
            return None;
        }
        match i32::from((*sa).sa_family) {
            libc::AF_INET => {
                let sin = sa.cast::<libc::sockaddr_in>();
                Some(IpAddr::V4(std::net::Ipv4Addr::from(u32::from_be(
                    (*sin).sin_addr.s_addr,
                ))))
            }
            libc::AF_INET6 => {
                let sin6 = sa.cast::<libc::sockaddr_in6>();
                Some(IpAddr::V6(std::net::Ipv6Addr::from(
                    (*sin6).sin6_addr.s6_addr,
                )))
            }
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills `head` with a list this function walks and
    // frees.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            if let Some(addr) = ip((*cur).ifa_addr) {
                out.push((addr, ip((*cur).ifa_netmask)));
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(unix))]
pub(crate) fn host_addresses() -> Vec<(IpAddr, Option<IpAddr>)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# TYPE      DATABASE   USER        ADDRESS                    METHOD
host        all        postgres    127.0.0.1/32               trust
hostnossl   all        carol       127.0.0.1/32               trust
hostssl     all        carol       127.0.0.1/32               reject
host        appdb      appuser     127.0.0.1/32               scram-sha-256
host        all        alice,bob   127.0.0.1/32               scram-sha-256
host        all        +team       127.0.0.1/32               trust
host        sameuser   erin        127.0.0.1/32               trust
host        samerole   frank       127.0.0.1/32               trust
host        all        dave        10.0.0.0/8                 trust
host        all        dave        127.0.0.1  255.255.255.255 password
host        all        erin        samehost                   md5
hostssl     all        frank       all            scram-sha-256  clientcert=verify-ca
local       all        all                                    trust
host        \"all\"      \"+odd\"      ::1/128                    trust
hostssl     all        mapped      all            cert  map=m clientname=DN
";

    fn decide<'a>(hba: &'a Hba, tls: bool, user: &str, database: &str) -> Option<&'a Rule> {
        decide_from(hba, tls, user, database, "127.0.0.1")
    }

    fn decide_from<'a>(
        hba: &'a Hba,
        tls: bool,
        user: &str,
        database: &str,
        address: &str,
    ) -> Option<&'a Rule> {
        let member = |user: &str, role: &str| user == role || (user == "member" && role == "team");
        hba.decide(
            &Connection {
                tls,
                address: address.parse().unwrap(),
                host: Vec::new(),
                user,
                database,
            },
            &member,
        )
    }

    #[test]
    fn the_first_matching_line_decides() {
        let hba = Hba::parse(FILE).expect("the file parses");
        let method = |tls, user, database| decide(&hba, tls, user, database).map(|r| r.method);
        assert_eq!(method(false, "postgres", "postgres"), Some(Method::Trust));
        // The type: carol is trusted in the clear and rejected over TLS.
        assert_eq!(method(false, "carol", "postgres"), Some(Method::Trust));
        assert_eq!(method(true, "carol", "postgres"), Some(Method::Reject));
        // The database: appuser has a line for appdb and none for postgres.
        assert_eq!(method(false, "appuser", "appdb"), Some(Method::Scram));
        assert_eq!(method(false, "appuser", "postgres"), None);
        // A list of users, and a role's members.
        assert_eq!(method(false, "bob", "postgres"), Some(Method::Scram));
        assert_eq!(method(false, "member", "postgres"), Some(Method::Trust));
        assert_eq!(method(false, "team", "postgres"), Some(Method::Trust));
        // sameuser and samerole.
        assert_eq!(method(false, "erin", "erin"), Some(Method::Trust));
        assert_eq!(method(false, "erin", "postgres"), Some(Method::Md5));
        assert_eq!(method(false, "frank", "frank"), Some(Method::Trust));
        assert_eq!(method(false, "frank", "team"), None);
        // An address that does not hold falls through to the next line.
        assert_eq!(method(false, "dave", "postgres"), Some(Method::Password));
        assert_eq!(
            decide_from(&hba, false, "dave", "postgres", "10.1.2.3").map(|r| r.method),
            Some(Method::Trust)
        );
        // Nobody has a line.
        assert_eq!(method(false, "nobody", "postgres"), None);
    }

    #[test]
    fn options_and_quoting_are_read() {
        let hba = Hba::parse(FILE).expect("the file parses");
        let frank = decide(&hba, true, "frank", "postgres").expect("frank over TLS");
        assert_eq!(
            (frank.method, frank.cert_check),
            (Method::Scram, CertCheck::VerifyCa)
        );
        let mapped = decide(&hba, true, "mapped", "postgres").expect("mapped");
        assert_eq!(mapped.method, Method::Cert);
        assert_eq!(mapped.cert_check, CertCheck::VerifyFull);
        assert_eq!(mapped.client_name, ClientName::DistinguishedName);
        assert_eq!(mapped.map.as_deref(), Some("m"));
        assert_eq!(hba.maps().collect::<Vec<_>>(), [("m", mapped.line)]);
        assert_eq!(hba.asks_for_certificates(), Some(frank.line));
        // Quoted, `all` is a database called all and `+odd` a user called +odd.
        assert!(decide_from(&hba, false, "+odd", "all", "::1").is_some());
        assert!(decide_from(&hba, false, "+odd", "postgres", "::1").is_none());
        assert!(decide_from(&hba, false, "odd", "all", "::1").is_none());
        // A `local` line matches nothing that arrives over TCP.
        assert!(decide(&hba, false, "anyone", "postgres").is_none());
    }

    #[test]
    fn an_address_matches_by_its_network() {
        let hba = Hba::parse(
            "host all a 192.168.0.0/16 trust\nhost all b 192.168.4.0 255.255.252.0 trust\n\
             host all c ::ffff:0:0/96 trust\nhost all d 0.0.0.0/0 trust\nhost all e samehost trust\n",
        )
        .expect("parses");
        let hit = |user, address| decide_from(&hba, false, user, "x", address).is_some();
        assert!(hit("a", "192.168.255.1") && !hit("a", "192.169.0.1"));
        assert!(hit("b", "192.168.7.255") && !hit("b", "192.168.8.0"));
        // An IPv4 client that arrived as a mapped IPv6 address is IPv4.
        assert!(hit("d", "::ffff:10.0.0.1") && hit("d", "10.0.0.1"));
        assert!(!hit("d", "::1"), "an IPv4 line does not match IPv6");
        assert!(hit("e", "127.0.0.1") && hit("e", "::1") && !hit("e", "10.0.0.1"));
        // `samehost` is any of this host's interface addresses, not only the
        // loopback ones.
        let on = |address: &str, host: &[&str]| {
            hba.decide(
                &Connection {
                    tls: false,
                    address: address.parse().unwrap(),
                    host: host.iter().map(|a| (a.parse().unwrap(), None)).collect(),
                    user: "e",
                    database: "x",
                },
                &|_, _| false,
            )
            .is_some()
        };
        assert!(on("192.0.2.7", &["10.0.0.1", "192.0.2.7"]));
        assert!(!on("192.0.2.8", &["10.0.0.1", "192.0.2.7"]));
        assert!(on("::ffff:192.0.2.7", &["192.0.2.7"]));
        // `samenet`: the client on an interface's network.
        let hba = Hba::parse("host all all samenet trust\n").expect("samenet parses");
        let net = |address: &str| {
            hba.decide(
                &Connection {
                    tls: false,
                    address: address.parse().unwrap(),
                    host: vec![(
                        "192.0.2.7".parse().unwrap(),
                        Some("255.255.255.0".parse().unwrap()),
                    )],
                    user: "u",
                    database: "x",
                },
                &|_, _| false,
            )
            .is_some()
        };
        assert!(net("192.0.2.200") && !net("192.0.3.1") && !net("::1"));
    }

    #[test]
    fn a_host_name_matches_whole_or_by_its_suffix() {
        assert!(hostname_match("localhost", "LocalHost"));
        assert!(!hostname_match("localhost", "localhost.example"));
        assert!(hostname_match(".example.com", "db.Example.com"));
        assert!(!hostname_match(".example.com", "example.com"));
        assert!(!hostname_match(".example.com", "db.example.org"));
    }

    #[test]
    fn an_at_file_holds_more_tokens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("admins"),
            "alice  # a comment\nbob,carol\n@more\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("more"), "dave\n").unwrap();
        let hba = Hba::parse_in("host all @admins all trust\n", Some(dir.path()))
            .expect("the file parses");
        for user in ["alice", "bob", "carol", "dave"] {
            assert!(
                decide_from(&hba, false, user, "x", "127.0.0.1").is_some(),
                "{user}"
            );
        }
        assert!(decide_from(&hba, false, "erin", "x", "127.0.0.1").is_none());
        // Quoted, `@admins` is a user called @admins.
        let quoted =
            Hba::parse_in("host all \"@admins\" all trust\n", Some(dir.path())).expect("parses");
        assert!(decide_from(&quoted, false, "@admins", "x", "127.0.0.1").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn this_hosts_addresses_are_listed() {
        let addresses = host_addresses();
        assert!(
            addresses.iter().any(|(a, _)| a.is_loopback()),
            "getifaddrs lists the loopback interface: {addresses:?}"
        );
    }

    #[test]
    fn a_line_this_server_cannot_apply_is_an_error_with_its_number() {
        for (text, part) in [
            (
                "host all all 127.0.0.1/32 peer\n",
                "\"peer\" is not supported",
            ),
            (
                "host all all 127.0.0.1/32 ldap\n",
                "\"ldap\" is not supported",
            ),
            (
                "host all all 127.0.0.1/32 trusted\n",
                "invalid authentication method",
            ),
            ("host all all 127.0.0.1/33 trust\n", "invalid CIDR mask"),
            (
                "host all all 127.0.0.1 255.0.255.0 trust\n",
                "not a run of leading ones",
            ),
            ("host all all 127.0.0.1 trust\n", "invalid IP mask"),
            (
                "host all all 127.0.0.1/32\n",
                "end-of-line before authentication method",
            ),
            ("host all\n", "end-of-line before role"),
            ("hostess all all all trust\n", "invalid connection type"),
            ("host all all all cert\n", "only supported on hostssl"),
            (
                "host all all all trust clientcert=verify-ca\n",
                "\"hostssl\" rows",
            ),
            (
                "hostssl all all all cert clientcert=verify-ca\n",
                "only accepts \"verify-full\"",
            ),
            (
                "hostssl all all all trust clientcert=yes\n",
                "invalid value for clientcert",
            ),
            ("hostssl all all all scram-sha-256 map=m\n", "map can only"),
            ("host all all all trust banana\n", "name=value"),
            (
                "host all all all trust colour=blue\n",
                "unrecognized authentication option",
            ),
            (
                "host all @absent-users-file all trust\n",
                "could not open secondary authentication file",
            ),
            ("host all \"alice all trust\n", "unterminated"),
            (
                "host all all 127.0.0.1/32,10.0.0.0/8 trust\n",
                "multiple values",
            ),
        ] {
            let (line, message) = Hba::parse(&format!("\n{text}")).expect_err(text);
            assert_eq!(line, 2, "{text}");
            assert!(message.contains(part), "{text}: {message}");
        }
    }

    #[test]
    fn a_mask_is_a_prefix_length() {
        assert_eq!(contiguous(0xffff_ff00, 32), Some(24));
        assert_eq!(contiguous(0, 32), Some(0));
        assert_eq!(contiguous(0xffff_ffff, 32), Some(32));
        assert_eq!(contiguous(0xff00_ff00, 32), None);
        assert_eq!(contiguous(u128::MAX, 128), Some(128));
        assert_eq!(contiguous(u128::MAX << 64, 128), Some(64));
    }
}
