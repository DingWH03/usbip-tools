use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

/// 规则系统的目标：用“尽量稳定的设备标识”描述“需要自动导出(bind)”的意图，
/// 并将该意图持久化到本地（默认 `/var/lib/usbip-server/rules.json`）。
///
/// 设计取舍：
/// - 我们避免使用 `busnum/devnum` 这种每次插拔都会变化的字段作为主键；
/// - 以 serial / devpath / vid-pid 作为匹配条件，允许渐进退化；
/// - “手动绑定/解绑”的行为会同步写入/禁用规则，从而做到重启和热插拔自动恢复。

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Bind,
}

/// 对设备的“匹配描述”。
///
/// 匹配逻辑是“所有非空字段都要相等”：
/// - serial 最稳定（但并非所有设备都有）
/// - DEVPATH 是内核路径（更像“插在哪个口”），换口会变但同口通常稳定
/// - VID/PID 是设备型号级别，可能匹配到多个同款设备
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchSpec {
    pub serial: Option<String>,
    pub devpath: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

impl MatchSpec {
    pub fn matches(&self, f: &DeviceFingerprint) -> bool {
        if let Some(serial) = &self.serial {
            if f.serial.as_deref() != Some(serial.as_str()) {
                return false;
            }
        }
        if let Some(devpath) = &self.devpath {
            if f.devpath.as_deref() != Some(devpath.as_str()) {
                return false;
            }
        }
        if let Some(vid) = self.vid {
            if f.vid != Some(vid) {
                return false;
            }
        }
        if let Some(pid) = self.pid {
            if f.pid != Some(pid) {
                return false;
            }
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.serial.is_none() && self.devpath.is_none() && self.vid.is_none() && self.pid.is_none()
    }
}

impl PartialEq for MatchSpec {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
            && self.devpath == other.devpath
            && self.vid == other.vid
            && self.pid == other.pid
    }
}

impl Eq for MatchSpec {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: Uuid,
    pub enabled: bool,
    pub note: Option<String>,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    pub action: RuleAction,
}

impl Rule {
    pub fn new(match_spec: MatchSpec, note: Option<String>) -> Result<Self> {
        if match_spec.is_empty() {
            return Err(anyhow!("match spec must not be empty"));
        }
        Ok(Self {
            id: Uuid::new_v4(),
            enabled: true,
            note,
            match_spec,
            action: RuleAction::Bind,
        })
    }
}

/// 规则文件的整体结构。
///
/// 当前策略是“线性列表 + 第一条命中的规则生效”（见 `usb::first_match`），
/// 便于用户在 JSON 中手工调整优先级。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RulesFile {
    pub rules: Vec<Rule>,
}

impl RulesFile {
    /// Find an existing rule by match spec (regardless of enabled).
    pub fn find_rule_mut(&mut self, match_spec: &MatchSpec) -> Option<&mut Rule> {
        self.rules.iter_mut().find(|r| &r.match_spec == match_spec)
    }

    /// Upsert a Bind rule. If it already exists, updates enabled/note.
    ///
    /// 这个函数主要服务于“UI 手动 bind/unbind → 规则持久化”的闭环：
    /// - 已存在：更新 enabled / note（note 仅在传入 Some 时覆盖）
    /// - 不存在：创建新规则
    pub fn upsert_bind_rule(
        &mut self,
        match_spec: MatchSpec,
        enabled: bool,
        note: Option<String>,
    ) -> Result<()> {
        if match_spec.is_empty() {
            return Err(anyhow!("match spec must not be empty"));
        }
        if let Some(r) = self.find_rule_mut(&match_spec) {
            r.enabled = enabled;
            if note.is_some() {
                r.note = note;
            }
            r.action = RuleAction::Bind;
            return Ok(());
        }
        self.rules.push(Rule {
            id: Uuid::new_v4(),
            enabled,
            note,
            match_spec,
            action: RuleAction::Bind,
        });
        Ok(())
    }

    /// Disable any rule that matches the given match spec.
    ///
    /// 注意：禁用规则并不会自动执行 unbind；unbind 由调用方显式触发。
    pub fn disable_rule(&mut self, match_spec: &MatchSpec) -> bool {
        if let Some(r) = self.find_rule_mut(match_spec) {
            let changed = r.enabled;
            r.enabled = false;
            return changed;
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFingerprint {
    pub busid: String,
    pub serial: Option<String>,
    pub devpath: Option<String>,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    /// USB 设备类（bDeviceClass），用于过滤 hub 等不可绑定设备。
    /// 典型：Hub = 0x09。
    pub device_class: Option<u8>,
}

#[derive(Clone)]
pub struct RuleStore {
    path: PathBuf,
}

impl RuleStore {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load_or_default(&self) -> Result<RulesFile> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let rules: RulesFile =
                    serde_json::from_slice(&bytes).context("parse rules json")?;
                Ok(rules)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(RulesFile::default()),
            Err(err) => Err(err).context("read rules file"),
        }
    }

    pub async fn store(&self, rules: &RulesFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 原子写：先写临时文件再 rename，避免系统异常导致规则文件半写入损坏。
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(rules).context("serialize rules json")?;
        tokio::fs::write(&tmp, &bytes)
            .await
            .context("write tmp rules")?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .context("rename tmp rules")?;
        Ok(())
    }
}

pub type SharedRules = Arc<RwLock<RulesFile>>;

pub fn best_effort_rule_for_device(f: &DeviceFingerprint) -> Option<MatchSpec> {
    // 规则生成策略：尽可能使用稳定字段。
    // - 优先 serial（最不依赖“插口”）
    // - 其次 devpath（同口复现性较好）
    // - 再退化到 vid/pid（只能做到“同型号”）
    if let Some(serial) = &f.serial {
        return Some(MatchSpec {
            serial: Some(serial.clone()),
            devpath: None,
            vid: f.vid,
            pid: f.pid,
        });
    }
    if let Some(devpath) = &f.devpath {
        return Some(MatchSpec {
            serial: None,
            devpath: Some(devpath.clone()),
            vid: f.vid,
            pid: f.pid,
        });
    }
    if f.vid.is_some() && f.pid.is_some() {
        return Some(MatchSpec {
            serial: None,
            devpath: None,
            vid: f.vid,
            pid: f.pid,
        });
    }
    None
}
