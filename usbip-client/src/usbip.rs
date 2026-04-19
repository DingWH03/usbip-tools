use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tokio::process::Command;

use crate::core::PrivilegeMode;

/// pkexec 失败时根据 stderr 追加的 GUI 提示（与单条 attach 一致）
fn pkexec_stderr_hint(stderr: &str) -> &'static str {
    let low = stderr.to_lowercase();
    if low.contains("no authentication agent found")
        || low.contains("no session")
        || low.contains("agent")
    {
        return "（提示：看起来缺少/未启动 polkit 认证代理。KDE 可安装并启动 polkit agent，例如包 `polkit-kde-agent-1`。）";
    }
    if low.contains("cannot open display") || low.contains("display") {
        return "（提示：pkexec 无法连接到图形会话 DISPLAY/WAYLAND。请从桌面会话启动，或检查环境变量/桌面文件 Exec。）";
    }
    ""
}

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

async fn run_output_with_privilege_os(
    mode: PrivilegeMode,
    program: &str,
    args: &[String],
) -> Result<std::process::Output> {
    let cmdline = std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    // Fast path: already root.
    if is_root() {
        return Command::new(program)
            .args(args)
            .output()
            .await
            .with_context(|| format!("执行失败: {cmdline}"));
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
                if Path::new(program).is_absolute() {
                    pk.arg(program);
                } else {
                    pk.arg(program);
                }
                pk.args(args);
                let out = pk.output().await.with_context(|| {
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
                su.arg(program);
                su.args(args);
                let out = su.output().await.with_context(|| {
                    format!("执行失败（sudo）: {cmdline}")
                })?;
                return Ok(out);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (program, args);
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

    /// 单次提权内依次 modprobe（若 sysfs 显示未加载）并对每个 busid 执行 `usbip attach`。
    /// 返回与原先循环 attach 相同风格的日志行（`attach 成功/失败: host busid`）。
    pub async fn attach_many(
        &self,
        mode: PrivilegeMode,
        host: &str,
        busids: &[String],
    ) -> Result<Vec<String>> {
        // 无设备且 vhci 已就绪：无需提权。
        if busids.is_empty() && is_vhci_loaded() {
            return Ok(Vec::new());
        }

        const BATCH_SCRIPT: &str = r#"modprobe="$1"
usbip="$2"
host="$3"
shift 3
if [ ! -d /sys/module/vhci_hcd ] && [ ! -d /sys/module/vhci-hcd ] && [ ! -d /sys/bus/platform/drivers/vhci_hcd ]; then
  "$modprobe" vhci-hcd 2>/dev/null || true
fi
for b in "$@"; do
  if "$usbip" attach -r "$host" -b "$b"; then
    echo "USBIP_BATCH_OK $b"
  else
    echo "USBIP_BATCH_FAIL $b"
  fi
done"#;

        let mut argv: Vec<String> = vec![
            "-c".to_string(),
            BATCH_SCRIPT.to_string(),
            "_".to_string(),
            self.modprobe_bin.clone(),
            self.usbip_bin.clone(),
            host.to_string(),
        ];
        argv.extend(busids.iter().cloned());

        let out = run_output_with_privilege_os(mode, "/bin/sh", &argv)
            .await
            .context("usbip attach (batch)")?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if mode == PrivilegeMode::GuiPkexec {
                let hint = pkexec_stderr_hint(&stderr);
                return Err(anyhow!(
                    "usbip attach batch failed (GUI/pkexec) (host={}): {}{}",
                    host,
                    stderr.trim(),
                    hint
                ));
            }
            return Err(anyhow!(
                "usbip attach batch failed (console/sudo) (host={}): {}",
                host,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut seen: HashMap<String, bool> = HashMap::new();
        for line in stdout.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("USBIP_BATCH_OK ") {
                seen.insert(rest.trim().to_string(), true);
            } else if let Some(rest) = t.strip_prefix("USBIP_BATCH_FAIL ") {
                seen.insert(rest.trim().to_string(), false);
            }
        }

        let stderr_tail = String::from_utf8_lossy(&out.stderr);
        let stderr_hint = if stderr_tail.trim().is_empty() {
            String::new()
        } else {
            format!(" ({})", stderr_tail.trim())
        };

        let mut lines = Vec::with_capacity(busids.len());
        for b in busids {
            match seen.get(b) {
                Some(true) => lines.push(format!("attach 成功: {host} {b}")),
                Some(false) => lines.push(format!(
                    "attach 失败: {host} {b}: usbip attach 返回非零{stderr_hint}"
                )),
                None => lines.push(format!(
                    "attach 失败: {host} {b}: 批处理未返回状态（stdout 中缺少 USBIP_BATCH_* 行）{stderr_hint}"
                )),
            }
        }
        Ok(lines)
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
