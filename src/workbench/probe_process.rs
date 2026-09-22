//! Test/example-only owner for a Node browser probe, never linked into the product.
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

pub(crate) struct ProbeProcess(Child);
impl ProbeProcess {
    pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.kill_on_drop(false).spawn().map(Self)
    }
}
impl Deref for ProbeProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl DerefMut for ProbeProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        self.0.stdin.take();
        if let Some(pid) = self.0.id() {
            // This unreaped child is our Node probe, never a user's browser.
            // Its handler closes/kills the separately owned Chrome group.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("browser probe cleanup exceeded its deadline");
        let _ = self.0.start_kill();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
#[path = "probe_process_tests.rs"]
mod tests;
