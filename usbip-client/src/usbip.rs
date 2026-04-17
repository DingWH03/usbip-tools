use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tokio::process::Command;

use crate::core::PrivilegeMode;

#[derive(Debug, Clone)]
pub struct UsbIp {
    pub usbip_bin: String,
    pub modprobe_bin: String,
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn is_env_key_allowed(k: &str) -> bool {
    matches!(
        k,
        "DISPLAY"
            | "WAYLAND_DISPLAY"
            | "XDG_RUNTIME_DIR"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "XAUTHORITY"
            | "LANG"
            | "LC_ALL"
            | "LC_MESSAGES"
    )
}

fn apply_desktop_session_env(cmd: &mut Command) {
    for (k, v) in std::env::vars() {
        if is_env_key_allowed(&k) {
            cmd.env(k, v);
        }
    }
}

fn pick_pkexec_bin() -> Option<&'static str> {
    // 尽量用绝对路径，避免 KDE/desktop 启动时 PATH 不完整导致找不到 pkexec
    for p in ["/usr/bin/pkexec", "/bin/pkexec", "/usr/local/bin/pkexec"] {
        if std::fs::metadata(p).is_ok() {
            return Some(p);
        }
    }
    None
}

async fn run_output_with_privilege(
    mode: PrivilegeMode,
    bin: &str,
    args: &[&str],
) -> Result<std::process::Output> {
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
        match mode {
            PrivilegeMode::GuiPkexec => {
                let pkexec_bin = pick_pkexec_bin().unwrap_or("pkexec");
                let mut pk = Command::new(pkexec_bin);
                apply_desktop_session_env(&mut pk);
                if Path::new(bin).is_absolute() {
                    pk.arg(bin);
                } else {
                    // If not absolute, still try; pkexec will search its PATH (may differ from user PATH).
                    pk.arg(bin);
                }
                pk.args(args);
                let out = pk.output().await.with_context(|| {
                    let cmdline = format!(
                        "{} {}",
                        bin,
                        args.iter().copied().collect::<Vec<_>>().join(" ")
                    );
                    let mut hint = String::new();
                    if pick_pkexec_bin().is_none() {
                        hint.push_str(
                            "（提示：未找到 pkexec。请安装 polkit/pkexec，例如 Debian/Ubuntu: `sudo apt-get install -y policykit-1`）",
                        );
                    }
                    format!("执行失败（pkexec）: {cmdline}{hint}")
                })?;
                return Ok(out);
            }
            PrivilegeMode::ConsoleSudo => {
                let mut su = Command::new("sudo");
                // Do NOT use -E here by default; keep it minimal and let sudo prompt in TTY.
                su.arg(bin);
                su.args(args);
                let out = su.output().await.with_context(|| {
                    format!(
                        "执行失败（sudo）: {} {}",
                        bin,
                        args.iter().copied().collect::<Vec<_>>().join(" ")
                    )
                })?;
                return Ok(out);
            }
        }
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
    pub async fn ensure_vhci_loaded(&self, mode: PrivilegeMode) -> Result<()> {
        // 如果模块已经加载，不要每次都重复 modprobe（也避免在某些环境下 modprobe 不存在时报错）。
        if is_vhci_loaded() {
            return Ok(());
        }

        // Best effort; if it fails we still try attach (may fail with a clearer error).
        let out = run_output_with_privilege(mode, &self.modprobe_bin, &["vhci-hcd"])
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

    pub async fn attach(&self, mode: PrivilegeMode, host: &str, busid: &str) -> Result<()> {
        let out =
            run_output_with_privilege(mode, &self.usbip_bin, &["attach", "-r", host, "-b", busid])
                .await
                .context("usbip attach")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if mode == PrivilegeMode::GuiPkexec {
                let mut hint = String::new();
                let low = stderr.to_lowercase();
                if low.contains("no authentication agent found")
                    || low.contains("no session")
                    || low.contains("agent")
                {
                    hint.push_str("（提示：看起来缺少/未启动 polkit 认证代理。KDE 可安装并启动 polkit agent，例如包 `polkit-kde-agent-1`。）");
                } else if low.contains("cannot open display") || low.contains("display") {
                    hint.push_str("（提示：pkexec 无法连接到图形会话 DISPLAY/WAYLAND。请从桌面会话启动，或检查环境变量/桌面文件 Exec。）");
                }
                return Err(anyhow!(
                    "usbip attach failed (GUI/pkexec) (host={}, busid={}): {}{}",
                    host,
                    busid,
                    stderr.trim(),
                    hint
                ));
            }
            return Err(anyhow!(
                "usbip attach failed (console/sudo) (host={}, busid={}): {}",
                host,
                busid,
                stderr.trim()
            ));
        }
        Ok(())
    }
}

fn is_vhci_loaded() -> bool {
    // sysfs 中模块名通常用下划线：vhci_hcd
    std::fs::metadata("/sys/module/vhci_hcd").is_ok()
        || std::fs::metadata("/sys/module/vhci-hcd").is_ok()
        || std::fs::metadata("/sys/bus/platform/drivers/vhci_hcd").is_ok()
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
