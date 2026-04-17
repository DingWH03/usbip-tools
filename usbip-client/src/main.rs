mod discovery;
mod core;
mod console;
mod gui;
mod usbip;

use anyhow::Result;

fn init_tracing() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> Result<()> {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ui = args.iter().any(|a| a == "--ui");

    if ui {
        let font_name: &'static str = Box::leak(
            std::env::var("USBIP_CLIENT_FONT")
                .unwrap_or_else(|_| "Noto Sans CJK SC".to_string())
                .into_boxed_str(),
        );
        gui::run_gui(font_name)?;
        return Ok(());
    }

    // 默认 console：支持 REPL + 子命令
    console::run_console(args)?;
    Ok(())
}
