use clap::{Parser, Subcommand};
use magpie_hud_server::{auth::KeyDatabase, ServerConfig};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Magpie HUD headless collector and authenticated web dashboard"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Start the collector, web UI and authenticated API (the default).
    Serve,
    /// Manage HUD viewing keys. These are NOT Magpie model API keys.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Print a sample configuration; edit paths before using it.
    ConfigExample,
}
#[derive(Subcommand)]
enum KeyCommand {
    Create {
        #[arg(long)]
        name: String,
    },
    List,
    Revoke {
        id: String,
    },
    Rotate {
        id: String,
    },
}
fn execute() -> Result<(), String> {
    let cli = Cli::parse();
    let mut config: ServerConfig = if let Some(path) = cli.config {
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "无法读取服务配置")?)
            .map_err(|e| format!("服务配置无效：{e}"))?
    } else {
        ServerConfig::default()
    };
    if let Some(directory) = cli.data_dir {
        config.data_dir = directory;
    }
    match cli.command.unwrap_or(Command::Serve) {
        Command::ConfigExample => println!(
            "{}",
            serde_json::to_string_pretty(&config).map_err(|_| "无法生成配置")?
        ),
        Command::Key { command } => {
            let mut db = KeyDatabase::load(&config.data_dir)?;
            match command {
                KeyCommand::List => {
                    for (id, record) in db.keys {
                        println!("{id}\t{}\t{}", record.name, record.created_at);
                    }
                }
                KeyCommand::Create { name } => {
                    let (_, token) = db.create(name);
                    db.save(&config.data_dir)?;
                    println!("{token}");
                    eprintln!("请保存此 HUD 查看密钥；服务端不保留可恢复的原文。");
                }
                KeyCommand::Revoke { id } => {
                    if db.keys.remove(&id).is_none() {
                        return Err("HUD 密钥 ID 不存在".into());
                    }
                    db.save(&config.data_dir)?;
                    eprintln!("密钥已撤销；对应网页会话和实时连接将失效。");
                }
                KeyCommand::Rotate { id } => {
                    let record = db.keys.remove(&id).ok_or("HUD 密钥 ID 不存在")?;
                    let (_, token) = db.create(record.name);
                    db.save(&config.data_dir)?;
                    println!("{token}");
                    eprintln!("旧密钥已撤销，请更新客户端保存的新密钥。");
                }
            }
        }
        Command::Serve => {
            // chrono::Local uses the process timezone. Set it before constructing
            // the Tokio runtime or spawning any collector thread.
            if let Some(tz) = &config.timezone {
                std::env::set_var("TZ", tz);
            }
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| "无法创建服务运行时")?
                .block_on(magpie_hud_server::serve(config))?;
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
