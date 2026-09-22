//! Read-only address discovery; never invokes Serve or modifies Tailscale.
use super::Address;
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::io::AsyncReadExt;

#[derive(Clone)]
pub(super) struct Found {
    pub hosts: Vec<(String, String)>,
    pub notice: Option<&'static str>,
}
impl Found {
    pub fn addresses(&self, port: u16) -> Vec<Address> {
        self.hosts
            .iter()
            .take(16)
            .map(|(host, label)| Address {
                id: host.clone(),
                label: label.clone(),
                origin: format!("http://{host}:{port}"),
            })
            .collect()
    }
}
fn lan() -> Vec<(String, String)> {
    let mut root = std::ptr::null_mut();
    // SAFETY: getifaddrs owns a linked allocation released once below; family
    // and null checks precede reading each IPv4 sockaddr.
    unsafe {
        if libc::getifaddrs(&mut root) != 0 {
            return vec![];
        }
        let mut current = root;
        let mut hosts = vec![];
        while !current.is_null() {
            let iface = &*current;
            if !iface.ifa_addr.is_null()
                && (*iface.ifa_addr).sa_family as i32 == libc::AF_INET
                && iface.ifa_flags & libc::IFF_UP as u32 != 0
            {
                let addr = &*(iface.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                if ip.is_private() {
                    hosts.push(ip);
                }
            }
            current = iface.ifa_next;
        }
        libc::freeifaddrs(root);
        hosts.sort();
        hosts.dedup();
        hosts
            .into_iter()
            .take(12)
            .map(|ip| (ip.to_string(), "局域网".into()))
            .collect()
    }
}
fn dns(value: &str) -> Option<String> {
    let value = value.trim_end_matches('.').to_ascii_lowercase();
    (value.len() <= 253
        && value.ends_with(".ts.net")
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }))
    .then_some(value)
}
fn parse(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("BackendState")?.as_str()? != "Running" {
        return None;
    }
    let own = value.get("Self")?;
    let mut hosts = vec![];
    if let Some(name) = own.get("DNSName").and_then(|v| v.as_str()).and_then(dns) {
        hosts.push((name, "Tailscale".into()));
    }
    for ip in own
        .get("TailscaleIPs")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .filter_map(|v| v.parse::<Ipv4Addr>().ok())
    {
        let octets = ip.octets();
        if octets[0] == 100 && (64..=127).contains(&octets[1]) && hosts.is_empty() {
            hosts.push((ip.to_string(), "Tailscale IP".into()));
        }
    }
    (!hosts.is_empty()).then_some(hosts)
}
pub(super) async fn discover() -> Found {
    let mut hosts = tokio::task::spawn_blocking(lan).await.unwrap_or_default();
    let executable = if cfg!(target_os = "macos")
        && std::path::Path::new("/Applications/Tailscale.app/Contents/MacOS/tailscale").is_file()
    {
        "/Applications/Tailscale.app/Contents/MacOS/tailscale"
    } else {
        "tailscale"
    };
    let read = async {
        let mut child = tokio::process::Command::new(executable)
            .args(["status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()?
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > 256 * 1024 {
            return None;
        }
        if !child.wait().await.ok()?.success() {
            return None;
        }
        parse(&bytes)
    };
    let tailscale = tokio::time::timeout(Duration::from_secs(3), read)
        .await
        .ok()
        .flatten();
    let notice = if let Some(found) = tailscale {
        hosts.extend(found);
        None
    } else {
        Some("tailscale_unavailable")
    };
    Found { hosts, notice }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_self_names_are_accepted_and_no_peer_or_injected_url_is_advertised() {
        assert!(parse(br#"{"BackendState":"Stopped"}"#).is_none());
        assert!(dns("bad/x.ts.net").is_none());
        assert!(dns("other.example").is_none());
        assert_eq!(parse(br#"{"BackendState":"Running","Self":{"DNSName":"machine.tail123.ts.net.","TailscaleIPs":["100.80.0.1"]},"Peer":{"private":"ignored"}}"#).unwrap(),vec![("machine.tail123.ts.net".into(),"Tailscale".into())]);
    }
}
