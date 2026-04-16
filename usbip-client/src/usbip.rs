use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct UsbIp {
    pub usbip_bin: String,
    pub modprobe_bin: String,
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

async fn run_output_maybe_privileged(bin: &str, args: &[&str]) -> Result<std::process::Output> {
    // Fast path: already root.
    if is_root() {
        return Command::new(bin).args(args).output().await.with_context(|| {
            format!(
                "执行失败: {} {}",
                bin,
                args.iter().copied().collect::<Vec<_>>().join(" ")
            )
        });
    }

    // Linux: prefer polkit (pkexec), fallback to sudo.
    // Note: pkexec typically shows a GUI auth dialog if a polkit agent exists.
    // We only elevate this single command, not the whole GUI process.
    #[cfg(target_os = "linux")]
    {
        let mut pk = Command::new("pkexec");
        if Path::new(bin).is_absolute() {
            pk.arg(bin);
        } else {
            // If not absolute, still try; pkexec will search its PATH (may differ from user PATH).
            pk.arg(bin);
        }
        pk.args(args);
        if let Ok(out) = pk.output().await {
            if out.status.success() {
                return Ok(out);
            }
        }

        let mut su = Command::new("sudo");
        su.arg("-E");
        su.arg(bin);
        su.args(args);
        return su.output().await.with_context(|| {
            format!(
                "执行失败（需要 root；pkexec/sudo 均失败）: {} {}",
                bin,
                args.iter().copied().collect::<Vec<_>>().join(" ")
            )
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bin, args);
        Err(anyhow!("该平台尚未实现按需提权执行（后续支持 Windows 时在这里适配）"))
    }
}

impl Default for UsbIp {
    fn default() -> Self {
        fn pick_bin(candidates: &[&str], fallback: &str) -> String {
            for p in candidates {
                if std::fs::metadata(p).is_ok() {
                    return p.to_string();
                }
            }
            fallback.to_string()
        }
        Self {
            // Many distros install `usbip` under /usr/sbin, which is often missing from a
            // regular user's PATH in GUI sessions.
            usbip_bin: pick_bin(&["/usr/sbin/usbip", "/usr/bin/usbip", "/sbin/usbip"], "usbip"),
            modprobe_bin: pick_bin(
                &["/usr/sbin/modprobe", "/sbin/modprobe", "/usr/bin/modprobe"],
                "modprobe",
            ),
        }
    }
}

impl UsbIp {
    pub async fn ensure_vhci_loaded(&self) -> Result<()> {
        // Best effort; if it fails we still try attach (may fail with a clearer error).
        let out = run_output_maybe_privileged(&self.modprobe_bin, &["vhci-hcd"])
            .await
            .context("modprobe vhci-hcd")?;
        if !out.status.success() {
            tracing::warn!(
                "modprobe vhci-hcd failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    pub async fn list_remote_devices(&self, host: &str) -> Result<Vec<RemoteDevice>> {
        let out = Command::new(&self.usbip_bin)
            .arg("list")
            .arg("-r")
            .arg(host)
            .output()
            .await
            .context("usbip list -r")?;
        if !out.status.success() {
            return Err(anyhow!(
                "usbip list -r failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(parse_devices_from_usbip_list_remote(&s))
    }

    pub async fn attach(&self, host: &str, busid: &str) -> Result<()> {
        let out = run_output_maybe_privileged(
            &self.usbip_bin,
            &["attach", "-r", host, "-b", busid],
        )
        .await
        .context("usbip attach")?;
        if !out.status.success() {
            return Err(anyhow!(
                "usbip attach failed (host={}, busid={}): {}",
                host,
                busid,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RemoteDevice {
    pub busid: String,
    pub desc: String,
    pub vidpid: Option<String>,
    pub sys_path: Option<String>,
}

fn parse_devices_from_usbip_list_remote(s: &str) -> Vec<RemoteDevice> {
    // Observed outputs:
    // - "busid 1-1.2"
    // - "- 1-1.2: ..."
    // - "      2-1.6: ..." (indented busid line under a host header)
    let mut seen = HashSet::new();
    let mut out: Vec<RemoteDevice> = Vec::new();
    let mut current: Option<RemoteDevice> = None;
    for line in s.lines() {
        let l = line.trim_end();
        let lt = l.trim();

        // If we hit a new device header, flush previous.
        if let Some(d) = parse_device_header_line(lt) {
            if let Some(prev) = current.take() {
                if seen.insert(prev.busid.clone()) {
                    out.push(prev);
                }
            }
            current = Some(d);
            continue;
        }

        // Capture sysfs path line: ": /sys/devices/...."
        if let Some(cur) = current.as_mut() {
            if let Some(rest) = lt.strip_prefix(": ") {
                if rest.starts_with("/sys/") {
                    cur.sys_path = Some(rest.to_string());
                }
            }
        }
    }

    if let Some(prev) = current.take() {
        if seen.insert(prev.busid.clone()) {
            out.push(prev);
        }
    }
    out
}

fn parse_device_header_line(l: &str) -> Option<RemoteDevice> {
    // Examples:
    // - "2-1.6: unknown vendor : unknown product (3346:100c)"
    // - "- 1-1.2: ..."
    // - "busid 1-1.2"

    if let Some(rest) = l.strip_prefix("busid ") {
        let busid = rest.split_whitespace().next().unwrap_or("");
        if looks_like_busid(busid) {
            return Some(RemoteDevice {
                busid: busid.to_string(),
                desc: String::new(),
                vidpid: None,
                sys_path: None,
            });
        }
        return None;
    }

    let l2 = l.strip_prefix("- ").unwrap_or(l);
    let (maybe_busid, rest) = l2.split_once(':')?;
    let busid = maybe_busid.trim();
    if !looks_like_busid(busid) {
        return None;
    }
    let rest = rest.trim();

    // Try to extract "(vvvv:pppp)" suffix.
    let mut desc = rest.to_string();
    let mut vidpid = None;
    if let Some(start) = rest.rfind('(') {
        if rest.ends_with(')') && start + 1 < rest.len() {
            let inside = &rest[start + 1..rest.len() - 1];
            if inside.len() == 9 && inside.chars().nth(4) == Some(':') {
                let (v, p) = inside.split_once(':')?;
                if v.chars().all(|c| c.is_ascii_hexdigit())
                    && p.chars().all(|c| c.is_ascii_hexdigit())
                {
                    vidpid = Some(inside.to_lowercase());
                    desc = rest[..start].trim().to_string();
                }
            }
        }
    }

    Some(RemoteDevice {
        busid: busid.to_string(),
        desc,
        vidpid,
        sys_path: None,
    })
}

fn looks_like_busid(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}
