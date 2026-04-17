use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length};

use crate::gui::style;
use crate::gui::{Msg, UsbIpClient};

pub(crate) fn root(app: &UsbIpClient) -> Element<'_, Msg> {
    let popup_view = popup(app);
    let top = topbar(app);

    let left = column![
        container(server_panel(app)).height(Length::FillPortion(5)),
        container(device_panel(app)).height(Length::FillPortion(6)),
    ]
    .spacing(style::GAP)
    .width(Length::Fill);

    let right = container(log_panel(app)).width(Length::Fill).height(Length::Fill);

    let body = row![
        container(left).width(Length::FillPortion(6)),
        container(right).width(Length::FillPortion(5)),
    ]
    .spacing(style::GAP)
    .height(Length::Fill);

    container(column![popup_view, top, body].spacing(style::GAP))
        .padding(style::PAD)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn topbar(app: &UsbIpClient) -> Element<'_, Msg> {
    let title = column![
        text("usbip-client").size(style::TITLE_SIZE),
        text(&app.status).size(style::SECTION_SIZE),
    ]
    .spacing(style::GAP_XS);

    let manual = row![
        text_input("手动添加：IP 或 IP:PORT", &app.manual_server)
            .on_input(Msg::ManualServerChanged)
            .on_submit(Msg::ManualServerAdd)
            .width(Length::FillPortion(4)),
        button("添加").on_press(Msg::ManualServerAdd),
        button("重新扫描").on_press(Msg::Refresh),
    ]
    .spacing(style::GAP_SM)
    .align_items(Alignment::Center)
    .width(Length::FillPortion(6));

    container(
        row![title.width(Length::FillPortion(4)), manual]
            .spacing(style::GAP)
            .align_items(Alignment::Center),
    )
    .padding(style::GAP_SM)
    .width(Length::Fill)
    .into()
}

fn server_panel(app: &UsbIpClient) -> Element<'_, Msg> {
    let header = row![
        text("服务端").size(style::SECTION_SIZE).width(Length::Fill),
        text(format!("{}", app.servers.len()))
            .size(style::SUBTLE_SIZE),
    ]
    .align_items(Alignment::Center);

    let mut list = column![].spacing(style::GAP_XS);
    if app.servers.is_empty() {
        list = list.push(text("未发现服务端。可点“重新扫描”，或手动添加一个地址。").size(14));
    } else {
        for (i, s) in app.servers.iter().enumerate() {
            let name = s
                .info
                .server_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let ver = s.info.version.clone().unwrap_or_else(|| "?".to_string());
            let selected = app.selected_server == Some(i);

            let left = column![
                text(format!("{}", s.addr.ip())).size(16),
                text(format!("{name} · v{ver}")).size(style::SUBTLE_SIZE),
            ]
            .spacing(2)
            .width(Length::Fill);

            let action = button(if selected { "已选择" } else { "选择" }).on_press(Msg::SelectServer(i));

            list = list.push(
                container(
                    row![left, action]
                        .spacing(style::GAP_SM)
                        .align_items(Alignment::Center),
                )
                .padding(style::GAP_SM),
            );
        }
    }

    card("服务端", header, scrollable(list))
}

fn device_panel(app: &UsbIpClient) -> Element<'_, Msg> {
    let selected_count = app.selected_busids.len();
    let header = row![
        text("设备").size(style::SECTION_SIZE).width(Length::Fill),
        text(if selected_count == 0 {
            format!("{} 个", app.remote.len())
        } else {
            format!("{} 个 · 已选 {}", app.remote.len(), selected_count)
        })
        .size(style::SUBTLE_SIZE),
    ]
    .align_items(Alignment::Center);

    let mut list = column![].spacing(style::GAP_XS);
    if app.selected_server.is_none() {
        list = list.push(text("先从上面选择一个服务端，然后这里会显示远端可连接的设备。").size(14));
    } else if app.remote.is_empty() {
        list = list.push(text("没有可连接的远端设备。").size(14));
    } else {
        for d in &app.remote {
            let label = if let Some(vp) = &d.vidpid {
                format!("{}  {}  ({})", d.busid, d.desc, vp)
            } else if d.desc.is_empty() {
                d.busid.clone()
            } else {
                format!("{}  {}", d.busid, d.desc)
            };

            let checked = app.selected_busids.contains(&d.busid);
            let busid = d.busid.clone();
            list = list.push(
                container(
                    row![
                        checkbox("", checked).on_toggle(move |on| {
                            Msg::ToggleDevice(busid.clone(), on)
                        }),
                        text(label).size(14).width(Length::Fill),
                    ]
                    .spacing(style::GAP_SM)
                    .align_items(Alignment::Center),
                )
                .padding(style::GAP_XS),
            );
        }
    }

    let footer = if app.selected_server.is_some() {
        row![
            text("选择设备后点击连接；需要时会弹出 polkit 提权窗口。").size(style::SUBTLE_SIZE).width(Length::Fill),
            button("连接所选设备").on_press(Msg::AttachSelected),
        ]
        .spacing(style::GAP_SM)
        .align_items(Alignment::Center)
    } else {
        row![text("").size(style::SUBTLE_SIZE)].align_items(Alignment::Center)
    };

    let content = column![scrollable(list).height(Length::Fill), footer].spacing(style::GAP_SM);
    card("设备", header, content)
}

fn log_panel(app: &UsbIpClient) -> Element<'_, Msg> {
    let header = row![
        text("日志").size(style::SECTION_SIZE).width(Length::Fill),
        button("复制").on_press(Msg::CopyLog),
    ]
    .spacing(style::GAP_SM)
    .align_items(Alignment::Center);

    let lines = column(
        app.log
            .iter()
            .rev()
            .take(200)
            .map(|l| text(l).size(12).into())
            .collect::<Vec<_>>(),
    )
    .spacing(2);

    let content = scrollable(lines).height(Length::Fill);
    card("日志", header, content)
}

fn popup(app: &UsbIpClient) -> Element<'_, Msg> {
    let Some(popup) = &app.popup else {
        return container(column![]).padding(0).width(Length::Shrink).into();
    };

    let header = row![
        text("提示 / 错误").size(style::SECTION_SIZE).width(Length::Fill),
        button("复制").on_press(Msg::CopyPopup),
        button("关闭").on_press(Msg::DismissPopup),
    ]
    .spacing(style::GAP_SM)
    .align_items(Alignment::Center);

    container(
        column![
            header,
            scrollable(text(popup).size(12)).height(Length::Fixed(style::POPUP_HEIGHT)),
        ]
        .spacing(style::GAP_SM),
    )
    .padding(style::GAP_SM)
    .width(Length::Fill)
    .into()
}

fn card<'a, C: Into<Element<'a, Msg>>>(
    _title: &str,
    header: impl Into<Element<'a, Msg>>,
    content: C,
) -> Element<'a, Msg> {
    container(column![header.into(), content.into()].spacing(style::GAP_SM))
        .padding(style::GAP_SM)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

