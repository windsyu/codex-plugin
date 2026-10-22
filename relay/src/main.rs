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
    /// Retained for compatibility; same-origin routing is always enabled.
    #[arg(long, global = true)]
    single_origin: bool,
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
        /// Private file to bootstrap windsyu; an existing administrator password is unchanged.
        #[arg(long)]
        bootstrap_password_file: Option<PathBuf>,
    },
    /// Offline windsyu administrator recovery; stop the service first.
    RecoverAdmin {
        #[arg(long)]
        password_file: PathBuf,
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
            single_origin: self.single_origin,
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
    if let Command::RecoverAdmin { password_file } = &a.command {
        let dir = a
            .data_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--data-dir is required"))?;
        Store::open(dir)?.recover_admin_from_file(password_file)?;
        println!("Administrator recovery completed; all previous credentials are invalid.");
        return Ok(());
    }
    let c = a.config()?;
    match a.command {
        Command::Serve {
            bind,
            bootstrap_password_file,
        } => {
            let store = Store::open(&c.data_dir)?;
            if let Some(path) = bootstrap_password_file {
                store.bootstrap_admin_from_file(&path)?;
            }
            anyhow::ensure!(
                store.has_admin()?,
                "first deployment requires --bootstrap-password-file pointing to a private password file"
            );
            drop(store);
            codex_relayd::serve(c, bind).await?;
        }
        Command::RecoverAdmin { .. } => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_origin_is_default_and_legacy_routing_flags_are_rejected() {
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
        assert!(parse(&["serve"]).is_ok());
        assert!(parse(&["--single-origin", "serve"]).is_ok());
        for flag in [
            "--workbench-domain",
            "--workbench-origin",
            "--fixed-gateway-id",
        ] {
            assert!(parse(&[flag, "obsolete", "serve"]).is_err());
        }
    }
}
