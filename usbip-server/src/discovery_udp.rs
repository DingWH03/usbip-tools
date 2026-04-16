use std::{net::SocketAddr, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tokio::net::UdpSocket;

const MAGIC: &[u8] = b"USBIP_DISCOVER\n";

#[derive(Debug, Clone)]
pub struct DiscoveryCfg {
    pub bind: SocketAddr,
    pub web_listen: SocketAddr,
}

#[derive(Serialize)]
struct DiscoverResp<'a> {
    server_name: &'a str,
    web_url: String,
    version: &'a str,
}

pub async fn run(cfg: DiscoveryCfg) -> Result<()> {
    let sock = UdpSocket::bind(cfg.bind)
        .await
        .with_context(|| format!("bind udp {}", cfg.bind))?;

    // 发现协议（最小约定）：
    // - 客户端广播发送 MAGIC（纯文本）
    // - 服务端回一包 JSON（UTF-8），告诉客户端“我是谁、Web 在哪、版本号”
    // Small recv timeout-like behavior via select in outer loop.
    let mut buf = [0u8; 1500];
    let version = env!("CARGO_PKG_VERSION");
    let server_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "usbip-server".to_string());

    tracing::info!(udp=%cfg.bind, "udp discovery listening");

    loop {
        let (n, peer) = sock.recv_from(&mut buf).await.context("udp recv_from")?;
        if n != MAGIC.len() || &buf[..n] != MAGIC {
            continue;
        }

        // A 方案：返回“对方看到的源 IP”，而不是简单使用某张网卡地址。
        // 这样在多网卡、多路由场景下，客户端会拿到与自己同网段可达的地址。
        let local_ip = match local_ip_towards(peer).await {
            Ok(ip) => ip,
            Err(err) => {
                tracing::warn!(%peer, %err, "cannot derive local ip; skipping reply");
                continue;
            }
        };

        let web_port = cfg.web_listen.port();
        let resp = DiscoverResp {
            server_name: &server_name,
            web_url: format!("http://{}:{}/", local_ip, web_port),
            version,
        };
        let payload = serde_json::to_vec(&resp).context("serialize discover response")?;
        if let Err(err) = sock.send_to(&payload, peer).await {
            tracing::warn!(%peer, %err, "udp reply failed");
        }
    }
}

async fn local_ip_towards(peer: SocketAddr) -> Result<std::net::IpAddr> {
    // Use a short-lived UDP socket connected to peer to let OS pick route/source ip.
    // Binding to 0.0.0.0:0 (or [::]:0) depending on peer family.
    let bind_addr: SocketAddr = match peer {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    };
    let s = UdpSocket::bind(bind_addr).await.context("bind tmp udp")?;

    // Best-effort: connect and then ask local_addr.
    // Also add a tiny delay budget for strange stacks.
    s.connect(peer).await.context("connect tmp udp")?;
    let la = tokio::time::timeout(Duration::from_millis(200), async { s.local_addr() })
        .await
        .map_err(|_| anyhow!("timeout local_addr"))?
        .context("local_addr")?;
    Ok(la.ip())
}
