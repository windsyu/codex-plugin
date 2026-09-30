//! Installed CLI experiment driver. Callers supply an isolated temporary home.
//! This is not production terminal input code and never retries submissions.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

pub(crate) struct NativeProbe {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<(Instant, Vec<u8>)>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    terminal: vt100::Parser,
    query: Vec<u8>,
}
impl NativeProbe {
    pub fn start(home: &Path, cwd: &Path, args: &[String]) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 45,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new(
            std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
        );
        command.cwd(cwd);
        // Callers provide a private fixture directory; CLI support files must
        // stay there as well, rather than inheriting the user's real home.
        command.env("HOME", home);
        command.env("USERPROFILE", home);
        command.env("CODEX_HOME", home);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.args(args);
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let (sender, output) = mpsc::channel(64);
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(count) = reader.read(&mut buffer) {
                let read_at = Instant::now();
                if count == 0
                    || sender
                        .blocking_send((read_at, buffer[..count].to_vec()))
                        .is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            _master: pair.master,
            writer,
            output,
            reader_thread: Some(reader_thread),
            terminal: vt100::Parser::new(45, 120, 0),
            query: Vec::new(),
        })
    }
    pub fn screen(&self) -> String {
        self.terminal.screen().contents()
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }
    pub async fn pump(&mut self) -> Result<Option<Instant>> {
        let Ok(bytes) = tokio::time::timeout(Duration::from_millis(50), self.output.recv()).await
        else {
            return Ok(None);
        };
        let (at, bytes) =
            bytes.ok_or_else(|| anyhow::anyhow!("native output ended before expected evidence"))?;
        for byte in bytes {
            self.terminal.process(&[byte]);
            if byte == 0x1b {
                self.query.clear();
                self.query.push(byte);
            } else if !self.query.is_empty() {
                self.query.push(byte);
                if self.query.len() >= 3 && (0x40..=0x7e).contains(&byte) {
                    if self.query.as_slice() == b"\x1b[6n" {
                        let (row, col) = self.terminal.screen().cursor_position();
                        self.write(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes())?;
                    }
                    let reply = match self.query.as_slice() {
                        b"\x1b[c" | b"\x1b[0c" => Some(b"\x1b[?1;2c".as_slice()),
                        b"\x1b[>c" | b"\x1b[>0c" => Some(b"\x1b[>0;0;0c".as_slice()),
                        b"\x1b[?u" => Some(b"\x1b[?0u".as_slice()),
                        _ => None,
                    };
                    if let Some(reply) = reply {
                        self.write(reply)?;
                    }
                    self.query.clear();
                } else if self.query.len() > 32 {
                    self.query.clear();
                }
            }
        }
        Ok(Some(at))
    }
    fn cursor_is_in_prompt(&self) -> bool {
        let screen = self.terminal.screen();
        let (row, column) = screen.cursor_position();
        !screen.hide_cursor()
            && column > 0
            && screen
                .rows(0, screen.size().1)
                .nth(usize::from(row))
                .is_some_and(|line| line.trim_start().starts_with('›'))
    }
    fn readiness_diagnostics(&self) -> serde_json::Value {
        let screen = self.screen();
        serde_json::json!({
            "welcome":screen.contains("OpenAI Codex"),
            "promptLine":screen.lines().any(|line| line.trim_start().starts_with('›')),
            "theme":screen.contains("Choose your style") || screen.contains("Select a theme"),
            "trust":screen.contains("Do you trust") || screen.contains("Do you want to work") || screen.contains("Trust this folder?"),
            "folderAccess":screen.contains("Folder access"),
            "modelNotice":screen.contains("Try new model") && screen.contains("Use existing model"),
            "existingModelSelected":screen.lines().any(|line| {
                let line = line.trim_start();
                (line.starts_with('›') || line.starts_with('>')) && line.contains("Use existing model")
            }),
            "cursor":self.terminal.screen().cursor_position(),
            "cursorInPrompt":self.cursor_is_in_prompt(),
        })
    }
    pub async fn ready(&mut self) -> Result<()> {
        let mut model_notice_handled = false;
        let mut model_notice_at = None;
        let mut model_selection_sent = false;
        let mut themed = false;
        let mut trusted = false;
        let mut ready_at = None;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                self.pump().await?;
                let screen = self.screen();
                if !self.cursor_is_in_prompt() {
                    ready_at = None;
                }
                if screen.contains("Try new model") && screen.contains("Use existing model") {
                    let settled = model_notice_at.get_or_insert_with(Instant::now).elapsed()
                        >= Duration::from_millis(300);
                    if settled && !model_selection_sent {
                        model_selection_sent = true;
                        self.write(b"\x1b[B")?;
                    } else if model_selection_sent
                        && !model_notice_handled
                        && screen.lines().any(|line| {
                            let line = line.trim_start();
                            (line.starts_with('›') || line.starts_with('>'))
                                && line.contains("Use existing model")
                        })
                    {
                        model_notice_handled = true;
                        self.write(b"\r")?;
                    }
                } else if !themed
                    && (screen.contains("Choose your style") || screen.contains("Select a theme"))
                {
                    self.write(b"\r")?;
                    themed = true;
                } else if !trusted
                    && (screen.contains("Do you trust")
                        || screen.contains("Do you want to work in this directory")
                        || screen.contains("Trust this folder?"))
                {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    self.write(b"\r")?;
                    trusted = true;
                } else if screen.contains("OpenAI Codex")
                    && self.cursor_is_in_prompt()
                    && ready_at.get_or_insert_with(Instant::now).elapsed()
                        >= Duration::from_millis(250)
                {
                    println!("NATIVE_READY {}", self.readiness_diagnostics());
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "native readiness deadline; {}",
                self.readiness_diagnostics()
            )
        })?
    }
    pub async fn submit(&mut self, prompt: &str) -> Result<()> {
        let pasted_at = Instant::now();
        self.write(format!("\x1b[200~{prompt}\x1b[201~").as_bytes())?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                self.pump().await?;
                if self.screen().contains(prompt)
                    && pasted_at.elapsed() >= Duration::from_millis(100)
                {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "native draft not displayed; no Enter sent; {}",
                self.readiness_diagnostics()
            )
        })??;
        self.write(b"\r")
    }
    pub async fn quit(&mut self) -> Result<()> {
        // Native Ctrl-D on an empty input exits; this does not kill the process.
        self.write(b"\x04")?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    ensure!(status.success(), "native CLI exit failed");
                    return Ok::<_, anyhow::Error>(());
                }
                // Drain the PTY so shutdown output cannot block native exit.
                let _ = self.pump().await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("native graceful exit deadline"))?
    }
}
impl Drop for NativeProbe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.output.close();
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
    }
}
