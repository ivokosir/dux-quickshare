#[macro_use]
extern crate log;

mod clip;
mod daemon;
mod notify;
mod paths;

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const USAGE: &str = "qsbar — Quick Share receiver for the bar

  qsbar daemon                 run the receiver
  qsbar watch                  stream state as JSON lines (for the bar)
  qsbar status                 print state once
  qsbar visible | hidden | toggle
  qsbar temp [minutes]         visible for a while (default 5)
  qsbar accept <id> [copy|save]
  qsbar reject <id>
  qsbar trust <device> | untrust <device>
  qsbar copy|save|open|reveal|remove <item>
  qsbar clear                  forget history (saved files stay)
  qsbar downloads              open the Downloads folder";

async fn request(req: &Value) -> Result<Value> {
    let stream = UnixStream::connect(paths::socket_path())
        .await
        .map_err(|_| anyhow!("qsbar daemon is not running"))?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{req}\n").as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    let line = lines.next_line().await?.ok_or_else(|| anyhow!("no reply"))?;
    Ok(serde_json::from_str(&line)?)
}

/// Streams snapshots forever, reconnecting when the daemon restarts.
async fn watch() -> Result<()> {
    let mut announced_down = false;
    loop {
        if let Ok(stream) = UnixStream::connect(paths::socket_path()).await {
            announced_down = false;
            let (read, mut write) = stream.into_split();
            if write.write_all(b"{\"cmd\":\"watch\"}\n").await.is_ok() {
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{line}");
                }
            }
        }
        if !announced_down {
            println!("{}", json!({"daemon": false}));
            announced_down = true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let cmd = arg(0);

    let req = match cmd.as_str() {
        "daemon" => {
            if std::env::var("RUST_LOG").is_err() {
                std::env::set_var("RUST_LOG", "info,mdns_sd=off,polling=error,neli=error,bluez_async=error,bluer=error,btleplug=error");
            }
            tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
            if let Err(e) = daemon::run().await {
                eprintln!("qsbar: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        "watch" => {
            let _ = watch().await;
            return;
        }
        "status" | "visible" | "hidden" | "toggle" | "clear" | "downloads" => json!({"cmd": cmd}),
        "temp" => json!({"cmd": "temp", "minutes": arg(1).parse::<u64>().unwrap_or(5)}),
        "accept" => json!({"cmd": "accept", "id": arg(1), "action": arg(2)}),
        "reject" => json!({"cmd": "reject", "id": arg(1)}),
        "trust" | "untrust" => json!({"cmd": cmd, "device": args[1..].join(" ")}),
        "copy" | "save" | "open" | "reveal" | "remove" => json!({"cmd": cmd, "item": arg(1)}),
        _ => {
            println!("{USAGE}");
            std::process::exit(if cmd.is_empty() || cmd == "help" || cmd == "--help" { 0 } else { 2 });
        }
    };

    match request(&req).await {
        Ok(reply) => {
            if cmd == "status" {
                println!("{}", serde_json::to_string_pretty(&reply).unwrap_or_default());
            } else if reply["ok"] == false {
                eprintln!("qsbar: {}", reply["error"].as_str().unwrap_or("failed"));
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("qsbar: {e}");
            std::process::exit(1);
        }
    }
}
