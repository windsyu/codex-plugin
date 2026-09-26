//! Fixed synthetic P07 measurements; shared with opt-in history pressure tests.
#[path = "../src/workbench/probe_process.rs"]
mod probe_process;
#[path = "support/r0_web_timing.rs"]
mod timing;

fn main() -> anyhow::Result<()> {
    timing::main()
}
