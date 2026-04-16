mod discovery;
mod usbip;

use anyhow::Result;
use iced::{
    Application,
    widget::{button, checkbox, column, container, row, scrollable, text, text_input},
    Alignment, Command, Element, Length, Theme,
};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

fn init_tracing() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> Result<()> {
    init_tracing();
    let font_name: &'static str = Box::leak(
        std::env::var("USBIP_CLIENT_FONT")
            .unwrap_or_else(|_| "Noto Sans CJK SC".to_string())
            .into_boxed_str(),
    );
    UsbIpClient::run(iced::Settings {
        default_font: iced::Font::with_name(font_name),
        window: iced::window::Settings {
            size: iced::Size::new(860.0, 540.0),
            ..Default::default()
        },
        ..Default::default()
    })?;
    Ok(())
}

#[derive(Debug, Clone)]
enum Msg {
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
}

struct UsbIpClient {
    status: String,
    servers: Vec<discovery::DiscoveredServer>,
    selected_server: Option<usize>,
    remote: Vec<usbip::RemoteDevice>,
    selected_busids: HashSet<String>,
    log: Vec<String>,
    manual_server: String,
}

impl UsbIpClient {
    fn push_log(&mut self, s: impl Into<String>) {
        self.log.push(s.into());
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }
}

impl iced::Application for UsbIpClient {
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
            },
            Command::perform(async { () }, |_| Msg::Start),
        )
    }

    fn title(&self) -> String {
        "usbip-client (iced)".to_string()
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Msg::Start => {
                self.status = "扫描局域网 usbip-server…".to_string();
                self.push_log("开始 UDP 广播扫描 (USBIP_DISCOVER)");
                return Command::perform(
                    async {
                        discovery::discover(discovery::DiscoveryCfg::default())
                            .await
                            .map_err(|e| e.to_string())
                    },
                    Msg::Discovered,
                );
            }
            Msg::Refresh => {
                self.status = "重新扫描中…".to_string();
                self.push_log("重新扫描");
                return Command::perform(
                    async {
                        discovery::discover(discovery::DiscoveryCfg::default())
                            .await
                            .map_err(|e| e.to_string())
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
                        format!(
                        "发现: {} ({})",
                        s.addr.ip(),
                        s.info
                            .server_name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string())
                        )
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
            }
            Msg::ManualServerChanged(v) => {
                self.manual_server = v;
            }
            Msg::ManualServerAdd => {
                let raw = self.manual_server.trim();
                if raw.is_empty() {
                    self.push_log("手动添加失败：为空");
                    return Command::none();
                }

                let addr: Option<SocketAddr> = raw.parse::<SocketAddr>().ok().or_else(|| {
                    raw.parse::<IpAddr>()
                        .ok()
                        .map(|ip| SocketAddr::new(ip, 3240))
                });

                let Some(addr) = addr else {
                    self.push_log(format!("手动添加失败：无法解析地址: {raw}"));
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
                        let usb = usbip::UsbIp::default();
                        usb.list_remote_devices(&host)
                            .await
                            .map_err(|e| e.to_string())
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
                        let usb = usbip::UsbIp::default();
                        usb.ensure_vhci_loaded().await.map_err(|e| e.to_string())?;
                        let mut ok = Vec::new();
                        for b in busids {
                            match usb.attach(&host, &b).await {
                                Ok(_) => ok.push(format!("attach 成功: {host} {b}")),
                                Err(e) => ok.push(format!("attach 失败: {host} {b}: {e}")),
                            }
                        }
                        Ok(ok)
                    },
                    Msg::Attached,
                );
            }
            Msg::Attached(Ok(lines)) => {
                self.status = "完成".to_string();
                for l in lines {
                    self.push_log(l);
                }
            }
            Msg::Attached(Err(err)) => {
                self.status = "attach 失败".to_string();
                self.push_log(format!("attach 失败: {err}"));
            }
        }
        Command::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let header = row![
            text("usbip-client").size(28),
            text(&self.status).size(16),
            text_input("手动添加：IP 或 IP:PORT", &self.manual_server)
                .on_input(Msg::ManualServerChanged)
                .on_submit(Msg::ManualServerAdd)
                .width(Length::FillPortion(2)),
            button("添加").on_press(Msg::ManualServerAdd),
            button("重新扫描").on_press(Msg::Refresh),
        ]
        .spacing(16)
        .align_items(Alignment::Center);

        let mut servers_col = column![text("发现的服务端").size(18)].spacing(8);
        if self.servers.is_empty() {
            servers_col = servers_col.push(text("(无)").size(14));
        } else {
            for (i, s) in self.servers.iter().enumerate() {
                let name = s
                    .info
                    .server_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let ver = s.info.version.clone().unwrap_or_else(|| "?".to_string());
                let web = s.info.web_url.clone().unwrap_or_default();
                let selected = self.selected_server == Some(i);
                let line = container(
                    row![
                        column![
                            text(format!("{} ({})", s.addr.ip(), name)).size(16),
                            text(format!("version={ver}")).size(12),
                            text(format!("web={web}")).size(12),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        button(if selected { "已选择" } else { "选择" }).on_press(Msg::SelectServer(i)),
                    ]
                    .spacing(12)
                    .align_items(Alignment::Center),
                )
                .padding(8);
                servers_col = servers_col.push(line);
            }
        }

        let mut dev_col = column![text("远端设备").size(18)].spacing(8);
        if self.selected_server.is_none() {
            dev_col = dev_col.push(text("(先选择一个服务端)").size(14));
        } else if self.remote.is_empty() {
            dev_col = dev_col.push(text("(无)").size(14));
        } else {
            for d in &self.remote {
                let label = if let Some(vp) = &d.vidpid {
                    format!("{}  {}  ({})", d.busid, d.desc, vp)
                } else if d.desc.is_empty() {
                    d.busid.clone()
                } else {
                    format!("{}  {}", d.busid, d.desc)
                };
                let checked = self.selected_busids.contains(&d.busid);
                dev_col = dev_col.push(
                    container(
                        row![
                            checkbox("", checked).on_toggle({
                                let busid = d.busid.clone();
                                move |on| Msg::ToggleDevice(busid.clone(), on)
                            }),
                            text(label).size(14).width(Length::Fill),
                        ]
                        .spacing(8)
                        .align_items(Alignment::Center),
                    )
                    .padding(6),
                );
            }
            dev_col = dev_col.push(button("连接所选设备").on_press(Msg::AttachSelected));
        }

        let log_view = scrollable(
            column![text("日志").size(18)]
                .push(
                    column(
                        self.log
                            .iter()
                            .rev()
                            .take(120)
                            .map(|l| text(l).size(12).into())
                            .collect::<Vec<_>>(),
                    )
                    .spacing(2),
                )
                .spacing(8),
        );

        container(
            column![
                header,
                row![
                    column![servers_col, dev_col].spacing(16).width(Length::FillPortion(3)),
                    log_view.width(Length::FillPortion(3))
                ]
                .spacing(16)
                .height(Length::Fill),
            ]
            .spacing(16),
        )
        .padding(16)
        .into()
    }
}
