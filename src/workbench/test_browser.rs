//! Installed Chrome probe. Capabilities go through the child environment and
//! never through argv, stdout, screenshots or unsanitized Playwright errors.

use std::path::Path;
use std::process::Stdio;

use super::probe_process::ProbeProcess;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};

pub(crate) struct BrowserProbe {
    child: ProbeProcess,
    lines: Lines<BufReader<ChildStdout>>,
}
impl BrowserProbe {
    pub fn start(
        url: &str,
        partial: &str,
        final_text: &str,
        mode: &str,
        screenshot: Option<&Path>,
    ) -> Result<Self> {
        let mut command = Command::new("node");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/web/e2e/r0-reading-probe.cjs"
            ))
            .env("WORKBENCH_PROBE_URL", url)
            .env("WORKBENCH_PROBE_PARTIAL", partial)
            .env("WORKBENCH_PROBE_FINAL", final_text)
            .env("WORKBENCH_PROBE_MODE", mode)
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(path) = screenshot {
            command.env("WORKBENCH_PROBE_SCREENSHOT", path);
        } else {
            command.env_remove("WORKBENCH_PROBE_SCREENSHOT");
        }
        let mut child =
            ProbeProcess::spawn(&mut command).context("start installed Chrome probe")?;
        let lines = BufReader::new(child.stdout.take().context("browser probe stdout")?).lines();
        Ok(Self { child, lines })
    }
    pub async fn next(&mut self) -> Result<Value> {
        let Some(line) = self
            .lines
            .next_line()
            .await
            .context("read browser probe status")?
        else {
            bail!("browser probe exited before reporting completion");
        };
        serde_json::from_str(&line).map_err(|_| anyhow::anyhow!("invalid browser probe status"))
    }
    pub async fn wait(&mut self) -> Result<()> {
        if !self
            .child
            .wait()
            .await
            .context("wait for browser probe")?
            .success()
        {
            bail!("browser probe failed (see sanitized stage)");
        }
        Ok(())
    }
}
impl Drop for BrowserProbe {
    fn drop(&mut self) {
        // EOF triggers the probe's bounded browser.close() even on a Rust panic.
        self.child.stdin.take();
    }
}
