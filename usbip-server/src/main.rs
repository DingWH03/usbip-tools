mod discovery_udp;
mod rules;
mod udev_watch;
mod usb;
mod web;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

/// 核心命令行配置结构体
#[derive(Parser, Debug)]
#[command(name = "usbip-server", version, about = "usbip server web manager")]
struct Cli {
    /// Web 管理界面的监听地址
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: SocketAddr,

    /// UDP 自动发现服务的绑定地址
    #[arg(long, default_value = "0.0.0.0:3240")]
    discovery_udp: SocketAddr,

    /// 是否禁用 UDP 自动发现功能
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    discovery_disable: bool,

    /// 状态数据存储目录（用于存放 rules.json 等）
    #[arg(long, default_value = "/var/lib/usbip-server")]
    state_dir: PathBuf,

    /// 显式指定的规则文件路径（若不提供，则在 state_dir 下生成）
    #[arg(long)]
    rules_path: Option<PathBuf>,

    /// 系统 usbip 二进制文件路径
    #[arg(long, default_value = "usbip")]
    usbip_bin: String,

    /// 系统 usbipd 二进制文件路径
    #[arg(long, default_value = "usbipd")]
    usbipd_bin: String,

    /// 是否由本程序负责启动和管理 usbipd 子进程
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    manage_usbipd: bool,
}

/// 初始化日志系统，支持通过环境变量 RUST_LOG 动态调整级别
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,usbip_server=info,tower_http=info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    // 1. 准备路径与目录
    let rules_path = cli
        .rules_path
        .unwrap_or_else(|| cli.state_dir.join("rules.json"));

    // 改进建议：在生产环境下，确保程序有权限创建此目录
    tokio::fs::create_dir_all(&cli.state_dir).await?;

    // 2. 初始化核心逻辑状态
    let rules_store = rules::RuleStore::new(rules_path);
    let rules = rules_store.load_or_default().await?;
    // 使用 Arc<RwLock> 实现多线程安全的读写，Web 接口写，Discovery 读
    let rules = Arc::new(RwLock::new(rules));

    // 事件广播：用于 Web UI 自动更新（SSE）。
    // 任意状态变化（udev、bind/unbind、规则变更、apply）都可以发送一条通知给前端刷新。
    let (events_tx, _events_rx) = broadcast::channel::<String>(64);

    // 包装 usbip 命令行操作封装层
    let usb = usb::UsbIp::new(cli.usbip_bin.clone(), cli.usbipd_bin.clone());

    // 3. 管理 usbipd 子进程（可选）
    if cli.manage_usbipd {
        // 改进建议：此处可以考虑使用 tokio::process 并在 main 结束时优雅杀死子进程
        if let Err(err) = usb.ensure_usbipd_running().await {
            tracing::error!(%err, "failed to start usbipd; continuing (bind may fail)");
        }
    }

    // 4. 启动时同步：根据持久化规则自动绑定当前已插入的设备
    if let Err(err) = usb.apply_rules(&*rules.read().await, &rules_store).await {
        tracing::error!(%err, "startup apply failed");
    }

    // 5. 启动 Udev 监听任务：实现设备热插拔自动绑定
    // 改进建议：持有此任务的 JoinHandle，以便后续监控任务健康状况
    udev_watch::spawn_udev_task(
        usb.clone(),
        rules.clone(),
        rules_store.clone(),
        events_tx.clone(),
    );

    // 6. 启动 UDP 自动发现服务
    if !cli.discovery_disable {
        let cfg = discovery_udp::DiscoveryCfg {
            bind: cli.discovery_udp,
            web_listen: cli.listen,
        };
        tokio::spawn(async move {
            if let Err(err) = discovery_udp::run(cfg).await {
                tracing::error!(%err, "udp discovery task exited");
            }
        });
    }

    // 7. 启动 Web 服务 (主线程阻塞于此)
    let app_state = web::AppState {
        rules: rules.clone(),
        rules_store: rules_store.clone(),
        usb,
        events: events_tx,
    };

    tracing::info!(listen=%cli.listen, "usbip-server web service started");
    
    // 扩展建议：添加信号监听 (Ctrl+C)，实现优雅停机 (Graceful Shutdown)
    // 比如：web::serve_with_graceful_shutdown(cli.listen, app_state, shutdown_signal()).await
    web::serve(cli.listen, app_state).await
}