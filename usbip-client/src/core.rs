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

    pub async fn ensure_vhci_loaded(&self) -> Result<()> {
        self.usb.ensure_vhci_loaded(self.mode).await
    }

    pub async fn attach(&self, host: &str, busid: &str) -> Result<()> {
        self.usb.attach(self.mode, host, busid).await
    }
}

