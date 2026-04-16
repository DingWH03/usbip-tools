use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context, Result};
use tokio::process::{Child, Command};

use crate::rules::{DeviceFingerprint, RuleAction, RuleStore, RulesFile};

#[derive(Debug, Clone, Default)]
pub struct PortStatus {
    pub in_use: bool,
    pub remote: Option<String>,
}

#[derive(Clone)]
pub struct UsbIp {
    usbip_bin: String,
    usbipd_bin: String,
    usbipd_child: Arc<Mutex<Option<Child>>>,
    usbip_host_sysfs: PathBuf,
}

impl UsbIp {
    pub fn new(usbip_bin: String, usbipd_bin: String) -> Self {
        Self {
            usbip_bin,
            usbipd_bin,
            usbipd_child: Arc::new(Mutex::new(None)),
            usbip_host_sysfs: PathBuf::from("/sys/bus/usb/drivers/usbip-host"),
        }
    }

    pub async fn ensure_usbipd_running(&self) -> Result<()> {
        if self
            .usbipd_child
            .lock()
            .map_err(|_| anyhow!("usbipd_child poisoned"))?
            .is_some()
        {
            return Ok(());
        }

        let mut cmd = Command::new(&self.usbipd_bin);
        cmd.arg("-D");
        cmd.kill_on_drop(true);
        let child = cmd.spawn().context("spawn usbipd -D")?;
        *self
            .usbipd_child
            .lock()
            .map_err(|_| anyhow!("usbipd_child poisoned"))? = Some(child);
        Ok(())
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceFingerprint>> {
        crate::udev_watch::enumerate_usb_devices().context("enumerate usb devices")
    }

    pub async fn bind(&self, busid: &str) -> Result<()> {
        let out = Command::new(&self.usbip_bin)
            .arg("bind")
            .arg("-b")
            .arg(busid)
            .output()
            .await
            .context("run usbip bind")?;
        if !out.status.success() {
            return Err(anyhow!(
                "usbip bind failed (busid={}): {}",
                busid,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    pub async fn unbind(&self, busid: &str) -> Result<()> {
        let out = Command::new(&self.usbip_bin)
            .arg("unbind")
            .arg("-b")
            .arg(busid)
            .output()
            .await
            .context("run usbip unbind")?;
        if !out.status.success() {
            return Err(anyhow!(
                "usbip unbind failed (busid={}): {}",
                busid,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    /// 获取当前已导出的设备 busid 集合（通过 sysfs 符号链接）
    pub async fn exported_busids(&self) -> Result<HashSet<String>> {
        let mut set = HashSet::new();
        let dir = match fs::read_dir(&self.usbip_host_sysfs) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(set),
            Err(e) => return Err(e).context("read usbip-host sysfs"),
        };

        for entry in dir {
            let entry = entry?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if entry.file_type()?.is_symlink() && name.contains('-') {
                set.insert(name.to_string());
            }
        }
        Ok(set)
    }

    /// 通过 `usbip list -r 127.0.0.1` 获取当前空闲（未被连接）的导出设备 busid
    async fn list_exportable_busids(&self) -> Result<HashSet<String>> {
        let output = Command::new(&self.usbip_bin)
            .args(["list", "-r", "127.0.0.1"])
            .output()
            .await
            .context("run usbip list -r 127.0.0.1")?;

        if !output.status.success() {
            // 命令失败时返回空集，避免影响整体状态
            tracing::warn!("usbip list -r failed: {}", String::from_utf8_lossy(&output.stderr));
            return Ok(HashSet::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_exportable_busids(&stdout))
    }

    /// 获取端口状态：结合 sysfs 导出列表与空闲导出列表，判断 in_use
    pub async fn port_status(&self) -> Result<HashMap<String, PortStatus>> {
        let exported = self.exported_busids().await?;
        let free_set = self.list_exportable_busids().await?;

        let mut map = HashMap::new();
        for busid in exported {
            let in_use = !free_set.contains(&busid);
            // remote 地址无法从服务端轻易获取，暂时留空
            map.insert(busid, PortStatus { in_use, remote: None });
        }
        Ok(map)
    }

    pub async fn apply_rules(&self, rules: &RulesFile, _store: &Arc<RuleStore>) -> Result<()> {
        let devices = self.list_devices().await?;
        for d in devices {
            if let Some(rule) = first_match(rules, &d) {
                if !rule.enabled {
                    continue;
                }
                match rule.action {
                    RuleAction::Bind => {
                        if let Err(err) = self.bind(&d.busid).await {
                            tracing::warn!(busid=%d.busid, %err, "bind failed");
                        } else {
                            tracing::info!(busid=%d.busid, "bound by rule");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// 解析 `usbip list -r 127.0.0.1` 输出，提取所有 busid
///
/// 示例输出格式：
/// ```
/// Exportable USB devices
/// ======================
///  - 127.0.0.1
///       7-1.3: Areson Technology Corp : unknown product (25a7:fa70)
///            : /sys/devices/...
///            : (Defined at Interface level) (00/00/00)
///
///       2-1.6: unknown vendor : unknown product (3346:100c)
///            : /sys/devices/...
/// ```
fn parse_exportable_busids(s: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in s.lines() {
        let trimmed = line.trim_start();
        // 以 "busid:" 开头的行（例如 "7-1.3: ..."）
        if let Some(rest) = trimmed.find(':').map(|idx| &trimmed[..idx]) {
            let candidate = rest.trim();
            // busid 通常形如 X-Y.Z 或 X-Y
            if candidate.contains('-') && !candidate.contains(' ') {
                set.insert(candidate.to_string());
            }
        }
    }
    set
}

// 以下函数已废弃，保留空实现以兼容
fn usbip_status_to_in_use(_status: &str) -> bool {
    false
}

fn parse_busids_from_usbip_list(_s: &str) -> HashSet<String> {
    HashSet::new()
}

fn parse_port_status(_s: &str) -> HashMap<String, PortStatus> {
    HashMap::new()
}

fn first_match<'a>(rules: &'a RulesFile, f: &DeviceFingerprint) -> Option<&'a crate::rules::Rule> {
    rules
        .rules
        .iter()
        .filter(|r| r.enabled)
        .find(|r| r.match_spec.matches(f))
}