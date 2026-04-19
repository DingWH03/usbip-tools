mod style;
mod view;

use anyhow::Result;
use iced::{clipboard, Application, Command, Element, Theme};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use crate::core::{Client, PrivilegeMode};
use crate::{discovery, usbip};

pub fn run_gui(font_name: &'static str) -> Result<()> {
    UsbIpClient::run(iced::Settings {
        default_font: iced::Font::with_name(font_name),
        window: iced::window::Settings {
            size: iced::Size::new(900.0, 600.0),
            ..Default::default()
        },
        ..Default::default()
    })?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) enum Msg {
    Start,
    Refresh,
    Discovered(Result<Vec<discovery::DiscoveredServer>, String>),
    ManualServerChanged(String),
    ManualServerAdd,
    SelectServer(usize),
    RemoteDevices(Result<Vec<usbip::RemoteDevice>, String>),
    ToggleDevice(String, bool),
    AttachSelected,
    Attached(Result<Vec<String>, String>),
    CopyLog,
    DismissPopup,
    CopyPopup,
}

pub(crate) struct UsbIpClient {
    pub(crate) status: String,
    pub(crate) servers: Vec<discovery::DiscoveredServer>,
    pub(crate) selected_server: Option<usize>,
    pub(crate) remote: Vec<usbip::RemoteDevice>,
    pub(crate) selected_busids: HashSet<String>,
    pub(crate) log: Vec<String>,
    pub(crate) manual_server: String,
    pub(crate) popup: Option<String>,
}

impl UsbIpClient {
    fn push_log(&mut self, s: impl Into<String>) {
        self.log.push(s.into());
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    fn log_text_for_copy(&self) -> String {
        // 按时间顺序（旧->新）拼接，方便用户复制粘贴定位问题
        self.log.join("\n")
    }

    fn show_popup(&mut self, title: &str, body: &str) {
        let body = body.trim();
        if body.is_empty() {
            return;
        }
        self.popup = Some(format!("{title}\n\n{body}"));
    }

    fn parse_manual_server(&self) -> Option<SocketAddr> {
        let raw = self.manual_server.trim();
        if raw.is_empty() {
            return None;
        }

        raw.parse::<SocketAddr>().ok().or_else(|| {
            raw.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, 3240))
        })
    }
}

impl Application for UsbIpClient {
    type Executor = iced::executor::Default;
    type Message = Msg;
    type Theme = Theme;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        (
            Self {
                status: "启动中…".to_string(),
                servers: Vec::new(),
                selected_server: None,
                remote: Vec::new(),
                selected_busids: HashSet::new(),
                log: Vec::new(),
                manual_server: String::new(),
                popup: None,
            },
            Command::perform(async { () }, |_| Msg::Start),
        )
    }

    fn title(&self) -> String {
        "usbip-client".to_string()
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Msg::Start => {
                self.status = "扫描局域网 usbip-server…".to_string();
                self.push_log("开始 UDP 广播扫描 (USBIP_DISCOVER)");
                return Command::perform(
                    async {
                        let c = Client::new(PrivilegeMode::GuiPkexec);
                        c.discover(discovery::DiscoveryCfg::default())
                            .await
                            .map_err(|e| format!("{e:#}"))
                    },
                    Msg::Discovered,
                );
            }
            Msg::Refresh => {
                self.status = "重新扫描中…".to_string();
                self.push_log("重新扫描");
                return Command::perform(
                    async {
                        let c = Client::new(PrivilegeMode::GuiPkexec);
                        c.discover(discovery::DiscoveryCfg::default())
                            .await
                            .map_err(|e| format!("{e:#}"))
                    },
                    Msg::Discovered,
                );
            }
            Msg::Discovered(Ok(servers)) => {
                self.servers = servers;
                self.status = format!("发现 {} 个服务端", self.servers.len());

                let lines: Vec<String> = self
                    .servers
                    .iter()
                    .map(|s| {
                        let name = s
                            .info
                            .server_name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        format!("发现: {} ({})", s.addr.ip(), name)
                    })
                    .collect();
                for l in lines {
                    self.push_log(l);
                }

                // Auto-select first server (but do NOT auto-attach).
                if self.selected_server.is_none() && !self.servers.is_empty() {
                    self.push_log("自动选择第一个服务端并拉取设备列表");
                    return Command::perform(async { () }, |_| Msg::SelectServer(0));
                }
            }
            Msg::Discovered(Err(err)) => {
                self.status = "扫描失败".to_string();
                self.push_log(format!("扫描失败: {err}"));
                self.show_popup("扫描失败", &err);
            }
            Msg::ManualServerChanged(v) => {
                self.manual_server = v;
            }
            Msg::ManualServerAdd => {
                let Some(addr) = self.parse_manual_server() else {
                    self.push_log("手动添加失败：地址为空或无法解析");
                    return Command::none();
                };

                if self.servers.iter().any(|s| s.addr.ip() == addr.ip()) {
                    self.push_log(format!("手动添加：已存在 {}", addr.ip()));
                    return Command::none();
                }

                self.push_log(format!("手动添加服务端：{}", addr.ip()));
                self.servers.push(discovery::DiscoveredServer {
                    addr,
                    info: discovery::DiscoverResp {
                        server_name: Some("manual".to_string()),
                        web_url: None,
                        version: None,
                    },
                });
                self.status = format!("服务端列表：{}", self.servers.len());
                if self.selected_server.is_none() {
                    return Command::perform(async { () }, |_| Msg::SelectServer(0));
                }
            }
            Msg::SelectServer(idx) => {
                let Some(s) = self.servers.get(idx).cloned() else {
                    return Command::none();
                };
                self.selected_server = Some(idx);
                self.remote.clear();
                self.selected_busids.clear();
                let host = s.addr.ip().to_string();
                self.status = format!("拉取设备列表：{host}");
                self.push_log(format!("拉取远端设备列表：server={host}"));
                return Command::perform(
                    async move {
                        let c = Client::new(PrivilegeMode::GuiPkexec);
                        c.list_remote_devices(&host)
                            .await
                            .map_err(|e| format!("{e:#}"))
                    },
                    Msg::RemoteDevices,
                );
            }
            Msg::RemoteDevices(Ok(devs)) => {
                self.remote = devs;
                self.status = format!("远端设备：{} 个", self.remote.len());
                if self.remote.is_empty() {
                    self.push_log("远端无可 attach 的设备");
                } else {
                    self.push_log(format!("远端可 attach 设备数量：{}", self.remote.len()));
                }
            }
            Msg::RemoteDevices(Err(err)) => {
                self.status = "拉取设备失败".to_string();
                self.push_log(format!("拉取设备失败: {err}"));
                self.show_popup("拉取设备失败", &err);
            }
            Msg::ToggleDevice(busid, on) => {
                if on {
                    self.selected_busids.insert(busid);
                } else {
                    self.selected_busids.remove(&busid);
                }
            }
            Msg::AttachSelected => {
                let Some(idx) = self.selected_server else {
                    self.push_log("未选择服务端");
                    return Command::none();
                };
                let Some(s) = self.servers.get(idx).cloned() else {
                    self.push_log("未选择服务端");
                    return Command::none();
                };
                if self.selected_busids.is_empty() {
                    self.push_log("未选择任何设备");
                    return Command::none();
                }
                let host = s.addr.ip().to_string();
                let busids: Vec<String> = self.selected_busids.iter().cloned().collect();
                self.status = format!("连接所选设备：{host}");
                self.push_log(format!("开始 attach：server={host}, count={}", busids.len()));
                return Command::perform(
                    async move {
                        let c = Client::new(PrivilegeMode::GuiPkexec);
                        c.attach_many(&host, &busids)
                            .await
                            .map_err(|e| format!("{e:#}"))
                    },
                    Msg::Attached,
                );
            }
            Msg::Attached(Ok(lines)) => {
                self.status = "完成".to_string();
                for l in lines {
                    self.push_log(l);
                }

                // 如果有失败行，弹窗提示（更快定位原因）
                if let Some(first) = self
                    .log
                    .iter()
                    .rev()
                    .take(80)
                    .find(|l| l.contains("attach 失败"))
                    .cloned()
                {
                    self.show_popup("attach 失败", &first);
                }
            }
            Msg::Attached(Err(err)) => {
                self.status = "attach 失败".to_string();
                self.push_log(format!("attach 失败: {err}"));
                self.show_popup("attach 失败", &err);
            }
            Msg::CopyLog => {
                return clipboard::write(self.log_text_for_copy());
            }
            Msg::DismissPopup => {
                self.popup = None;
            }
            Msg::CopyPopup => {
                if let Some(s) = &self.popup {
                    return clipboard::write(s.clone());
                }
            }
        }
        Command::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        view::root(self)
    }
}

