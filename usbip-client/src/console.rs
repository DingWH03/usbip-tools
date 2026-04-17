use anyhow::{anyhow, Result};
use std::io::{self, Write};

use crate::core::{Client, PrivilegeMode};
use crate::discovery;

pub fn run_console(args: Vec<String>) -> Result<()> {
    if let Some(cmd) = args.get(0).map(|s| s.as_str()) {
        match cmd {
            "search" | "s" => return cmd_search(),
            "list" => return cmd_list(args.get(1..).unwrap_or_default()),
            "link" | "l" => return cmd_link(args.get(1..).unwrap_or_default()),
            "help" | "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
    }

    run_repl()
}

fn print_help() {
    println!("usbip-client console mode");
    println!();
    println!("Subcommands:");
    println!("  usbip-client search|s");
    println!("  usbip-client list <host>");
    println!("  usbip-client link|l <host> <busid...>");
    println!();
    println!("Interactive (default): run `usbip-client` to enter REPL. Type `help` for commands.");
}

fn tokio_rt() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!(e))
}

fn cmd_search() -> Result<()> {
    let rt = tokio_rt()?;
    rt.block_on(async {
        let c = Client::new(PrivilegeMode::ConsoleSudo);
        let servers = c.discover(discovery::DiscoveryCfg::default()).await?;
        if servers.is_empty() {
            println!("No usbip-server found");
            return Ok(());
        }
        for (i, s) in servers.iter().enumerate() {
            let name = s
                .info
                .server_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let ver = s.info.version.clone().unwrap_or_else(|| "?".to_string());
            println!("#{i} {}  name={name}  version={ver}", s.addr.ip());
        }
        Ok(())
    })
}

fn cmd_list(rest: &[String]) -> Result<()> {
    let Some(host) = rest.get(0) else {
        return Err(anyhow!("Usage: usbip-client list <host>"));
    };
    let rt = tokio_rt()?;
    rt.block_on(async {
        let c = Client::new(PrivilegeMode::ConsoleSudo);
        let devs = c.list_remote_devices(host).await?;
        if devs.is_empty() {
            println!("No devices");
            return Ok(());
        }
        for d in devs {
            let vp = d.vidpid.unwrap_or_else(|| "-".to_string());
            let desc = if d.desc.is_empty() { "-" } else { &d.desc };
            println!("{}  vidpid={}  desc={}", d.busid, vp, desc);
        }
        Ok(())
    })
}

fn cmd_link(rest: &[String]) -> Result<()> {
    let Some(host) = rest.get(0) else {
        return Err(anyhow!("Usage: usbip-client link <host> <busid...>"));
    };
    let busids: Vec<String> = rest.get(1..).unwrap_or_default().to_vec();
    if busids.is_empty() {
        return Err(anyhow!("Usage: usbip-client link <host> <busid...>"));
    }
    let rt = tokio_rt()?;
    rt.block_on(async {
        let c = Client::new(PrivilegeMode::ConsoleSudo);
        c.ensure_vhci_loaded().await?;
        for b in busids {
            match c.attach(host, &b).await {
                Ok(_) => println!("OK {host} {b}"),
                Err(e) => println!("FAIL {host} {b}: {e:#}"),
            }
        }
        Ok(())
    })
}

#[derive(Default)]
struct ReplState {
    listed: Vec<ListedDevice>,
}

#[derive(Debug, Clone)]
struct ListedDevice {
    idx: usize,
    host: String,
    busid: String,
    desc: String,
    vidpid: Option<String>,
}

fn run_repl() -> Result<()> {
    println!("usbip-client REPL");
    println!("Type 'help' for commands, 'quit' to exit.\n");

    let rt = tokio_rt()?;
    let mut st = ReplState::default();

    print!("Scanning... ");
    io::stdout().flush().ok();
    match refresh_device_list(&rt, &mut st) {
        Ok(_) => {
            println!("done.");
            display_device_table(&st);
        }
        Err(e) => println!("scan failed: {:#}", e),
    }

    loop {
        print!("\n> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();

        match cmd {
            "quit" | "exit" | "q" => break,
            "help" | "h" | "?" => {
                println!("Commands:");
                println!("  list, l          Refresh and show all devices");
                println!("  connect, c <#>   Connect devices (e.g., c 1,3-5)");
                println!("  help             Show this help");
                println!("  quit, q          Exit");
            }
            "list" | "l" => {
                print!("Refreshing... ");
                io::stdout().flush().ok();
                match refresh_device_list(&rt, &mut st) {
                    Ok(_) => {
                        println!("done.");
                        display_device_table(&st);
                    }
                    Err(e) => println!("failed: {:#}", e),
                }
            }
            "connect" | "c" => {
                if args.is_empty() {
                    println!("Usage: connect <indices> (e.g., c 1,3-5)");
                    continue;
                }
                let spec = args.join(" ");
                let indices = match parse_index_spec(&spec) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("Invalid indices: {}", e);
                        continue;
                    }
                };
                let targets: Vec<_> = indices
                    .iter()
                    .filter_map(|&idx| st.listed.iter().find(|d| d.idx == idx).cloned())
                    .collect();
                if targets.is_empty() {
                    println!("No matching devices.");
                    continue;
                }

                println!("Connecting {} device(s)...", targets.len());
                let res = rt.block_on(async {
                    let c = Client::new(PrivilegeMode::ConsoleSudo);
                    c.ensure_vhci_loaded().await?;
                    let mut results = Vec::new();
                    for t in targets {
                        match c.attach(&t.host, &t.busid).await {
                            Ok(_) => results.push(format!("OK {} {}", t.host, t.busid)),
                            Err(e) => results.push(format!("FAIL {} {}: {:#}", t.host, t.busid, e)),
                        }
                    }
                    Ok::<_, anyhow::Error>(results)
                });

                match res {
                    Ok(lines) => {
                        for line in lines {
                            println!("{}", line);
                        }
                    }
                    Err(e) => println!("Connection error: {:#}", e),
                }
            }
            _ => println!("Unknown command: {}", cmd),
        }
    }
    Ok(())
}

fn refresh_device_list(rt: &tokio::runtime::Runtime, st: &mut ReplState) -> Result<()> {
    let c = Client::new(PrivilegeMode::ConsoleSudo);
    let servers = rt.block_on(c.discover(discovery::DiscoveryCfg::default()))?;

    let mut listed = Vec::new();
    let mut idx = 1;
    for srv in servers {
        let host = srv.addr.ip().to_string();
        let devs = match rt.block_on(c.list_remote_devices(&host)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: {} list failed: {:#}", host, e);
                continue;
            }
        };
        for d in devs {
            listed.push(ListedDevice {
                idx,
                host: host.clone(),
                busid: d.busid,
                desc: if d.desc.is_empty() { "-".to_string() } else { d.desc },
                vidpid: d.vidpid,
            });
            idx += 1;
        }
    }
    st.listed = listed;
    Ok(())
}

fn display_device_table(st: &ReplState) {
    if st.listed.is_empty() {
        println!("No devices found.");
        return;
    }
    println!("\n{:<6} {:<18} {:<10} {:<12} {}", "INDEX", "HOST", "BUSID", "VID:PID", "DESC");
    for d in &st.listed {
        let vp = d.vidpid.as_deref().unwrap_or("-");
        println!(
            "{:<6} {:<18} {:<10} {:<12} {}",
            d.idx,
            truncate(&d.host, 18),
            truncate(&d.busid, 10),
            truncate(vp, 12),
            truncate(&d.desc, 40)
        );
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        format!("{}…", s.chars().take(max_len - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

fn parse_index_spec(spec: &str) -> Result<Vec<usize>> {
    let mut out: Vec<usize> = Vec::new();
    for part in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let start: usize = a.trim().parse().map_err(|_| anyhow!("invalid range: {part}"))?;
            let end: usize = b.trim().parse().map_err(|_| anyhow!("invalid range: {part}"))?;
            if start == 0 || end == 0 {
                return Err(anyhow!("indices start at 1: {part}"));
            }
            let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
            for i in lo..=hi {
                out.push(i);
            }
        } else {
            let i: usize = part.parse().map_err(|_| anyhow!("invalid number: {part}"))?;
            if i == 0 {
                return Err(anyhow!("indices start at 1: {part}"));
            }
            out.push(i);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}