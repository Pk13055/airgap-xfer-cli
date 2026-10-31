# airgap-xfer operator notes

These notes cover protocol v2 with resumable optical transfer. Both machines
must run the same build: a v1 peer is reported as `the other side runs
protocol v1; this build speaks v2`, and the two will not talk.

Implementation plan: [resilient optical transfer](superpowers/plans/2026-10-31-resilient-optical-transfer.md).

## Screen layout

- **Left column:** this side's outgoing codes. They are always black on
  white, one terminal column per QR module, and never clipped. Once linked,
  the sender tiles as many DATA codes as fit.
- **Right pane while aiming:** the full camera preview. Use it to point the
  lid.
- **Right pane once linked:** a fuzzy ASCII *peer view*. It is a blurred,
  perspective-corrected crop of the other side's code(s) built from real
  camera pixels. It is deliberately not a QR code, so the other camera can
  never read it by mistake.
  - Its colour changes when the camera reads a code it has not just read. A
    code held on screen keeps one colour, so a frozen colour means nothing
    new is arriving.
  - When tracking drops or decodes stop for 1.2 s, the last picture stays up,
    dimmed, under a yellow `STALE Ns · reacquiring` label.
- **Narrow terminals:** if the right pane would get fewer than 24 columns, it
  is hidden. The title line still says `peer view live` or `peer view STALE,
  reacquiring`. The sender gives up a column of codes for the peer view only
  when it has three or more columns.

## Confirmations

| Side | What you do |
|---|---|
| Sender | Aim until the title shows `● tracking`, press Enter to link. When the prompt names the file, size and chunk count, press Enter once more to send. |
| Receiver | Nothing. It shows HELLO, links, and accepts on its own. |

Guards:
- Enter does nothing before tracking locks.
- The receiver never takes Enter as acceptance.
- An Enter within 0.4 s of a prompt appearing is ignored, so a held or bounced key cannot confirm two prompts.
- Reconnects never prompt.

## Interruptions and resume

- The file travels as fixed 96-byte chunks. A bigger QR code carries more
  chunks per frame, but the chunk numbering never changes, so falling back to
  a smaller code keeps everything already received.
- **Covered camera or lost alignment:** both sides keep sending (data from
  the sender, progress reports from the receiver). The transfer continues the
  moment the view returns.
- **Longer losses:** after about 20 rounds without new chunks, a side
  reconnects by itself. On reconnect:
  - The receiver checks that the new offer matches the interrupted one in
    every field, then reports what it already holds.
  - The sender fills the missing ranges, lowest first.
  - The transcript says `resuming with N/M chunks`.
- An attempt that moved data reconnects at the same QR size. An attempt that
  moved nothing steps down to a smaller, slower code. Ten fruitless attempts
  end the transfer.
- Received data lives in memory only. **Quitting either program forgets the
  partial transfer.** Nothing is checkpointed to disk.
- The receiver checks the whole archive's SHA-256 before using it. It writes
  and fsyncs `<name>.tar.zst`, unpacks it, and only then shows OK. The sender
  reports success only after that OK.

## Decoder benchmark

```
cargo run --release --example decoder_bench
```

Measured on Pk-MBA.local, Darwin 25.2.0 (arm64, T8122), on 2026-11-01:

```
15 iterations per row, 1920x1080 synthetic frames
path                                      median ms     p90 ms       ok
v10 full-frame scan                            2.93       4.46    15/15
v10 tracked crop                               2.83       3.16    15/15
v10 monitor crop (96 px)                       0.06       0.06    15/15
v25 full-frame scan                            2.57       2.71    15/15
v25 tracked crop                               3.61       3.76    15/15
v25 monitor crop (96 px)                       0.06       0.06    15/15
v40 full-frame scan                            3.16       3.47    15/15
v40 tracked crop                               4.52       7.47    11/15
v40 monitor crop (96 px)                       0.06       0.06    15/15
4 x v15 tiles, decode_all                     34.76      35.25    15/15

Note: `v40 tracked crop` decoded only 11/15 iterations on this synthetic frame, so its timing mixes failed and successful decodes.
```

Run it on both laptops and compare. If the monitor goes STALE during steady
transfer while the benchmark shows long decode times, the camera thread is
the bottleneck.

## Verification status

### Automated results (this repository, one machine)

| Check | Command | Result |
|---|---|---|
| Release build | `cargo build --release` | pass |
| Test suite | `cargo test` | 144 passed, 0 failed |
| Single-threaded | `cargo test -- --test-threads=1` | 144 passed, 0 failed |
| Repeat run (protocol) | `cargo test -- session:: transport::` ×5 | 5/5 green (24 tests each; the `cargo test session:: transport::` form in the plan is rejected by cargo, so the filters were passed after `--`) |
| Formatting (changed files) | `rustfmt --edition 2021 --check …` | clean |
| Formatting (whole crate) | `cargo fmt --check` | drift in `src/lib.rs` and `src/pack.rs` only (pre-existing) |
| Lint | `cargo clippy --all-targets` | 2 pre-existing warnings: `warning: field assignment outside of initializer for an instance created with Default::default()` (`src/qr.rs:196:5`); `warning: you seem to be trying to use `match` for destructuring a single pattern. Consider using `if let`` (`src/link.rs:408:9`) |
| Doc links | `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps` | pass |
| Markdown links | relative-link check over this file | pass |

These are in-process tests over a simulated optical channel and synthetic
camera frames. They cover layout, confirmation guards, duplicates and
conflicts, offer identity, loss and resume, fallback recovery, and the order
of archive persistence and OK. **They do not exercise real webcams or
screens.**

### Outstanding two-device checks (not yet performed)

- [ ] macOS ↔ Windows: a 10 MB+ directory sends end to end. The receiver's tree matches the source.
- [ ] Aiming: the preview is on the right and the code on the left. The sender's Enter is ignored until `● tracking`. The receiver never asks for a key.
- [ ] After LINK: the preview is replaced by the peer view. Its colour changes as new codes arrive. Holding one code (the receiver waiting) keeps one colour.
- [ ] Cover the receiver's camera for ~5 s mid-transfer: the peer view goes `STALE · reacquiring`, then returns to live. The chunk count continues rather than restarting.
- [ ] Cover for 2+ minutes: both sides reconnect without a prompt. The transcript shows `resuming with N/M chunks`, and fewer than M chunks are resent.
- [ ] Force a fallback (tilt the lid until large codes fail): the code shrinks and already-received chunks are kept.
- [ ] Shrink a terminal below the code width + 24 columns: the right pane disappears and the codes stay whole and readable.
- [ ] Kill the receiver during unpacking: the sender does not report success.
- [ ] Quit and restart either side mid-transfer: the next transfer starts from zero (no recovery after quitting).
- [ ] Run a v1 binary against this build: both sides show the protocol-version mismatch.
- [ ] Sustained use: a 30+ minute transfer, with keys responsive throughout and no growth in memory (Activity Monitor / Task Manager).
- [ ] Record `decoder_bench` output on both laptops in the benchmark section above.
