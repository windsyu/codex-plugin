# vt100 0.16.2 local patch

Source: the published MIT-licensed `vt100` 0.16.2 crate, pinned by the prior
Cargo.lock registry checksum. The `src/`, Cargo manifest, LICENSE, README and
CHANGELOG were copied unchanged before applying the patch below. The Cargo
registry and the official Codex installation/reference source remain unchanged.

Published crate checksum: `054ff75fb8fa83e609e685106df4faeffdf3a735d3c74ebce97ec557d5d36fd9`.

Upstream report: <https://github.com/doy/vt100-rust/issues/28>.

The sole source change is in `Row::resize`: clear the last cell, preserving its
attributes, if a shrink cut off a wide character's continuation. This is the
invariant already enforced by upstream `Row::truncate`. Grid resize applies it
to both active and inactive screens without injecting ANSI into a partially
parsed UTF-8/escape sequence. A subsequent erase or overwrite stays in bounds.

Regression: `workbench::terminal_screen::tests::shrinking_through_wide_cells_keeps_both_screens_erasable`
failed with an out-of-bounds panic against the original crate. It checks normal
and alternate screens, shrink/expand, erase, Chinese/emoji and paste-mode
preservation. The real Chrome/installed CLI probe also resizes a Chinese draft.

See ADR 0040. Remove this patch when a released upstream version includes the
fix and passes the same regression, snapshot/mode and real browser checks.
No dependency upgrade or protocol change is implied by the local patch.
