//! Provisioning, deployment, the timed run, and teardown.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::doapi::Api;
use crate::remote;
use crate::BenchResult;
use crate::{ALL_ROLES, SERVER_ROLE};

pub const DEFAULT_PREFIX: &str = "secantus-bench";
pub const DEFAULT_REGION: &str = "lon1";
// CPU-optimized (dedicated hyperthread), NOT shared-CPU `s-*`. A shared
// droplet's steal time varies with whoever else is on the host, which makes
// run-to-run comparison meaningless — the one thing a perf harness must not do.
pub const DEFAULT_SERVER_SIZE: &str = "c-4";
pub const DEFAULT_CLIENT_SIZE: &str = "c-2";
pub const DEFAULT_IMAGE: &str = "ubuntu-24-04-x64";

#[derive(Debug, Clone)]
pub struct Config {
    pub prefix: String,
    pub region: String,
    pub server_size: String,
    pub client_size: String,
    pub image: String,
    pub ssh_key: PathBuf,
    pub ssh_cidr: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: DEFAULT_PREFIX.to_string(),
            region: DEFAULT_REGION.to_string(),
            server_size: DEFAULT_SERVER_SIZE.to_string(),
            client_size: DEFAULT_CLIENT_SIZE.to_string(),
            image: DEFAULT_IMAGE.to_string(),
            ssh_key: remote::default_ssh_key(),
            ssh_cidr: String::new(),
        }
    }
}

impl Config {
    pub fn droplet_name(&self, role: &str) -> String {
        format!("{}-{role}", self.prefix)
    }

    pub fn snapshot_name(&self, role: &str) -> String {
        format!("{}-{role}-snap", self.prefix)
    }

    pub fn size_for(&self, role: &str) -> &str {
        if role == SERVER_ROLE {
            &self.server_size
        } else {
            &self.client_size
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub role: String,
    pub droplet: Value,
}

impl Node {
    pub fn id(&self) -> i64 {
        self.droplet.get("id").and_then(|v| v.as_i64()).unwrap_or(0)
    }

    pub fn name(&self) -> String {
        self.droplet
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    pub fn status(&self) -> String {
        self.droplet
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    }

    pub fn public(&self) -> String {
        network_ip(&self.droplet, "public")
    }

    pub fn private(&self) -> String {
        network_ip(&self.droplet, "private")
    }

    pub fn memory_mb(&self) -> u64 {
        self.droplet
            .get("memory")
            .and_then(|v| v.as_u64())
            .unwrap_or(2048)
    }

    pub fn vcpus(&self) -> u64 {
        self.droplet
            .get("vcpus")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }

    pub fn size_slug(&self) -> String {
        self.droplet
            .get("size_slug")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
}

pub fn network_ip(droplet: &Value, kind: &str) -> String {
    droplet
        .get("networks")
        .and_then(|n| n.get("v4"))
        .and_then(|v| v.as_array())
        .map(|nets| {
            nets.iter()
                .find(|n| n.get("type").and_then(|t| t.as_str()) == Some(kind))
                .and_then(|n| n.get("ip_address").and_then(|i| i.as_str()))
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default()
}

/// Fill in this machine's IP when the caller gave none, and accept a bare
/// address as a /32.
///
/// An empty CIDR would otherwise reach the API as `addresses: [""]` and fail
/// the very first `up`.
pub fn normalise_ssh_cidr(
    value: &str,
    detect: &dyn Fn() -> BenchResult<String>,
) -> BenchResult<String> {
    if value.is_empty() {
        return Ok(format!("{}/32", detect()?));
    }
    if !value.contains('/') {
        return Ok(format!("{value}/32"));
    }
    Ok(value.to_string())
}

/// This machine's public IP, for the SSH firewall rule.
pub fn detect_public_ip() -> BenchResult<String> {
    for url in ["https://checkip.amazonaws.com", "https://api.ipify.org"] {
        let out = Command::new("curl")
            .args(["--silent", "--max-time", "15", url])
            .output();
        if let Ok(out) = out {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if ip.matches('.').count() == 3 && !ip.is_empty() {
                return Ok(ip);
            }
        }
    }
    Err(
        "Could not detect this machine's public IP for the SSH firewall rule.\n\
         Pass it explicitly: --ssh-cidr 203.0.113.7/32 (or a wider range if your IP moves)."
            .to_string(),
    )
}

pub const CLOUD_INIT: &str = r#"#cloud-config
package_update: true
packages:
  - curl
  - ca-certificates
  - chrony
write_files:
  - path: /etc/sysctl.d/99-secantus-bench.conf
    content: |
      net.core.somaxconn = 4096
      net.ipv4.tcp_max_syn_backlog = 4096
      net.ipv4.ip_local_port_range = 10000 65535
      fs.file-max = 1048576
runcmd:
  - sysctl --system
"#;

// -- resource discovery / creation -----------------------------------------

pub fn discover(api: &Api, cfg: &Config) -> BenchResult<Vec<Node>> {
    let droplets = api.paged(
        "/droplets",
        "droplets",
        &format!("&tag_name={}", cfg.prefix),
    )?;
    let mut out = Vec::new();
    for role in ALL_ROLES {
        let wanted = cfg.droplet_name(role);
        if let Some(d) = droplets
            .iter()
            .find(|d| d.get("name").and_then(|n| n.as_str()) == Some(&wanted))
        {
            out.push(Node {
                role: role.to_string(),
                droplet: d.clone(),
            });
        }
    }
    Ok(out)
}

pub fn node_for<'a>(nodes: &'a [Node], role: &str) -> Option<&'a Node> {
    nodes.iter().find(|n| n.role == role)
}

pub fn ensure_ssh_key(api: &Api, cfg: &Config) -> BenchResult<i64> {
    let pub_path = PathBuf::from(format!("{}.pub", cfg.ssh_key.display()));
    if !pub_path.exists() {
        return Err(format!(
            "No public key at {}. Generate one (ssh-keygen -t ed25519) or point --ssh-key at an \
             existing private key.",
            pub_path.display()
        ));
    }
    let pub_key = std::fs::read_to_string(&pub_path)
        .map_err(|e| format!("reading {}: {e}", pub_path.display()))?
        .trim()
        .to_string();
    // Compare the base64 body, not the whole line: names and comments differ
    // between what is on disk and what the account already holds.
    let body = pub_key
        .split_whitespace()
        .nth(1)
        .unwrap_or(&pub_key)
        .to_string();
    for key in api.paged("/account/keys", "ssh_keys", "")? {
        let stored = key.get("public_key").and_then(|v| v.as_str()).unwrap_or("");
        if stored.split_whitespace().nth(1) == Some(body.as_str()) {
            return Ok(key.get("id").and_then(|v| v.as_i64()).unwrap_or(0));
        }
    }
    let host = hostname();
    let name = format!("{}-{host}", cfg.prefix);
    println!(
        "uploading SSH key {} to DigitalOcean as {name:?}",
        pub_path.display()
    );
    let reply = api.request(
        "POST",
        "/account/keys",
        Some(&json!({"name": name, "public_key": pub_key})),
    )?;
    reply
        .get("ssh_key")
        .and_then(|k| k.get("id"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "DigitalOcean returned no key id".to_string())
}

pub fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A dedicated VPC so client -> server traffic stays on private, free,
/// low-latency networking rather than the public interface.
pub fn ensure_vpc(api: &Api, cfg: &Config) -> BenchResult<String> {
    let name = format!("{}-vpc", cfg.prefix);
    for vpc in api.paged("/vpcs", "vpcs", "")? {
        let matches = vpc.get("name").and_then(|v| v.as_str()) == Some(name.as_str())
            && vpc.get("region").and_then(|v| v.as_str()) == Some(cfg.region.as_str());
        if matches {
            return Ok(vpc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string());
        }
    }
    println!("creating VPC {name} in {}", cfg.region);
    let reply = api.request(
        "POST",
        "/vpcs",
        Some(&json!({"name": name, "region": cfg.region})),
    )?;
    Ok(reply
        .get("vpc")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// The firewall body: SSH from this machine only, the wire port from tagged
/// droplets only.
///
/// The server runs with authentication off — that is what a benchmark wants and
/// what a public MongoDB port absolutely is not — so this is load-bearing
/// security, not decoration.
pub fn firewall_body(cfg: &Config) -> Value {
    let everywhere = json!({"addresses": ["0.0.0.0/0", "::/0"]});
    json!({
        "name": format!("{}-fw", cfg.prefix),
        "inbound_rules": [
            {"protocol": "tcp", "ports": "22", "sources": {"addresses": [cfg.ssh_cidr]}},
            {"protocol": "tcp", "ports": crate::SERVER_PORT.to_string(),
             "sources": {"tags": [cfg.prefix]}},
            {"protocol": "icmp", "sources": {"tags": [cfg.prefix]}}
        ],
        "outbound_rules": [
            {"protocol": "tcp", "ports": "1-65535", "destinations": everywhere},
            {"protocol": "udp", "ports": "1-65535", "destinations": everywhere},
            {"protocol": "icmp", "destinations": everywhere}
        ],
        "tags": [cfg.prefix],
    })
}

pub fn ensure_firewall(api: &Api, cfg: &Config) -> BenchResult<String> {
    let name = format!("{}-fw", cfg.prefix);
    let body = firewall_body(cfg);
    for fw in api.paged("/firewalls", "firewalls", "")? {
        if fw.get("name").and_then(|v| v.as_str()) == Some(name.as_str()) {
            let id = fw
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            api.request("PUT", &format!("/firewalls/{id}"), Some(&body))?;
            return Ok(id);
        }
    }
    println!(
        "creating firewall {name} (ssh from {}, {} from tag {})",
        cfg.ssh_cidr,
        crate::SERVER_PORT,
        cfg.prefix
    );
    let reply = api.request("POST", "/firewalls", Some(&body))?;
    Ok(reply
        .get("firewall")
        .and_then(|f| f.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

pub fn newest_snapshot(snapshots: &[Value], name: &str) -> Option<Value> {
    let mut matches: Vec<&Value> = snapshots
        .iter()
        .filter(|s| s.get("name").and_then(|v| v.as_str()) == Some(name))
        .collect();
    matches.sort_by_key(|s| {
        s.get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    });
    matches.last().map(|s| (*s).clone())
}

pub fn wait_for_status(
    api: &Api,
    droplet_id: i64,
    status: &str,
    timeout: Duration,
) -> BenchResult<Value> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let droplet = api
            .request("GET", &format!("/droplets/{droplet_id}"), None)?
            .get("droplet")
            .cloned()
            .ok_or_else(|| format!("droplet {droplet_id} not found"))?;
        let current = droplet.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if current == status && (status != "active" || !network_ip(&droplet, "public").is_empty()) {
            return Ok(droplet);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "droplet {droplet_id} did not reach status {status:?} within {:.0}s",
                timeout.as_secs_f64()
            ));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

pub fn wait_action(api: &Api, action_id: i64, timeout: Duration) -> BenchResult<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let reply = api.request("GET", &format!("/actions/{action_id}"), None)?;
        match reply
            .get("action")
            .and_then(|a| a.get("status"))
            .and_then(|s| s.as_str())
        {
            Some("completed") => return Ok(()),
            Some("errored") => return Err(format!("action {action_id} errored")),
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "action {action_id} did not complete within {:.0}s",
                timeout.as_secs_f64()
            ));
        }
        std::thread::sleep(Duration::from_secs(4));
    }
}

pub fn droplet_action(api: &Api, droplet_id: i64, body: &Value) -> BenchResult<i64> {
    let reply = api.request(
        "POST",
        &format!("/droplets/{droplet_id}/actions"),
        Some(body),
    )?;
    reply
        .get("action")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "DigitalOcean returned no action id".to_string())
}

pub fn wait_ssh(cfg: &Config, ip: &str, timeout: Duration) -> BenchResult<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(out) = remote::ssh_raw(&cfg.ssh_key, ip, "true") {
            if out.status == 0 {
                return Ok(());
            }
            // A refused connection means "still booting"; a rejected key means
            // "never going to work". Only the first is worth waiting out.
            if let Some(reason) = remote::terminal_ssh_failure(&out.stderr) {
                return Err(format!("ssh to {ip}: {reason}"));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "ssh to {ip} never came up within {:.0}s",
                timeout.as_secs_f64()
            ));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Half of RAM, mirroring mongod's default WiredTiger cache sizing.
pub fn auto_cache_size(memory_mb: u64) -> String {
    let gib = std::cmp::max(1, (memory_mb as f64 * 0.5 / 1024.0) as u64);
    format!("{gib}G")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_names_derive_from_the_prefix() {
        let cfg = Config {
            prefix: "bench-x".into(),
            server_size: "c-8".into(),
            client_size: "c-2".into(),
            ..Config::default()
        };
        assert_eq!(cfg.droplet_name("server"), "bench-x-server");
        assert_eq!(cfg.snapshot_name("client-1"), "bench-x-client-1-snap");
        assert_eq!(cfg.size_for("server"), "c-8");
        assert_eq!(cfg.size_for("client-1"), "c-2");
    }

    #[test]
    fn cache_size_is_half_of_ram_and_never_zero() {
        assert_eq!(auto_cache_size(8192), "4G");
        assert_eq!(auto_cache_size(2048), "1G");
        assert_eq!(auto_cache_size(512), "1G");
    }

    #[test]
    fn ip_helpers_pick_the_right_interface() {
        let droplet = json!({"networks": {"v4": [
            {"type": "public", "ip_address": "203.0.113.9"},
            {"type": "private", "ip_address": "10.106.0.3"}
        ]}});
        assert_eq!(network_ip(&droplet, "public"), "203.0.113.9");
        assert_eq!(network_ip(&droplet, "private"), "10.106.0.3");
        assert_eq!(network_ip(&json!({"networks": {"v4": []}}), "private"), "");
    }

    #[test]
    fn firewall_never_exposes_the_wire_port_to_the_internet() {
        let cfg = Config {
            ssh_cidr: "203.0.113.7/32".into(),
            ..Config::default()
        };
        let body = firewall_body(&cfg);
        let rules = body["inbound_rules"].as_array().unwrap();
        let ssh_rule = rules.iter().find(|r| r["ports"] == "22").unwrap();
        assert_eq!(ssh_rule["sources"]["addresses"][0], "203.0.113.7/32");
        let port = crate::SERVER_PORT.to_string();
        let wire = rules.iter().find(|r| r["ports"] == port).unwrap();
        assert_eq!(wire["sources"]["tags"][0], cfg.prefix);
        assert!(wire["sources"].get("addresses").is_none());
    }

    #[test]
    fn ssh_cidr_is_filled_in_or_widened_to_a_slash_32() {
        let detect = || Ok("203.0.113.7".to_string());
        assert_eq!(normalise_ssh_cidr("", &detect).unwrap(), "203.0.113.7/32");
        assert_eq!(
            normalise_ssh_cidr("198.51.100.4", &detect).unwrap(),
            "198.51.100.4/32"
        );
        assert_eq!(
            normalise_ssh_cidr("198.51.100.0/24", &detect).unwrap(),
            "198.51.100.0/24"
        );
    }

    #[test]
    fn newest_snapshot_wins() {
        let snaps = vec![
            json!({"name": "s", "id": "old", "created_at": "2026-01-01T00:00:00Z"}),
            json!({"name": "s", "id": "new", "created_at": "2026-06-01T00:00:00Z"}),
            json!({"name": "other", "id": "x", "created_at": "2027-01-01T00:00:00Z"}),
        ];
        assert_eq!(newest_snapshot(&snaps, "s").unwrap()["id"], "new");
        assert!(newest_snapshot(&snaps, "absent").is_none());
    }
}
