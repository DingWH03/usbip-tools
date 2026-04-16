use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result};
use get_if_addrs::{get_if_addrs, IfAddr};
use serde::Deserialize;
use tokio::net::UdpSocket;

const MAGIC: &[u8] = b"USBIP_DISCOVER\n";

#[derive(Debug, Clone)]
pub struct DiscoveryCfg {
    pub targets: Vec<SocketAddr>,
    pub timeout: Duration,
    pub max_replies: usize,
}

impl Default for DiscoveryCfg {
    fn default() -> Self {
        let mut targets = compute_ipv4_broadcast_targets(3240);
        // Fallback to limited broadcast.
        targets.push(SocketAddr::new(IpAddr::from([255, 255, 255, 255]), 3240));
        targets.sort_by_key(|a| a.to_string());
        targets.dedup();
        Self {
            targets,
            timeout: Duration::from_millis(900),
            max_replies: 64,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoverResp {
    pub server_name: Option<String>,
    pub web_url: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub addr: SocketAddr,
    pub info: DiscoverResp,
}

pub async fn discover(cfg: DiscoveryCfg) -> Result<Vec<DiscoveredServer>> {
    // Bind ephemeral UDP port for receiving replies.
    let sock = UdpSocket::bind("0.0.0.0:0").await.context("bind udp")?;
    sock.set_broadcast(true).context("set broadcast")?;

    for t in &cfg.targets {
        let _ = sock.send_to(MAGIC, *t).await;
    }

    let deadline = tokio::time::Instant::now() + cfg.timeout;
    let mut buf = [0u8; 2048];
    let mut uniq: BTreeMap<SocketAddr, DiscoverResp> = BTreeMap::new();

    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        if uniq.len() >= cfg.max_replies {
            break;
        }

        let recv = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await;
        let Ok(Ok((n, peer))) = recv else {
            break;
        };
        if n == 0 {
            continue;
        }
        let info: Result<DiscoverResp> =
            serde_json::from_slice(&buf[..n]).context("parse discover json");
        match info {
            Ok(v) => {
                uniq.insert(peer, v);
            }
            Err(err) => {
                tracing::debug!(%peer, %err, "invalid discover reply");
            }
        }
    }

    Ok(uniq
        .into_iter()
        .map(|(addr, info)| DiscoveredServer { addr, info })
        .collect())
}

fn compute_ipv4_broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    let ifs = match get_if_addrs() {
        Ok(v) => v,
        Err(_) => return out,
    };
    for iface in ifs {
        let IfAddr::V4(v4) = iface.addr else { continue };
        let ip = v4.ip;
        let mask = v4.netmask;
        // Skip loopback and invalid masks.
        if ip.is_loopback() {
            continue;
        }
        let bcast = ipv4_broadcast(ip, mask);
        if bcast != Ipv4Addr::UNSPECIFIED {
            out.push(SocketAddr::new(IpAddr::V4(bcast), port));
        }
    }
    out
}

fn ipv4_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    let ip_u = u32::from(ip);
    let mask_u = u32::from(netmask);
    if mask_u == 0 {
        return Ipv4Addr::UNSPECIFIED;
    }
    Ipv4Addr::from(ip_u | !mask_u)
}
