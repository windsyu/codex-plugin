//! The observer owns a separate executor thread. Parsing or reading-page work
//! cannot occupy the runtime that forwards the CLI's model traffic.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;

use super::capture::CaptureReceiver;
use super::decode::{Decoder, DecoderLimits};
use super::live::LiveHub;
use super::redaction::RedactionPolicy;

pub struct Observer {
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl Observer {
    pub fn start(
        receiver: CaptureReceiver,
        hub: Arc<LiveHub>,
        policy: Arc<RedactionPolicy>,
        limits: DecoderLimits,
    ) -> Result<Self> {
        Self::start_with_delay(receiver, hub, policy, limits, Duration::ZERO)
    }

    /// A bounded R0 fault-injection hook; never accepts a browser parameter.
    pub fn start_with_delay(
        mut receiver: CaptureReceiver,
        hub: Arc<LiveHub>,
        policy: Arc<RedactionPolicy>,
        limits: DecoderLimits,
        delay: Duration,
    ) -> Result<Self> {
        let (stop, mut stop_rx) = watch::channel(false);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .context("build observation executor")?;
        let thread = std::thread::Builder::new().name("model-observer".into()).spawn(move || {
            runtime.block_on(async move {
                tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = stop_rx.changed() => return }
                let mut decoder = Decoder::new(limits, policy);
                let mut health = tokio::time::interval(Duration::from_millis(50));
                loop {
                    tokio::select! {
                        biased;
                        _ = stop_rx.changed() => break,
                        _ = health.tick() => hub.capture_health(receiver.stats()),
                        observation = receiver.recv() => {
                            let Some(observation) = observation else { break; };
                            decoder.push(&observation, |event| hub.apply(event));
                            hub.capture_health(receiver.stats());
                        }
                    }
                }
            });
        }).context("spawn observation executor")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
