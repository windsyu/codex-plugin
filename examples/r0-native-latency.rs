//! Fixed synthetic P07 measurements; shared with opt-in history pressure tests.
#[path = "../src/workbench/test_native.rs"]
mod test_native;
#[path = "support/r0_native_timing.rs"]
mod timing;

fn main() -> anyhow::Result<()> {
    timing::main()
}
