use anyhow::Result;

use crate::{discovery, usbip};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegeMode {
    /// GUI 模式：使用 pkexec 触发 polkit 弹窗提权。
    GuiPkexec,
    /// Console 模式：使用 sudo 在终端中提示输入密码。
    ConsoleSudo,
}

#[derive(Debug, Clone)]
pub struct Client {
    usb: usbip::UsbIp,
    mode: PrivilegeMode,
}

impl Client {
    pub fn new(mode: PrivilegeMode) -> Self {
        Self {
            usb: usbip::UsbIp::default(),
            mode,
        }
    }

    pub async fn discover(
        &self,
        cfg: discovery::DiscoveryCfg,
    ) -> Result<Vec<discovery::DiscoveredServer>> {
        discovery::discover(cfg).await
    }

    pub async fn list_remote_devices(&self, host: &str) -> Result<Vec<usbip::RemoteDevice>> {
        self.usb.list_remote_devices(host).await
    }

    /// 单次提权批量 attach（GUI/控制台均少弹密码）。
    pub async fn attach_many(&self, host: &str, busids: &[String]) -> Result<Vec<String>> {
        self.usb.attach_many(self.mode, host, busids).await
    }
}

