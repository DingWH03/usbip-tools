use std::{
    collections::HashMap,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tokio::sync::{broadcast, mpsc};
use tokio::time::sleep;

use crate::{
    rules::{DeviceFingerprint, RuleStore, RulesFile},
    usb::UsbIp,
};

pub fn enumerate_usb_devices() -> Result<Vec<DeviceFingerprint>> {
    let mut e = udev::Enumerator::new().context("udev enumerator")?;
    e.match_subsystem("usb").context("match_subsystem")?;
    e.match_property("DEVTYPE", "usb_device")
        .context("match_property DEVTYPE")?;

    let mut out = Vec::new();
    for d in e.scan_devices().context("scan_devices")? {
        let busid = d.sysname().to_string_lossy().to_string();

        let vid = d
            .attribute_value("idVendor")
            .and_then(|s| s.to_str())
            .and_then(|x| u16::from_str_radix(x, 16).ok());
        let pid = d
            .attribute_value("idProduct")
            .and_then(|s| s.to_str())
            .and_then(|x| u16::from_str_radix(x, 16).ok());

        let serial = d
            .attribute_value("serial")
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        let product = d
            .attribute_value("product")
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        let manufacturer = d
            .attribute_value("manufacturer")
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        let device_class = d
            .attribute_value("bDeviceClass")
            .and_then(|s| s.to_str())
            .and_then(|x| u8::from_str_radix(x, 16).ok());

        let devpath = d
            .property_value("DEVPATH")
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        out.push(DeviceFingerprint {
            busid,
            serial,
            devpath,
            vid,
            pid,
            manufacturer,
            product,
            device_class,
        });
    }
    Ok(out)
}

pub fn spawn_udev_task(
    usb: UsbIp,
    rules: crate::rules::SharedRules,
    store: std::sync::Arc<RuleStore>,
    events: broadcast::Sender<String>,
) {
    tokio::spawn(async move {
        tracing::info!("starting udev monitor for USB hotplug");

        // monitor socket 是非 Send 的（内部持有原生指针），不能跨 .await。
        // 这里用阻塞线程读取事件，再转发给 async 任务处理。
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, String)>();
        tokio::task::spawn_blocking(move || {
            // udev 事件监听应当用 monitor，而不是对 /sys 做 inotify（sysfs 往往不会产生预期事件）。
            let monitor = match udev::MonitorBuilder::new() {
                Ok(b) => match b.match_subsystem("usb") {
                    Ok(b) => match b.listen() {
                        Ok(m) => m,
                        Err(_) => return,
                    },
                    Err(_) => return,
                },
                Err(_) => return,
            };

            let fd = monitor.as_raw_fd();
            loop {
                // 先尝试“非阻塞取一次事件”
                if let Some(ev) = monitor.iter().next() {
                    let action = ev.event_type().to_string();
                    let busid = ev.sysname().to_string_lossy().to_string();
                    if busid.contains('-') {
                        let _ = tx.send((busid, action));
                    }
                    continue;
                }

                // 没事件就阻塞等 fd 可读，避免空转。
                // SAFETY: poll 仅借用 fd 值，不会越界访问。
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe {
                    let _ = libc::poll(&mut pfd as *mut libc::pollfd, 1, -1);
                }
            }
        });

        let mut last_event: HashMap<String, Instant> = HashMap::new();
        let debounce = Duration::from_millis(600);

        while let Some((busid, action)) = rx.recv().await {
            // udev 对 USB 子系统会发很多“接口级”事件，sysname 形如 "7-1.3:1.0"。
            // 规则和 usbip bind 关注的是设备级 busid（不带 ':'）。这里直接过滤掉，避免日志与重试风暴。
            if busid.contains(':') {
                continue;
            }

            // 只在“设备出现/变化”时尝试应用规则。
            // remove/unbind/bind 这类事件不可能产生“自动导出”的正向效果，只会造成重试与噪声日志。
            if action != "add" && action != "change" {
                tracing::debug!(busid=%busid, %action, "ignoring udev action");
                continue;
            }

            // 这条日志用 INFO 级别：默认 systemd 日志级别下也能看到，
            // 便于确认“热插拔事件确实触发了 watcher”。
            tracing::info!(busid=%busid, %action, "usb hotplug detected");

            let now = Instant::now();
            if let Some(t) = last_event.get(&busid) {
                if now.duration_since(*t) < debounce {
                    continue;
                }
            }
            last_event.insert(busid.clone(), now);

            tracing::debug!(busid=%busid, "udev triggered, applying rules");

            sleep(Duration::from_millis(200)).await;

            let rules_guard = rules.read().await;
            if let Err(err) = apply_for_one_with_retry(&usb, &rules_guard, &busid).await {
                tracing::warn!(busid=%busid, %err, "hotplug apply failed");
            } else {
                let _ = events.send("udev".to_string());
            }
            drop(rules_guard);
            let _ = store.path();
        }
    });
}

async fn apply_for_one_with_retry(usb: &UsbIp, rules: &RulesFile, busid: &str) -> Result<()> {
    // 热插拔场景下，udev 属性（尤其 serial/devpath）可能在目录出现后的一小段时间内仍为空，
    // 这会导致规则匹配暂时失败（Ok(false)）。因此这里对 “没匹配/没找到” 也做有限重试。
    let max_retries = 20;
    let retry_delay = Duration::from_millis(250);

    for attempt in 0..max_retries {
        match try_apply_for_one(usb, rules, busid).await {
            Ok(true) => return Ok(()),
            Ok(false) if attempt == max_retries - 1 => {
                tracing::info!(%busid, "hotplug processed but no rule matched (no auto-bind)");
                return Ok(());
            }
            Ok(false) => {
                sleep(retry_delay).await;
            }
            Err(e) if attempt == max_retries - 1 => return Err(e),
            Err(_) => {
                // 降噪：只在较低频率上记录重试（否则 systemd 日志会被刷屏）。
                if attempt == 0 || attempt % 5 == 0 {
                    tracing::debug!(%busid, attempt, "retry apply due to missing attributes");
                }
                sleep(retry_delay).await;
            }
        }
    }
    Ok(())
}

async fn try_apply_for_one(usb: &UsbIp, rules: &RulesFile, busid: &str) -> Result<bool> {
    let devices = usb.list_devices().await?;
    let Some(dev) = devices.into_iter().find(|d| d.busid == busid) else {
        // 设备目录先出现、udev 还没枚举到是常见情况：触发上层重试。
        anyhow::bail!("device not yet enumerated");
    };

    let exported = usb.exported_busids().await?;
    if exported.contains(busid) {
        // 已经导出时不需要重试，否则会造成大量重复日志。
        tracing::debug!(%busid, "device already exported, skip bind");
        return Ok(true);
    }

    if let Some(rule) = rules
        .rules
        .iter()
        .find(|r| r.enabled && r.match_spec.matches(&dev))
    {
        match rule.action {
            crate::rules::RuleAction::Bind => {
                usb.bind(busid).await?;
                tracing::info!(busid=%busid, "auto-bound by hotplug rule");
                return Ok(true);
            }
        }
    }
    Ok(false)
}