use clap::{Parser, Subcommand};
use codex_relayd::{Config, Store};
use std::{net::SocketAddr, path::PathBuf};
#[derive(Parser)]
#[command(version, about = "Trusted public workbench relay (deploy behind TLS)")]
struct Args {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    portal_origin: Option<String>,
    #[arg(long, global = true, conflicts_with_all = ["workbench_origin", "fixed_gateway_id"])]
    workbench_domain: Option<String>,
    #[arg(long, global = true, requires = "fixed_gateway_id")]
    workbench_origin: Option<String>,
    #[arg(long, global = true, requires = "workbench_origin")]
    fixed_gateway_id: Option<uuid::Uuid>,
    #[arg(long, global = true, default_value_t = 4)]
    password_workers: usize,
    #[arg(long, global = true)]
    allow_insecure_loopback: bool,
    #[arg(long, global = true)]
    trusted_client_ip_header: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: SocketAddr,
    },
    Invite {
        #[arg(long)]
        username: String,
    },
    Reset {
        #[arg(long)]
        username: String,
    },
    Disable {
        #[arg(long)]
        username: String,
    },
}
impl Args {
    fn config(&self) -> anyhow::Result<Config> {
        let c = Config {
            data_dir: self
                .data_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--data-dir is required"))?,
            portal_origin: self
                .portal_origin
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--portal-origin is required"))?,
            workbench_domain: self.workbench_domain.clone().unwrap_or_default(),
            workbench_origin: self.workbench_origin.clone(),
            fixed_gateway_id: self.fixed_gateway_id,
            password_workers: self.password_workers,
            allow_insecure_loopback: self.allow_insecure_loopback,
            trusted_client_ip_header: self.trusted_client_ip_header.clone(),
        };
        c.validate()?;
        Ok(c)
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let c = a.config()?;
    match a.command {
        Command::Serve { bind } => codex_relayd::serve(c, bind).await?,
        Command::Invite { username } => {
            let t = Store::open(&c.data_dir)?.issue(&username, false)?;
            println!("{}/#invite={}", c.portal_origin, t);
        }
        Command::Reset { username } => {
            let t = Store::open(&c.data_dir)?.issue(&username, true)?;
            println!("{}/#reset={}", c.portal_origin, t);
        }
        Command::Disable { username } => Store::open(&c.data_dir)?.disable(&username)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_requires_one_routing_mode_and_fixed_identity_pair() {
        let base = [
            "codex-relayd",
            "--data-dir",
            "/tmp/synthetic-unused",
            "--portal-origin",
            "https://192.0.2.10:9443",
        ];
        let parse = |tail: &[&str]| -> anyhow::Result<Config> {
            Args::try_parse_from(base.into_iter().chain(tail.iter().copied()))?.config()
        };
        assert!(parse(&["--workbench-domain", "example.com", "serve"]).is_ok());
        assert!(
            Args::try_parse_from([
                "codex-relayd",
                "serve",
                "--data-dir",
                "/tmp/synthetic-unused",
                "--portal-origin",
                "https://192.0.2.10:9443",
                "--workbench-domain",
                "example.com"
            ])
            .is_ok()
        );
        assert!(
            Args::try_parse_from(["codex-relayd", "--workbench-domain", "example.com", "serve"])
                .unwrap()
                .config()
                .is_err()
        );
        assert!(parse(&["serve"]).is_err());
        assert!(parse(&["--workbench-origin", "https://192.0.2.10:9444", "serve"]).is_err());
        let id = "5b626c57-ff39-4482-a408-de601ebf86d2";
        assert!(
            parse(&[
                "--fixed-gateway-id",
                id,
                "--workbench-domain",
                "example.com",
                "serve"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--fixed-gateway-id",
                id,
                "--workbench-origin",
                "https://192.0.2.10:9444",
                "--password-workers",
                "1",
                "serve"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "--fixed-gateway-id",
                id,
                "--workbench-origin",
                "https://192.0.2.10:9444",
                "--workbench-domain",
                "example.com",
                "serve"
            ])
            .is_err()
        );
    }
}
