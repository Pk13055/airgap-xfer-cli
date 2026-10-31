# Resilient Optical Transfer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **The user asked for all changes to stay uncommitted.** Every task ends with a *checkpoint* (tests plus `git status --short`), not a commit. Never run `git commit`, `git add`, `git stash`, or anything else that writes to git history or the index.

**Goal:** Make `airgap-xfer` transfers easier to monitor, able to survive temporary optical loss without restarting the file, and responsive during long runs. Build this into the existing CLI, TUI, webcam capture and QR protocol rather than as a new application.

**Architecture:** Protocol v2 cuts the archive into stable 96-byte logical chunks that are independent of the QR version. Each DATA frame packs as many adjacent missing chunks as the negotiated QR version holds. The receiver answers with PROGRESS reports: a contiguous prefix plus bitmaps over 32-chunk windows. Every transfer message carries a transfer id derived from the full offer. The receiver keeps its assembled chunks in an in-memory `ReceiveCache` across fallback attempts, and reuses them only for a field-for-field identical offer.

The fallback ladder works like this:
- An attempt that moved chunks reconnects at the same rung.
- An attempt that moved nothing steps down to a smaller QR version.
- A HELLO, PROBE or LINK that the transport reads is replayed to the next handshake.

The receiver says OK only after the archive has been returned, written, fsynced and unpacked. In the TUI, one pure layout function drives both drawing and the sender's tile count. The right-hand pane shows the camera preview while aiming. After linking it shows a fuzzy, tinted ASCII rendering of the rectified camera crop of the peer's code, and dims and labels it STALE when tracking or decoding stops.

**Tech Stack:** Rust 2021 (toolchain 1.97.1), clap 4.5, ratatui 0.29, crossterm 0.28, image 0.25, nokhwa 0.10, qrcode 0.14, rxing 0.7, sha2 0.10, crc32fast 1.4, tar, zstd. No new crates.

## Global Constraints

- **No new network transport, no GUI, no new dependencies.** `Cargo.toml` `[dependencies]` and `Cargo.lock` must be byte-identical at the end.
- **No new CLI options.** `send` keeps exactly `path`, `--camera`, `--keep-temp`, `--no-invert`. `recv` keeps exactly `outdir`, `--camera`, `--force`, `--keep-temp`, `--no-invert`. Help *text* may change.
- **Resume is in memory only.** No checkpoint files, no recovery after either process quits. The only file the receiver writes is the final `<basename>.tar.zst` plus the unpacked tree, as today.
- **Protocol v2 wire format:**
  - Magic `AX`, version byte `2`, big-endian fields, CRC32 trailer.
  - Logical chunk = **96 bytes**. Progress window = **32 chunks**.
  - Frames from any other version are never decoded. They are reported as a protocol mismatch.
- **Full SHA-256 verification is retained**: the receiver checks the whole archive before returning it, and checks it again before persisting it.
- **Completion order:** recv returns the blob → write → `fsync` file (and parent dir on Unix) → unpack → only then show OK. On any failure, show FAIL and never OK.
- **Outgoing codes:** black-on-white, one terminal column per module, two modules per row, never clipped, always in the left column. The ASCII monitor must never use the `█ ▀ ▄` glyphs or a white background.
- **Confirmation model unchanged:** the sender aims until tracking locks, presses Enter, then presses Enter once more to send. The receiver never confirms. Reconnects and resumes never prompt.
- **Do not touch:** `bin/airgap-xfer` (do not rebuild into it), `.superpowers/`, `docs/superpowers/plans/2026-08-16-*`, `docs/superpowers/specs/*`, `install.sh`, `install.ps1`, `src/pack.rs`, `src/main.rs`, or anything outside this repository (sibling directories `../laya`, `../skills`).
- **Formatting:**
  - Run `rustfmt --edition 2021 <file>` on every `.rs` file you create or modify, **except `src/lib.rs`**: rustfmt recurses from `lib.rs` into every module and would reformat untouched files.
  - `cargo fmt --check` already fails on every file at baseline. Report whatever drift remains in untouched files; do not fix it.
- **Lint:**
  - `cargo clippy --all-targets` may show only these pre-existing warnings, where their lines still exist:
    - `src/qr.rs` `decode_all` (`field assignment outside of initializer`)
    - `src/link.rs` HELLO loop (`match` for a single pattern)
    - the `block v0.1.6` future-incompat notice
  - New code must add no warnings.
- **Doc links:** `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps` must pass.
- **Deterministic tests:**
  - Inject loss by counting frames, never by wall-clock sleeps.
  - Time-dependent logic takes an explicit `Instant` so tests can pass synthetic times.
  - Fast-mode millisecond timeouts (existing `fast` configs) are allowed.
- **Reporting:** keep "automated results" strictly separate from "outstanding two-device checks". Never claim a two-device behaviour was verified.

---

## File Map

| Path | Status | Responsibility |
|---|---|---|
| `src/chunks.rs` | create | 96-byte logical chunks, `ChunkSet` bitset, `ChunkStore` with duplicate and conflict accounting, span planning, progress reports |
| `src/resume.rs` | create | `Offer` (transfer identity), `ReceiveCache` (in-memory resume), `process_nonce` |
| `src/deliver.rs` | create | `ArchiveSink`, `DiskSink` (write + fsync + unpack), `deliver` (OK strictly last) |
| `src/tui/gate.rs` | create | Pure keypress rules for confirmation gates |
| `src/tui/layout.rs` | create | `Chrome`, `plan()`, `TileGrid`, `ScreenSize`, `data_tiles()`, shared by the renderer and tile allocation |
| `src/tui/monitor.rs` | create | Fuzzy ASCII peer monitor: `ascii_rows`, `freshness`, `tint`, `MonitorWidget` |
| `examples/decoder_bench.rs` | create | Synthetic decoder benchmark (no webcam) |
| `docs/operator-notes.md` | create | Operator notes, automated results, outstanding two-device checks |
| `src/lib.rs` | modify | Register `chunks`, `resume`, `deliver` |
| `src/error.rs` | modify | `Restart`, `ProtocolMismatch`, `Refused` |
| `src/frame.rs` | rewrite | v2 envelope and payloads, capacities, `foreign_version`, `fail_error` |
| `src/qr.rs` | modify | v2 capacity preflight; test payload shapes |
| `src/optical.rs` | modify | `Lossy` by first chunk, `Replay` pushback |
| `src/link.rs` | modify | v2 handshake: offer, cache-aware acceptance, progress-seeded sender |
| `src/transport.rs` | rewrite | v2 span/progress transport |
| `src/session.rs` | rewrite (Task 4) | Progress-aware ladder, replay, cache across attempts |
| `src/cli.rs` | modify | Build the `Offer`; receive through `deliver`; help text |
| `src/live.rs` | modify | Test payload shape only |
| `src/detect.rs` | modify | `Tracker::region`, `hull_quad`, `Quad::area` |
| `src/tui/mod.rs` | rewrite (Task 7) | Layout-driven draw, gate rules, monitor pane |
| `src/tui/optical.rs` | modify | Labels, `ScreenSize`/`Tiles`, foreign-version error |
| `src/tui/camera.rs` | rewrite (Task 8) | Screen-hold dedupe, epoch, peer crop, preview skip after link |
| `src/tui/widget.rs` | modify | `QrGridWidget` draws from a `TileGrid` |

Task order: 1 → 2 → 3 → 4 → 5 (protocol, all testable with the in-memory `optical::pair`), then 6 → 7 → 8 → 9 (UI), then 10 (bench, docs, verification). Every task leaves `cargo build` and `cargo test` green.

---

### Task 1: Logical chunks, chunk sets, and the chunk store

**Files:**
- Create: `src/chunks.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const CHUNK_BYTES: usize = 96`
  - `pub const WINDOW_CHUNKS: u32 = 32`
  - `pub fn chunk_count(size: u64) -> Option<u32>`
  - `pub fn span_bytes(size: u64, first: u32, count: u32) -> Option<Range<usize>>`
  - `pub fn report_limit(contiguous: u32, max_windows: usize, total: u32) -> u32`
  - `pub fn plan_spans(have: &ChunkSet, per_frame: u32, limit: u32, max_frames: usize) -> Vec<(u32, u32)>`
  - `ChunkSet`:
    - `new(total)`, `from_report(total, contiguous, &[u32]) -> Option<Self>`
    - `total()`, `count()`, `is_complete()`, `contains(i)`, `insert(i) -> bool`, `contiguous()`
    - `window_bits(w) -> u32`, `report(max_windows) -> (u32, Vec<u32>)`, `apply_report(contiguous, &[u32]) -> Option<u32>`
  - `ChunkStore`:
    - `new(size) -> Option<Self>`
    - `size()`, `have() -> &ChunkSet`, `is_complete()`, `conflicts() -> u64`, `blob() -> &[u8]`
    - `next_serial() -> u16`, `accept(first, count, &[u8]) -> Result<Accepted, SpanError>`
  - `Accepted { new: u32, duplicate: u32, conflict: u32 }`
  - `SpanError { OutOfRange, WrongLength }`

- [ ] **Step 1: Write the failing tests**

Create `src/chunks.rs` with only the test module below, and add `pub mod chunks;` to `src/lib.rs` directly under `pub mod cli;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_count_depends_only_on_size() {
        assert_eq!(chunk_count(0), Some(0));
        assert_eq!(chunk_count(1), Some(1));
        assert_eq!(chunk_count(96), Some(1));
        assert_eq!(chunk_count(97), Some(2));
        assert_eq!(chunk_count(u64::from(u32::MAX) * 96), Some(u32::MAX));
        assert_eq!(chunk_count(u64::from(u32::MAX) * 96 + 1), None);
    }

    #[test]
    fn spans_cover_whole_chunks_and_a_short_last_one() {
        assert_eq!(span_bytes(200, 0, 1), Some(0..96));
        assert_eq!(span_bytes(200, 0, 3), Some(0..200));
        assert_eq!(span_bytes(200, 2, 1), Some(192..200));
        assert_eq!(span_bytes(200, 2, 2), None, "runs past the last chunk");
        assert_eq!(span_bytes(200, 3, 1), None);
        assert_eq!(span_bytes(200, 0, 0), None);
    }

    #[test]
    fn inserting_twice_never_counts_twice() {
        let mut set = ChunkSet::new(70);
        assert!(set.insert(3));
        assert!(!set.insert(3));
        assert!(!set.insert(70), "out of range");
        assert_eq!(set.count(), 1);
        assert_eq!(set.contiguous(), 0);
        for index in 0..3 {
            set.insert(index);
        }
        assert_eq!(set.contiguous(), 4);
        for index in 0..70 {
            set.insert(index);
        }
        assert!(set.is_complete());
        assert_eq!(set.contiguous(), 70);
    }

    #[test]
    fn a_report_round_trips_through_the_wire_shape() {
        let mut set = ChunkSet::new(100);
        for index in (0..40).chain([45, 70, 99]) {
            set.insert(index);
        }
        let (contiguous, bitmaps) = set.report(8);
        assert_eq!(contiguous, 40);
        assert_eq!(bitmaps.len(), 3, "windows 1, 2 and 3 (the last one short)");
        assert_eq!(ChunkSet::from_report(100, contiguous, &bitmaps), Some(set));
    }

    #[test]
    fn a_report_is_capped_and_a_complete_set_has_nothing_left_to_report() {
        let mut set = ChunkSet::new(64 * 32);
        assert_eq!(set.report(5).1.len(), 5);
        for index in 0..set.total() {
            set.insert(index);
        }
        assert_eq!(set.report(5), (64 * 32, vec![]));
    }

    #[test]
    fn a_report_for_a_different_length_is_ignored() {
        let mut set = ChunkSet::new(10);
        assert_eq!(set.apply_report(11, &[]), None);
        assert_eq!(set.count(), 0);
        assert_eq!(set.apply_report(4, &[u32::MAX]), Some(10));
        assert_eq!(set.apply_report(4, &[u32::MAX]), Some(0), "repeats add nothing");
    }

    #[test]
    fn spans_pack_adjacent_holes_lowest_first() {
        let mut have = ChunkSet::new(40);
        for index in [0, 1, 5, 6, 7] {
            have.insert(index);
        }
        assert_eq!(plan_spans(&have, 4, 40, 3), vec![(2, 3), (8, 4), (12, 4)]);
        assert_eq!(plan_spans(&have, 4, 10, 8), vec![(2, 3), (8, 2)], "never past the limit");
        assert_eq!(plan_spans(&have, 0, 40, 8), vec![]);
    }

    #[test]
    fn the_limit_is_the_end_of_the_last_reportable_window() {
        assert_eq!(report_limit(0, 2, 1000), 64);
        assert_eq!(report_limit(40, 2, 1000), 96);
        assert_eq!(report_limit(40, 2, 70), 70);
    }

    #[test]
    fn duplicates_do_not_inflate_progress_and_conflicts_keep_the_first_bytes() {
        let mut store = ChunkStore::new(200).unwrap();
        let first: Vec<u8> = (0..96).collect();
        assert_eq!(
            store.accept(0, 1, &first),
            Ok(Accepted { new: 1, ..Accepted::default() })
        );
        assert_eq!(
            store.accept(0, 1, &first),
            Ok(Accepted { duplicate: 1, ..Accepted::default() })
        );
        assert_eq!(
            store.accept(0, 1, &[0xEE; 96]),
            Ok(Accepted { conflict: 1, ..Accepted::default() })
        );
        assert_eq!(store.have().count(), 1);
        assert_eq!(store.conflicts(), 1);
        assert_eq!(&store.blob()[..96], &first[..]);
    }

    #[test]
    fn a_span_mixing_held_and_new_chunks_counts_each_once() {
        let mut store = ChunkStore::new(200).unwrap();
        store.accept(1, 1, &[1; 96]).unwrap();
        let mut span = vec![0u8; 96];
        span.extend([1; 96]);
        span.extend([2; 8]);
        assert_eq!(
            store.accept(0, 3, &span),
            Ok(Accepted { new: 2, duplicate: 1, conflict: 0 })
        );
        assert!(store.is_complete());
        assert_eq!(store.blob(), &span[..]);
    }

    #[test]
    fn malformed_spans_change_nothing() {
        let mut store = ChunkStore::new(200).unwrap();
        assert_eq!(store.accept(0, 1, &[0; 95]), Err(SpanError::WrongLength));
        assert_eq!(store.accept(3, 1, &[0; 8]), Err(SpanError::OutOfRange));
        assert_eq!(store.have().count(), 0);
    }

    #[test]
    fn every_report_gets_a_new_serial() {
        let mut store = ChunkStore::new(10).unwrap();
        let first = store.next_serial();
        assert_ne!(first, store.next_serial());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test chunks::`
Expected: compile errors such as `cannot find function chunk_count in this scope` and `cannot find type ChunkSet`.

- [ ] **Step 3: Implement**

Put this above the test module in `src/chunks.rs`:

```rust
//! Version-independent transfer units.
//!
//! The archive is cut into fixed [`CHUNK_BYTES`]-byte logical chunks once,
//! when the offer is made. The negotiated QR version only decides how many
//! adjacent chunks ride in one DATA frame, so a link that falls back to a
//! smaller code keeps every chunk the receiver already holds.

use std::ops::Range;

/// Bytes per logical chunk. Only the archive's last chunk may be shorter.
pub const CHUNK_BYTES: usize = 96;
/// Chunks described by one progress bitmap.
pub const WINDOW_CHUNKS: u32 = 32;

/// Chunks an archive of `size` bytes is cut into, or `None` when that does
/// not fit the protocol's 32-bit chunk indices.
pub fn chunk_count(size: u64) -> Option<u32> {
    u32::try_from(size.div_ceil(CHUNK_BYTES as u64)).ok()
}

/// Byte range of chunks `first..first + count` in an archive of `size`
/// bytes, or `None` when the span is empty or runs past the last chunk.
pub fn span_bytes(size: u64, first: u32, count: u32) -> Option<Range<usize>> {
    if count == 0 {
        return None;
    }
    let chunk = CHUNK_BYTES as u64;
    let end_chunk = u64::from(first) + u64::from(count);
    if (end_chunk - 1) * chunk >= size {
        return None;
    }
    let start = u64::from(first) * chunk;
    let end = (end_chunk * chunk).min(size);
    Some(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

/// First chunk past the last window a receiver can describe in one report
/// of `max_windows` bitmaps starting at `contiguous`'s window.
pub fn report_limit(contiguous: u32, max_windows: usize, total: u32) -> u32 {
    let first = u64::from(contiguous / WINDOW_CHUNKS);
    let end = (first + max_windows as u64) * u64::from(WINDOW_CHUNKS);
    end.min(u64::from(total)) as u32
}

/// Picks up to `max_frames` spans of adjacent chunks missing from `have`,
/// lowest first, each at most `per_frame` long and none reaching `limit`.
///
/// Filling the lowest holes first keeps a resumed transfer roughly
/// sequential instead of scattering it across the file.
pub fn plan_spans(have: &ChunkSet, per_frame: u32, limit: u32, max_frames: usize) -> Vec<(u32, u32)> {
    let mut spans = Vec::new();
    if per_frame == 0 {
        return spans;
    }
    let limit = limit.min(have.total());
    let mut index = have.contiguous();
    while index < limit && spans.len() < max_frames {
        if have.contains(index) {
            index += 1;
            continue;
        }
        let first = index;
        while index < limit && !have.contains(index) && index - first < per_frame {
            index += 1;
        }
        spans.push((first, index - first));
    }
    spans
}

/// Which chunks of a transfer are present, as a fixed-length bitset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSet {
    total: u32,
    words: Vec<u64>,
    count: u32,
}

impl ChunkSet {
    /// An empty set for a transfer of `total` chunks.
    pub fn new(total: u32) -> Self {
        Self {
            total,
            words: vec![0; (total as usize).div_ceil(64)],
            count: 0,
        }
    }

    /// The sender's view rebuilt from a receiver's progress report, or
    /// `None` when the report cannot describe a transfer of `total` chunks.
    pub fn from_report(total: u32, contiguous: u32, bitmaps: &[u32]) -> Option<Self> {
        let mut set = Self::new(total);
        set.apply_report(contiguous, bitmaps)?;
        Some(set)
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    /// Chunks present. Raised only when [`ChunkSet::insert`] reports a new
    /// chunk, so a repeated delivery can never inflate it.
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn is_complete(&self) -> bool {
        self.count == self.total
    }

    pub fn contains(&self, index: u32) -> bool {
        index < self.total && self.words[index as usize / 64] & (1u64 << (index % 64)) != 0
    }

    /// Marks `index` present. Returns whether it was missing before.
    pub fn insert(&mut self, index: u32) -> bool {
        if index >= self.total || self.contains(index) {
            return false;
        }
        self.words[index as usize / 64] |= 1u64 << (index % 64);
        self.count += 1;
        true
    }

    /// Index of the first missing chunk, or `total` when complete.
    pub fn contiguous(&self) -> u32 {
        for (i, word) in self.words.iter().enumerate() {
            if *word != u64::MAX {
                return (i as u32 * 64 + word.trailing_ones()).min(self.total);
            }
        }
        self.total
    }

    /// Bitmap of window `window`: bit `k` covers chunk `window * 32 + k`.
    /// Chunks past the end read as present, so a short final window is
    /// complete as soon as its real chunks are.
    pub fn window_bits(&self, window: u32) -> u32 {
        let base = u64::from(window) * u64::from(WINDOW_CHUNKS);
        (0..WINDOW_CHUNKS).fold(0, |bits, k| {
            let index = base + u64::from(k);
            if index >= u64::from(self.total) || self.contains(index as u32) {
                bits | 1 << k
            } else {
                bits
            }
        })
    }

    /// A progress report: the first missing chunk, then one bitmap per
    /// window from that chunk's window onward, at most `max_windows` of them.
    pub fn report(&self, max_windows: usize) -> (u32, Vec<u32>) {
        let contiguous = self.contiguous();
        let first = contiguous / WINDOW_CHUNKS;
        let last = self.total.div_ceil(WINDOW_CHUNKS);
        let bitmaps = (first..last)
            .take(max_windows)
            .map(|window| self.window_bits(window))
            .collect();
        (contiguous, bitmaps)
    }

    /// Folds a progress report in. Returns how many chunks were newly
    /// marked, or `None` (changing nothing) when the report cannot describe
    /// a transfer of this length.
    pub fn apply_report(&mut self, contiguous: u32, bitmaps: &[u32]) -> Option<u32> {
        if contiguous > self.total {
            return None;
        }
        let before = self.count;
        for index in self.contiguous()..contiguous {
            self.insert(index);
        }
        let first = u64::from(contiguous / WINDOW_CHUNKS);
        for (offset, bits) in bitmaps.iter().enumerate() {
            let base = (first + offset as u64) * u64::from(WINDOW_CHUNKS);
            for k in 0..WINDOW_CHUNKS {
                let index = base + u64::from(k);
                if bits & (1 << k) != 0 && index < u64::from(self.total) {
                    self.insert(index as u32);
                }
            }
        }
        Some(self.count - before)
    }
}

/// What one DATA span did to the receiver's copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    /// Chunks stored for the first time.
    pub new: u32,
    /// Chunks already held with identical bytes.
    pub duplicate: u32,
    /// Chunks already held with *different* bytes. The first copy is kept.
    pub conflict: u32,
}

/// Why a DATA span could not be applied at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanError {
    OutOfRange,
    WrongLength,
}

/// The receiver's copy of the archive, assembled chunk by chunk.
#[derive(Clone, Debug)]
pub struct ChunkStore {
    size: u64,
    blob: Vec<u8>,
    have: ChunkSet,
    conflicts: u64,
    serial: u16,
}

impl ChunkStore {
    /// An empty store for an archive of `size` bytes, or `None` when that
    /// cannot be held in memory on this machine.
    pub fn new(size: u64) -> Option<Self> {
        Some(Self {
            size,
            blob: vec![0; usize::try_from(size).ok()?],
            have: ChunkSet::new(chunk_count(size)?),
            conflicts: 0,
            serial: 0,
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn have(&self) -> &ChunkSet {
        &self.have
    }

    pub fn is_complete(&self) -> bool {
        self.have.is_complete()
    }

    /// Chunks that arrived again with different bytes, over the store's life.
    pub fn conflicts(&self) -> u64 {
        self.conflicts
    }

    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    /// A new serial for the next progress report, so the sender's camera
    /// never mistakes a fresh report for the one already on screen.
    pub fn next_serial(&mut self) -> u16 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Applies DATA carrying chunks `first..first + count`.
    ///
    /// A chunk already held is never overwritten: identical bytes count as a
    /// duplicate, different bytes as a conflict. Only genuinely new chunks
    /// raise the progress count.
    pub fn accept(&mut self, first: u32, count: u32, bytes: &[u8]) -> Result<Accepted, SpanError> {
        let span = span_bytes(self.size, first, count).ok_or(SpanError::OutOfRange)?;
        if span.len() != bytes.len() {
            return Err(SpanError::WrongLength);
        }
        let mut accepted = Accepted::default();
        for index in first..first + count {
            let chunk = span_bytes(self.size, index, 1).expect("inside a validated span");
            let incoming = &bytes[chunk.start - span.start..chunk.end - span.start];
            if !self.have.contains(index) {
                self.blob[chunk].copy_from_slice(incoming);
                self.have.insert(index);
                accepted.new += 1;
            } else if &self.blob[chunk] == incoming {
                accepted.duplicate += 1;
            } else {
                accepted.conflict += 1;
                self.conflicts += 1;
            }
        }
        Ok(accepted)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test chunks::`
Expected: 12 tests pass. Then run `cargo test` to confirm the full suite still passes (76 pre-existing tests plus 12).

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/chunks.rs
cargo test 2>&1 | grep "test result"
git status --short
```
Expected: `src/chunks.rs` is untracked and `src/lib.rs` is modified. Nothing else changes.

---

### Task 2: Transfer identity and the in-memory resume cache

**Files:**
- Create: `src/resume.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `chunks::{chunk_count, ChunkStore}` (Task 1).
- Produces:
  - `pub const UNSCOPED: u64 = 0`
  - `Offer`:
    - Fields, all `pub`: `transfer_id: u64`, `sender_nonce: u64`, `basename: String`, `uncompressed_hint: u64`, `compressed_size: u64`, `chunk_count: u32`, `sha256: [u8; 32]`
    - Derives: `Clone, Debug, PartialEq, Eq`
    - `Offer::new(basename: impl Into<String>, uncompressed_hint: u64, compressed_size: u64, sha256: [u8; 32], sender_nonce: u64) -> Option<Offer>`
    - `Offer::is_consistent(&self) -> bool`
  - `pub fn process_nonce() -> u64`
  - `pub enum Reuse { Fresh, Resumed { held: u32 }, Replaced { dropped: u32 } }`
  - `#[derive(Default)] pub struct ReceiveCache` with:
    - `adopt(&mut self, &Offer) -> Option<Reuse>`
    - `store(&mut self, &Offer) -> Option<&mut ChunkStore>`
    - `held(&self) -> u32`
    - `clear(&mut self)`

- [ ] **Step 1: Write the failing tests**

Create `src/resume.rs` containing only this test module, and add `pub mod resume;` to `src/lib.rs` under `pub mod qr;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> Offer {
        Offer::new("dir", 1000, 500, [7; 32], 1).unwrap()
    }

    #[test]
    fn an_offer_derives_its_chunk_count_and_a_scoped_id() {
        let offer = offer();
        assert_eq!(offer.chunk_count, 6);
        assert_ne!(offer.transfer_id, UNSCOPED);
        assert!(offer.is_consistent());
        assert_eq!(
            Offer::new("dir", 1000, 500, [7; 32], 1).unwrap(),
            offer,
            "the same inputs name the same transfer, whatever the QR version"
        );
    }

    #[test]
    fn every_field_feeds_the_id() {
        let base = offer();
        for other in [
            Offer::new("dir2", 1000, 500, [7; 32], 1),
            Offer::new("dir", 1001, 500, [7; 32], 1),
            Offer::new("dir", 1000, 501, [7; 32], 1),
            Offer::new("dir", 1000, 500, [8; 32], 1),
            Offer::new("dir", 1000, 500, [7; 32], 2),
        ] {
            assert_ne!(other.unwrap().transfer_id, base.transfer_id);
        }
    }

    #[test]
    fn a_tampered_offer_is_inconsistent() {
        let mut forged = offer();
        forged.transfer_id ^= 1;
        assert!(!forged.is_consistent());
        let mut miscounted = offer();
        miscounted.chunk_count += 1;
        assert!(!miscounted.is_consistent());
    }

    #[test]
    fn the_cache_resumes_only_an_identical_offer() {
        let a = offer();
        let mut cache = ReceiveCache::default();
        assert_eq!(cache.adopt(&a), Some(Reuse::Fresh));
        cache.store(&a).unwrap().accept(0, 2, &[5; 192]).unwrap();
        assert_eq!(cache.adopt(&a), Some(Reuse::Resumed { held: 2 }));
        assert_eq!(cache.held(), 2);

        let b = Offer::new("dir", 1000, 500, [8; 32], 1).unwrap();
        assert_eq!(cache.adopt(&b), Some(Reuse::Replaced { dropped: 2 }));
        assert_eq!(cache.held(), 0);
        assert!(cache.store(&a).is_none(), "the old transfer's chunks are gone");
        assert!(cache.store(&b).is_some());
    }

    #[test]
    fn clearing_forgets_everything() {
        let a = offer();
        let mut cache = ReceiveCache::default();
        cache.adopt(&a).unwrap();
        cache.store(&a).unwrap().accept(0, 1, &[5; 96]).unwrap();
        cache.clear();
        assert_eq!(cache.held(), 0);
        assert_eq!(cache.adopt(&a), Some(Reuse::Fresh));
    }

    #[test]
    fn process_nonces_never_repeat() {
        assert_ne!(process_nonce(), process_nonce());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test resume::`
Expected: compile errors because `Offer`, `ReceiveCache`, `Reuse`, `UNSCOPED` and `process_nonce` are not defined.

- [ ] **Step 3: Implement**

Put this above the tests in `src/resume.rs`:

```rust
//! Transfer identity and the receiver's in-memory resume cache.
//!
//! A receiver that loses the optical link keeps every chunk it has assembled
//! for as long as the process lives. When the sender comes back — after a
//! covered camera, a lost alignment, or a fallback to a smaller QR — its
//! offer is compared field by field with the one the chunks arrived under,
//! and they are reused only on an exact match. Nothing is written to disk:
//! quitting either side forgets the partial transfer.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::chunks::{chunk_count, ChunkStore};

/// Transfer id for frames sent before any offer exists, e.g. a FAIL painted
/// because the display itself broke.
pub const UNSCOPED: u64 = 0;

/// Everything the sender commits to in GO. Two offers name the same transfer
/// only when every field matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub transfer_id: u64,
    /// Different for every sender process, so a receiver never resumes into
    /// a sender that has since quit and been started again.
    pub sender_nonce: u64,
    pub basename: String,
    pub uncompressed_hint: u64,
    pub compressed_size: u64,
    pub chunk_count: u32,
    pub sha256: [u8; 32],
}

impl Offer {
    /// The offer for an archive, with its chunk count and id derived. `None`
    /// when the archive has more chunks than 32-bit indices can number.
    pub fn new(
        basename: impl Into<String>,
        uncompressed_hint: u64,
        compressed_size: u64,
        sha256: [u8; 32],
        sender_nonce: u64,
    ) -> Option<Self> {
        let mut offer = Self {
            transfer_id: UNSCOPED,
            sender_nonce,
            basename: basename.into(),
            uncompressed_hint,
            compressed_size,
            chunk_count: chunk_count(compressed_size)?,
            sha256,
        };
        offer.transfer_id = offer.derived_id();
        Some(offer)
    }

    /// Whether the id and chunk count are the ones this content implies.
    /// An offer failing this came from a different protocol revision or was
    /// damaged past what the frame CRC caught, and is refused.
    pub fn is_consistent(&self) -> bool {
        chunk_count(self.compressed_size) == Some(self.chunk_count)
            && self.transfer_id == self.derived_id()
    }

    /// First eight bytes of SHA-256 over every other field. Derived rather
    /// than random so a fallback re-offer needs no extra state; never
    /// [`UNSCOPED`].
    fn derived_id(&self) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(b"airgap-xfer offer v2");
        hasher.update(self.sender_nonce.to_be_bytes());
        hasher.update((self.basename.len() as u64).to_be_bytes());
        hasher.update(self.basename.as_bytes());
        hasher.update(self.uncompressed_hint.to_be_bytes());
        hasher.update(self.compressed_size.to_be_bytes());
        hasher.update(self.chunk_count.to_be_bytes());
        hasher.update(self.sha256);
        let digest = hasher.finalize();
        match u64::from_be_bytes(digest[..8].try_into().expect("eight bytes")) {
            UNSCOPED => 1,
            id => id,
        }
    }
}

/// A value unique to this process (and to this call), for
/// [`Offer::sender_nonce`].
pub fn process_nonce() -> u64 {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_be_bytes());
    hasher.update(nanos.to_be_bytes());
    hasher.update(CALLS.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    u64::from_be_bytes(hasher.finalize()[..8].try_into().expect("eight bytes"))
}

/// What adopting an offer did to the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reuse {
    /// Nothing was held; an empty store was created.
    Fresh,
    /// The identical offer was held; its `held` chunks carry over.
    Resumed { held: u32 },
    /// A different offer was held; its `dropped` chunks were discarded.
    Replaced { dropped: u32 },
}

/// The receiver's partial transfer, kept across fallback attempts for as
/// long as the process runs.
#[derive(Default)]
pub struct ReceiveCache {
    held: Option<(Offer, ChunkStore)>,
}

impl ReceiveCache {
    /// Makes `offer` the current transfer. Held chunks are reused only when
    /// every field of `offer` matches the one they arrived under. `None` when
    /// the archive cannot be held in memory here.
    pub fn adopt(&mut self, offer: &Offer) -> Option<Reuse> {
        if let Some((held_offer, store)) = &self.held {
            if held_offer == offer {
                return Some(Reuse::Resumed {
                    held: store.have().count(),
                });
            }
        }
        let store = ChunkStore::new(offer.compressed_size)?;
        let dropped = self.held.take().map(|(_, store)| store.have().count());
        self.held = Some((offer.clone(), store));
        Some(match dropped {
            Some(dropped) => Reuse::Replaced { dropped },
            None => Reuse::Fresh,
        })
    }

    /// The store for `offer`, if `offer` is the one currently adopted.
    pub fn store(&mut self, offer: &Offer) -> Option<&mut ChunkStore> {
        match &mut self.held {
            Some((held_offer, store)) if held_offer == offer => Some(store),
            _ => None,
        }
    }

    /// Chunks held for the current transfer, or 0.
    pub fn held(&self) -> u32 {
        self.held
            .as_ref()
            .map_or(0, |(_, store)| store.have().count())
    }

    /// Forgets everything, e.g. after a hash mismatch proves an accepted
    /// chunk was wrong and resuming would fail identically.
    pub fn clear(&mut self) {
        self.held = None;
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test resume:: && cargo test 2>&1 | grep "test result"`
Expected: 6 new tests pass and the whole suite is green.

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/resume.rs
git status --short
```

---

### Task 3: Protocol v2 cut-over (wire format, handshake, transport)

This is the one task where the crate cannot compile part-way through, so its steps are ordered by file. You must finish all of them before running the tests.

**Files:**
- Rewrite: `src/frame.rs`, `src/transport.rs`
- Modify: `src/error.rs`, `src/qr.rs`, `src/optical.rs`, `src/link.rs`, `src/session.rs`, `src/cli.rs`, `src/live.rs`, `src/tui/optical.rs`, `src/tui/camera.rs`

**Interfaces:**
- Consumes:
  - From Task 1: `ChunkSet`, `ChunkStore`, `plan_spans`, `report_limit`, `span_bytes`, `CHUNK_BYTES`, `WINDOW_CHUNKS`
  - From Task 2: `Offer`, `ReceiveCache`, `Reuse`, `UNSCOPED`, `process_nonce`
- Produces:
  - `frame::PROTOCOL_VERSION = 2`
  - `frame::Payload` variants:
    - `Hello{protocol_ver, role}`, `Probe{..}` and `Link{..}` (unchanged)
    - `Go(Offer)`
    - `Data { transfer_id: u64, first: u32, count: u8, bytes: Vec<u8> }`
    - `Progress { transfer_id: u64, serial: u16, contiguous: u32, bitmaps: Vec<u32> }`
    - `Fin { transfer_id: u64, sha256: [u8; 32] }`
    - `Ok { transfer_id: u64 }`
    - `Fail { transfer_id: u64, reason: u8 }`
  - `frame::chunks_per_frame(u8) -> u32` (v10..v40: 1, 2, 4, 7, 9, 17)
  - `frame::progress_windows(u8) -> usize` (v10: 29, others: 64)
  - `frame::foreign_version(&[u8]) -> Option<u8>`
  - `frame::fail_error(u8) -> Error`
  - Constants: `frame::DATA_HEADER = 13`, `frame::PROGRESS_HEADER = 15`, `frame::GO_FIXED = 70`, `frame::MAX_PROGRESS_WINDOWS = 64`
  - `Error::Restart(Box<Payload>)`, `Error::ProtocolMismatch { peer: u8, ours: u8 }`, `Error::Refused(&'static str)`
  - `link::Session { qr_version: u8, dwell_ms: u16, offer: Offer }`
  - `link::Linked { session: Session, acked: ChunkSet }`
  - `link::run_send_handshake(opt, offer: &Offer, cfg) -> Result<Linked>`
  - `link::run_recv_handshake(opt, outdir, force, cfg, cache: &mut ReceiveCache) -> Result<Session>`
  - `transport::send_blob(opt, &Session, blob: &[u8], acked: &mut ChunkSet, cfg) -> Result<()>`
  - `transport::recv_blob(opt, &Session, store: &mut ChunkStore, cfg) -> Result<Vec<u8>>`
  - `transport::progress_payload(&Offer, &mut ChunkStore, windows: usize) -> Payload`
  - `transport::BURST_FRAMES = 32`
  - `optical::Lossy { inner, drop_first: HashSet<u32> }`
  - `session::send_with_fallback(opt, &Offer, blob, terminal_max) -> Result<Completed>`

- [ ] **Step 1: Errors**

In `src/error.rs`, add `use crate::frame::Payload;` after `use std::path::PathBuf;`, and add these variants after `Aborted`:

```rust
    /// The peer started a new handshake (HELLO, PROBE or LINK) while this
    /// side was mid-transfer. The payload is kept so the next handshake can
    /// consume it: the peer will not show it "again".
    #[error("peer restarted the handshake")]
    Restart(Box<Payload>),
    #[error("the other side runs protocol v{peer}; this build speaks v{ours}. Install the same airgap-xfer on both machines")]
    ProtocolMismatch { peer: u8, ours: u8 },
    #[error("receiver refused the offer: {0}")]
    Refused(&'static str),
```

- [ ] **Step 2: Rewrite `src/frame.rs`**

Replace the whole file with:

```rust
//! The wire format: an `AX` envelope around one protocol message.
//!
//! Protocol v2 moves the archive in fixed 96-byte logical chunks (see
//! [`crate::chunks`]) and scopes every transfer message to the offer's
//! transfer id, so frames from an older attempt or another transfer can
//! never be mistaken for this one.

use crate::{chunks::CHUNK_BYTES, resume::Offer, Error, Result};
use crc32fast::Hasher;

pub const PROTOCOL_VERSION: u8 = 2;
pub const ROLE_RECV: u8 = 2;
pub const FAIL_HASH: u8 = 1;
pub const FAIL_DISK: u8 = 2;
pub const FAIL_PROTOCOL: u8 = 3;
pub const FAIL_ABORTED: u8 = 4;
pub const ENVELOPE_OVERHEAD: usize = 10;
/// DATA body before the chunk bytes: transfer id, first chunk, chunk count.
pub const DATA_HEADER: usize = 8 + 4 + 1;
/// PROGRESS body before the bitmaps: transfer id, serial, contiguous, count.
pub const PROGRESS_HEADER: usize = 8 + 2 + 4 + 1;
/// GO body besides the basename: name length, size hint, compressed size,
/// chunk count, SHA-256, transfer id, sender nonce.
pub const GO_FIXED: usize = 2 + 8 + 8 + 4 + 32 + 8 + 8;
/// Most window bitmaps one progress report carries: 2048 chunks ahead.
pub const MAX_PROGRESS_WINDOWS: usize = 64;
pub const KIND_HELLO: u8 = 1;
pub const KIND_PROBE: u8 = 2;
pub const KIND_LINK: u8 = 3;
pub const KIND_GO: u8 = 4;
pub const KIND_DATA: u8 = 5;
pub const KIND_PROGRESS: u8 = 6;
pub const KIND_FIN: u8 = 7;
pub const KIND_OK: u8 = 8;
pub const KIND_FAIL: u8 = 9;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hello = 1,
    Probe = 2,
    Link = 3,
    Go = 4,
    Data = 5,
    Progress = 6,
    Fin = 7,
    Ok = 8,
    Fail = 9,
}

impl Kind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            KIND_HELLO => Some(Kind::Hello),
            KIND_PROBE => Some(Kind::Probe),
            KIND_LINK => Some(Kind::Link),
            KIND_GO => Some(Kind::Go),
            KIND_DATA => Some(Kind::Data),
            KIND_PROGRESS => Some(Kind::Progress),
            KIND_FIN => Some(Kind::Fin),
            KIND_OK => Some(Kind::Ok),
            KIND_FAIL => Some(Kind::Fail),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Hello {
        protocol_ver: u8,
        role: u8,
    },
    Probe {
        id: u8,
        qr_version: u8,
        dwell_ms: u16,
        last: bool,
    },
    Link {
        qr_version: u8,
        dwell_ms: u16,
        probe_loss: u8,
    },
    Go(Offer),
    /// Chunks `first..first + count`, back to back.
    Data {
        transfer_id: u64,
        first: u32,
        count: u8,
        bytes: Vec<u8>,
    },
    /// Every chunk below `contiguous` is held; `bitmaps[k]` covers window
    /// `contiguous / 32 + k`.
    Progress {
        transfer_id: u64,
        serial: u16,
        contiguous: u32,
        bitmaps: Vec<u32>,
    },
    Fin {
        transfer_id: u64,
        sha256: [u8; 32],
    },
    Ok {
        transfer_id: u64,
    },
    Fail {
        transfer_id: u64,
        reason: u8,
    },
}

/// Binary (byte-mode) payload capacity at Quartile ECC, matching what
/// `qrcode` will actually encode, minus a few bytes of headroom so a full
/// DATA envelope cannot miss the encoder's bit budget.
pub fn qr_byte_capacity(version: u8) -> usize {
    let raw: usize = match version {
        10 => 151,
        15 => 292,
        20 => 482,
        25 => 715,
        30 => 982,
        40 => 1663,
        _ => 0,
    };
    raw.saturating_sub(8)
}

/// Logical chunks one DATA frame carries at `version`. The chunk itself
/// never changes size; a bigger code only carries more of them.
pub fn chunks_per_frame(version: u8) -> u32 {
    let room = qr_byte_capacity(version).saturating_sub(ENVELOPE_OVERHEAD + DATA_HEADER);
    (room / CHUNK_BYTES).min(usize::from(u8::MAX)) as u32
}

/// Window bitmaps one progress report carries at `version`.
pub fn progress_windows(version: u8) -> usize {
    (qr_byte_capacity(version).saturating_sub(ENVELOPE_OVERHEAD + PROGRESS_HEADER) / 4)
        .min(MAX_PROGRESS_WINDOWS)
}

/// The version byte of an `AX` envelope from a different protocol version,
/// so a mismatched peer can be named instead of silently ignored.
pub fn foreign_version(bytes: &[u8]) -> Option<u8> {
    if bytes.len() >= ENVELOPE_OVERHEAD && bytes.starts_with(b"AX") && bytes[2] != PROTOCOL_VERSION {
        Some(bytes[2])
    } else {
        None
    }
}

/// The error this side reports when the peer sends FAIL with `reason`.
pub fn fail_error(reason: u8) -> Error {
    match reason {
        FAIL_HASH => Error::HashMismatch,
        FAIL_DISK => Error::Refused("it cannot store the archive (disk full or too large)"),
        FAIL_ABORTED => Error::Refused("the destination already exists (run recv with --force)"),
        _ => Error::HandshakeFailed,
    }
}

fn kind_of(p: &Payload) -> u8 {
    match p {
        Payload::Hello { .. } => Kind::Hello.as_u8(),
        Payload::Probe { .. } => Kind::Probe.as_u8(),
        Payload::Link { .. } => Kind::Link.as_u8(),
        Payload::Go(_) => Kind::Go.as_u8(),
        Payload::Data { .. } => Kind::Data.as_u8(),
        Payload::Progress { .. } => Kind::Progress.as_u8(),
        Payload::Fin { .. } => Kind::Fin.as_u8(),
        Payload::Ok { .. } => Kind::Ok.as_u8(),
        Payload::Fail { .. } => Kind::Fail.as_u8(),
    }
}

fn put_payload(p: &Payload, out: &mut Vec<u8>) {
    match p {
        Payload::Hello { protocol_ver, role } => {
            out.push(*protocol_ver);
            out.push(*role);
        }
        Payload::Probe {
            id,
            qr_version,
            dwell_ms,
            last,
        } => {
            out.push(*id);
            out.push(*qr_version);
            out.extend_from_slice(&dwell_ms.to_be_bytes());
            out.push(u8::from(*last));
        }
        Payload::Link {
            qr_version,
            dwell_ms,
            probe_loss,
        } => {
            out.push(*qr_version);
            out.extend_from_slice(&dwell_ms.to_be_bytes());
            out.push(*probe_loss);
        }
        Payload::Go(offer) => {
            let name = offer.basename.as_bytes();
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&offer.uncompressed_hint.to_be_bytes());
            out.extend_from_slice(&offer.compressed_size.to_be_bytes());
            out.extend_from_slice(&offer.chunk_count.to_be_bytes());
            out.extend_from_slice(&offer.sha256);
            out.extend_from_slice(&offer.transfer_id.to_be_bytes());
            out.extend_from_slice(&offer.sender_nonce.to_be_bytes());
        }
        Payload::Data {
            transfer_id,
            first,
            count,
            bytes,
        } => {
            out.extend_from_slice(&transfer_id.to_be_bytes());
            out.extend_from_slice(&first.to_be_bytes());
            out.push(*count);
            out.extend_from_slice(bytes);
        }
        Payload::Progress {
            transfer_id,
            serial,
            contiguous,
            bitmaps,
        } => {
            out.extend_from_slice(&transfer_id.to_be_bytes());
            out.extend_from_slice(&serial.to_be_bytes());
            out.extend_from_slice(&contiguous.to_be_bytes());
            out.push(bitmaps.len() as u8);
            for bits in bitmaps {
                out.extend_from_slice(&bits.to_be_bytes());
            }
        }
        Payload::Fin {
            transfer_id,
            sha256,
        } => {
            out.extend_from_slice(&transfer_id.to_be_bytes());
            out.extend_from_slice(sha256);
        }
        Payload::Ok { transfer_id } => out.extend_from_slice(&transfer_id.to_be_bytes()),
        Payload::Fail {
            transfer_id,
            reason,
        } => {
            out.extend_from_slice(&transfer_id.to_be_bytes());
            out.push(*reason);
        }
    }
}

pub fn encode(p: &Payload) -> Result<Vec<u8>> {
    if matches!(p, Payload::Progress { bitmaps, .. } if bitmaps.len() > MAX_PROGRESS_WINDOWS) {
        return Err(Error::BadFrame);
    }
    let mut inner = Vec::new();
    put_payload(p, &mut inner);
    if inner.len() > u16::MAX as usize {
        return Err(Error::BadFrame);
    }
    let mut out = Vec::with_capacity(ENVELOPE_OVERHEAD + inner.len());
    out.extend_from_slice(b"AX");
    out.push(PROTOCOL_VERSION);
    out.push(kind_of(p));
    out.extend_from_slice(&(inner.len() as u16).to_be_bytes());
    out.extend_from_slice(&inner);
    let mut hasher = Hasher::new();
    hasher.update(&out);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    Ok(out)
}

fn u16_at(p: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([p[o], p[o + 1]])
}

fn u32_at(p: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(p[o..o + 4].try_into().expect("four bytes"))
}

fn u64_at(p: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(p[o..o + 8].try_into().expect("eight bytes"))
}

pub fn decode(bytes: &[u8]) -> Result<Payload> {
    if bytes.len() < ENVELOPE_OVERHEAD || &bytes[0..2] != b"AX" {
        return Err(Error::BadFrame);
    }
    let ver = bytes[2];
    let kind = bytes[3];
    let len = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    if ver != PROTOCOL_VERSION {
        return Err(Error::BadFrame);
    }
    let crc_off = 6 + len;
    if bytes.len() != crc_off + 4 {
        return Err(Error::BadFrame);
    }
    let mut hasher = Hasher::new();
    hasher.update(&bytes[..crc_off]);
    let got = u32::from_be_bytes(bytes[crc_off..].try_into().unwrap());
    if hasher.finalize() != got {
        return Err(Error::BadFrame);
    }
    let p = &bytes[6..crc_off];
    let payload = match kind {
        KIND_HELLO if p.len() == 2 => Payload::Hello {
            protocol_ver: p[0],
            role: p[1],
        },
        KIND_PROBE if p.len() == 5 => Payload::Probe {
            id: p[0],
            qr_version: p[1],
            dwell_ms: u16_at(p, 2),
            last: p[4] == 1,
        },
        KIND_LINK if p.len() == 4 => Payload::Link {
            qr_version: p[0],
            dwell_ms: u16_at(p, 1),
            probe_loss: p[3],
        },
        KIND_GO if p.len() >= GO_FIXED => {
            let n = usize::from(u16_at(p, 0));
            if p.len() != GO_FIXED + n {
                return Err(Error::BadFrame);
            }
            let basename = String::from_utf8(p[2..2 + n].to_vec()).map_err(|_| Error::BadFrame)?;
            let o = 2 + n;
            let mut sha256 = [0u8; 32];
            sha256.copy_from_slice(&p[o + 20..o + 52]);
            Payload::Go(Offer {
                transfer_id: u64_at(p, o + 52),
                sender_nonce: u64_at(p, o + 60),
                basename,
                uncompressed_hint: u64_at(p, o),
                compressed_size: u64_at(p, o + 8),
                chunk_count: u32_at(p, o + 16),
                sha256,
            })
        }
        KIND_DATA if p.len() >= DATA_HEADER => {
            let count = p[12];
            let body = &p[DATA_HEADER..];
            let most = usize::from(count) * CHUNK_BYTES;
            // Every chunk but the last is full, so the span is longer than
            // `count - 1` chunks and no longer than `count`.
            if count == 0 || body.len() > most || body.len() <= most - CHUNK_BYTES {
                return Err(Error::BadFrame);
            }
            Payload::Data {
                transfer_id: u64_at(p, 0),
                first: u32_at(p, 8),
                count,
                bytes: body.to_vec(),
            }
        }
        KIND_PROGRESS if p.len() >= PROGRESS_HEADER => {
            let n = usize::from(p[14]);
            if n > MAX_PROGRESS_WINDOWS || p.len() != PROGRESS_HEADER + 4 * n {
                return Err(Error::BadFrame);
            }
            Payload::Progress {
                transfer_id: u64_at(p, 0),
                serial: u16_at(p, 8),
                contiguous: u32_at(p, 10),
                bitmaps: (0..n).map(|i| u32_at(p, PROGRESS_HEADER + 4 * i)).collect(),
            }
        }
        KIND_FIN if p.len() == 40 => {
            let mut sha256 = [0u8; 32];
            sha256.copy_from_slice(&p[8..40]);
            Payload::Fin {
                transfer_id: u64_at(p, 0),
                sha256,
            }
        }
        KIND_OK if p.len() == 8 => Payload::Ok {
            transfer_id: u64_at(p, 0),
        },
        KIND_FAIL if p.len() == 9 => Payload::Fail {
            transfer_id: u64_at(p, 0),
            reason: p[8],
        },
        _ => return Err(Error::BadFrame),
    };
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr::SUPPORTED_VERSIONS;

    fn roundtrip(p: Payload) {
        let b = encode(&p).unwrap();
        assert_eq!(decode(&b).unwrap(), p);
    }

    fn offer() -> Offer {
        Offer::new("project", 1000, 400, [7u8; 32], 42).unwrap()
    }

    #[test]
    fn every_kind_roundtrips() {
        roundtrip(Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        });
        roundtrip(Payload::Probe {
            id: 5,
            qr_version: 40,
            dwell_ms: 150,
            last: true,
        });
        roundtrip(Payload::Link {
            qr_version: 25,
            dwell_ms: 200,
            probe_loss: 33,
        });
        roundtrip(Payload::Go(offer()));
        roundtrip(Payload::Data {
            transfer_id: 9,
            first: 99,
            count: 2,
            bytes: vec![1; 150],
        });
        roundtrip(Payload::Progress {
            transfer_id: 9,
            serial: 7,
            contiguous: 32,
            bitmaps: vec![0x8000_0001, 7],
        });
        roundtrip(Payload::Fin {
            transfer_id: 9,
            sha256: [9u8; 32],
        });
        roundtrip(Payload::Ok { transfer_id: 9 });
        roundtrip(Payload::Fail {
            transfer_id: 9,
            reason: FAIL_HASH,
        });
    }

    #[test]
    fn drops_truncated_bad_magic_bad_crc_unknown_kind() {
        assert!(decode(&[]).is_err());
        assert!(decode(b"NO").is_err());
        let mut good = encode(&Payload::Ok { transfer_id: 1 }).unwrap();
        let last = good.len() - 1;
        good[last] ^= 0xff;
        assert!(matches!(decode(&good), Err(crate::Error::BadFrame)));
        good = encode(&Payload::Ok { transfer_id: 1 }).unwrap();
        good[3] = 99;
        assert!(decode(&good).is_err());
    }

    #[test]
    fn data_spans_must_be_whole_chunks_but_the_last() {
        let data = |count: u8, len: usize| {
            encode(&Payload::Data {
                transfer_id: 1,
                first: 0,
                count,
                bytes: vec![0; len],
            })
            .unwrap()
        };
        assert!(decode(&data(0, 0)).is_err(), "no chunks");
        assert!(decode(&data(1, 0)).is_err(), "an empty chunk");
        assert!(decode(&data(1, 97)).is_err(), "more than one chunk");
        assert!(decode(&data(2, 96)).is_err(), "a missing second chunk");
        assert!(decode(&data(2, 97)).is_ok(), "a short last chunk");
    }

    #[test]
    fn progress_reports_cannot_exceed_the_window_cap() {
        let too_many = Payload::Progress {
            transfer_id: 1,
            serial: 1,
            contiguous: 0,
            bitmaps: vec![0; MAX_PROGRESS_WINDOWS + 1],
        };
        assert!(encode(&too_many).is_err());
    }

    #[test]
    fn bigger_codes_carry_more_chunks_but_the_chunk_never_changes() {
        assert_eq!(SUPPORTED_VERSIONS.map(chunks_per_frame), [1, 2, 4, 7, 9, 17]);
        assert_eq!(progress_windows(10), 29);
        assert_eq!(progress_windows(40), MAX_PROGRESS_WINDOWS);
    }

    #[test]
    fn capacity_is_unchanged_from_v1() {
        assert_eq!(qr_byte_capacity(10), 143);
        assert_eq!(qr_byte_capacity(15), 284);
        assert_eq!(qr_byte_capacity(20), 474);
        assert_eq!(qr_byte_capacity(25), 707);
        assert_eq!(qr_byte_capacity(30), 974);
        assert_eq!(qr_byte_capacity(40), 1655);
    }

    #[test]
    fn an_envelope_from_another_protocol_version_is_named_not_decoded() {
        let mut v1 = encode(&Payload::Ok { transfer_id: 1 }).unwrap();
        v1[2] = 1;
        assert!(decode(&v1).is_err());
        assert_eq!(foreign_version(&v1), Some(1));
        assert_eq!(foreign_version(&encode(&Payload::Ok { transfer_id: 1 }).unwrap()), None);
        assert_eq!(foreign_version(b"junk"), None);
    }

    #[test]
    fn fail_reasons_map_to_errors() {
        assert!(matches!(fail_error(FAIL_HASH), Error::HashMismatch));
        assert!(matches!(fail_error(FAIL_DISK), Error::Refused(_)));
        assert!(matches!(fail_error(FAIL_ABORTED), Error::Refused(_)));
        assert!(matches!(fail_error(FAIL_PROTOCOL), Error::HandshakeFailed));
    }
}
```

- [ ] **Step 3: QR capacity preflight (`src/qr.rs`)**

1. Change `use crate::{frame, Error, Result};` to `use crate::{chunks::CHUNK_BYTES, frame, Error, Result};`.
2. Replace the whole `ensure_full_data_frame_fits` function (lines 59-77) with:

```rust
fn ensure_fits(bytes: &[u8], version: u8) -> Result<()> {
    let cap = frame::qr_byte_capacity(version);
    if bytes.len() > cap {
        return Err(too_long(version, bytes.len(), &format!("max {cap}")));
    }
    let mut bits = Bits::new(Version::Normal(version as i16));
    bits.push_byte_data(bytes)
        .map_err(|_| too_long(version, bytes.len(), "byte mode"))?;
    bits.push_terminator(EcLevel::Q)
        .map_err(|_| too_long(version, bytes.len(), "terminator"))?;
    Ok(())
}

/// Checks that the largest DATA span and the largest progress report both
/// fit `version` under byte mode.
///
/// Call this after the link is locked and before GO, so a capacity mistake
/// fails the handshake instead of crashing a side mid-transfer.
pub fn ensure_full_data_frame_fits(version: u8) -> Result<()> {
    let count = frame::chunks_per_frame(version);
    if count == 0 {
        let need = frame::ENVELOPE_OVERHEAD + frame::DATA_HEADER + CHUNK_BYTES;
        return Err(too_long(version, need, "no room for one chunk"));
    }
    let data = frame::Payload::Data {
        transfer_id: u64::MAX,
        first: u32::MAX - count,
        count: count as u8,
        bytes: vec![0xA5; count as usize * CHUNK_BYTES],
    };
    let progress = frame::Payload::Progress {
        transfer_id: u64::MAX,
        serial: u16::MAX,
        contiguous: u32::MAX,
        bitmaps: vec![u32::MAX; frame::progress_windows(version)],
    };
    ensure_fits(&frame::encode(&data)?, version)?;
    ensure_fits(&frame::encode(&progress)?, version)
}
```

3. In the tests:
   - In `a_code_in_a_wide_frame_decodes_like_the_camera_sees_it`, change `frame::Payload::Ok` to `frame::Payload::Ok { transfer_id: 7 }`.
   - In `qr_roundtrip_binary_envelope`, change the payload to `frame::Payload::Data { transfer_id: 1, first: 1, count: 1, bytes: (0u8..=80).collect() }`.
   - In `decode_all_finds_two_codes_on_one_canvas`, use `frame::Payload::Ok { transfer_id: 1 }` and `frame::Payload::Fin { transfer_id: 1, sha256: [9; 32] }`.
   - Replace the loop body of `a_full_data_chunk_fits_every_supported_version` with:

```rust
        for version in SUPPORTED_VERSIONS {
            let cap = frame::qr_byte_capacity(version);
            let count = frame::chunks_per_frame(version);
            assert!(count >= 1, "v{version} must carry at least one chunk");
            let bytes = frame::encode(&frame::Payload::Data {
                transfer_id: u64::MAX,
                first: 0,
                count: count as u8,
                bytes: vec![0xA5u8; count as usize * CHUNK_BYTES],
            })
            .unwrap();
            assert!(bytes.len() <= cap, "v{version}: DATA envelope {} > capacity {cap}", bytes.len());
            encode_version(&bytes, version)
                .unwrap_or_else(|err| panic!("v{version} full DATA span must encode, got {err}"));
            encode_version(&vec![0xFFu8; cap], version)
                .unwrap_or_else(|err| panic!("v{version} capacity {cap} must encode, got {err}"));
            ensure_full_data_frame_fits(version)
                .unwrap_or_else(|err| panic!("v{version} handshake preflight must pass, got {err}"));
            assert!(
                encode_version(&vec![0xFFu8; cap + 1], version).is_err(),
                "v{version} must reject capacity+1"
            );
        }
```

- [ ] **Step 4: `src/optical.rs`**

Replace the `Lossy` struct and its `impl` with:

```rust
pub struct Lossy<T> {
    pub inner: T,
    /// DATA spans to lose once each, by first chunk.
    pub drop_first: HashSet<u32>,
}

impl<T: Optical> Optical for Lossy<T> {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        self.inner.show(payload)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        let payload = self.inner.poll(timeout)?;
        if let Some(Payload::Data { first, .. }) = payload.as_ref() {
            if self.drop_first.remove(first) {
                return Ok(None);
            }
        }
        Ok(payload)
    }
}
```

In the tests:
- In `pair_delivers_hello`, replace `protocol_ver: 1` with `protocol_ver: crate::frame::PROTOCOL_VERSION`.
- Rename `lossy_drops_configured_data_seq` to `lossy_drops_configured_span`, and use:
  - `drop_first: [3].into_iter().collect()`
  - `Payload::Data { transfer_id: 1, first: 3, count: 1, bytes: vec![1] }`, then `Payload::Data { transfer_id: 1, first: 4, count: 1, bytes: vec![2] }`
  - the match arm `Some(Payload::Data { first, .. }) => assert_eq!(first, 4)`

- [ ] **Step 5: Rewrite `src/transport.rs`**

Replace the whole file with:

```rust
//! Moving the archive once the link is up.
//!
//! The sender pages DATA frames, each packing as many *adjacent* missing
//! 96-byte chunks as the negotiated QR version holds, and after every burst
//! waits for a progress report. The receiver assembles chunks into a
//! [`ChunkStore`] that outlives this attempt: a stall here costs time, never
//! data, because the next attempt resumes from whatever the store holds.

use std::{
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    chunks::{plan_spans, report_limit, span_bytes, ChunkSet, ChunkStore, WINDOW_CHUNKS},
    frame::{chunks_per_frame, fail_error, progress_windows, Payload, FAIL_HASH, FAIL_PROTOCOL},
    link::Session,
    optical::Optical,
    resume::{Offer, UNSCOPED},
    Error, Result,
};

/// DATA frames painted between progress reports.
pub const BURST_FRAMES: usize = 32;
/// Rounds in a row without a new chunk before this attempt gives up and
/// hands the link back to the fallback ladder.
const STALL_LIMIT: u32 = 20;
/// Missing chunk numbers quoted in a stall error.
const STALL_SAMPLE: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportConfig {
    pub fast: bool,
}

impl TransportConfig {
    /// Millisecond timeouts, for in-process tests.
    pub fn fast() -> Self {
        Self { fast: true }
    }
}

fn ack_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        // Must outlast the receiver's post-burst quiet wait (1.5× dwell, up to
        // ~1.8s) plus the time to encode and paint the progress QR.
        Duration::from_secs(5)
    }
}

/// How long the receiver waits for another *new* chunk before concluding the
/// sender has finished the burst and wants a progress report.
fn burst_quiet(cfg: TransportConfig, dwell_ms: u16) -> Duration {
    if cfg.fast {
        Duration::ZERO
    } else {
        Duration::from_millis((u64::from(dwell_ms) * 3 / 2).max(80))
    }
}

/// Live transfers never treat "no DATA yet" as a stall: the sender may still
/// be painting GO. Fast tests keep a bound so a silent peer cannot hang them.
fn first_data_budget(cfg: TransportConfig) -> Option<Duration> {
    if cfg.fast {
        Some(ack_timeout(cfg) * STALL_LIMIT)
    } else {
        None
    }
}

/// How long the sender waits for OK after FIN. The receiver writes, syncs
/// and unpacks the archive before answering, which for a large tree takes a
/// while; the sender must not give up and report failure meanwhile.
fn fin_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(120)
    }
}

/// Least time between unprompted progress reports while only duplicates
/// arrive (the sender is resending because it missed a report).
fn report_refresh(cfg: TransportConfig) -> Duration {
    ack_timeout(cfg) / 2
}

fn status_line(have: &ChunkSet, new_this_attempt: u32, elapsed: Duration) -> String {
    let rate = f64::from(new_this_attempt) / elapsed.as_secs_f64().max(0.001);
    format!(
        "{}/{} chunks · next hole #{} · {rate:.1} chunks/s",
        have.count(),
        have.total(),
        have.contiguous()
    )
}

fn missing_sample(have: &ChunkSet) -> Vec<u32> {
    (have.contiguous()..have.total())
        .filter(|index| !have.contains(*index))
        .take(STALL_SAMPLE)
        .collect()
}

/// HELLO / PROBE / LINK mean the peer gave up on this attempt and started
/// another handshake. The payload goes back to the caller so that handshake
/// can still read it.
fn is_handshake_restart(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::Hello { .. } | Payload::Probe { .. } | Payload::Link { .. }
    )
}

/// Whether a FAIL addressed to `transfer_id` concerns `offer`. Unscoped FAILs
/// come from a peer whose display broke before it knew any offer.
fn concerns(offer: &Offer, transfer_id: u64) -> bool {
    transfer_id == offer.transfer_id || transfer_id == UNSCOPED
}

/// The receiver's next progress report for `offer`.
pub fn progress_payload(offer: &Offer, store: &mut ChunkStore, windows: usize) -> Payload {
    let (contiguous, bitmaps) = store.have().report(windows);
    Payload::Progress {
        transfer_id: offer.transfer_id,
        serial: store.next_serial(),
        contiguous,
        bitmaps,
    }
}

fn show_spans(
    opt: &mut impl Optical,
    blob: &[u8],
    offer: &Offer,
    spans: &[(u32, u32)],
    cfg: TransportConfig,
    dwell_ms: u16,
) -> Result<()> {
    let tiles = opt.tile_count().max(1);
    for batch in spans.chunks(tiles) {
        let payloads: Vec<Payload> = batch
            .iter()
            .map(|&(first, count)| {
                let range = span_bytes(offer.compressed_size, first, count)
                    .expect("planned spans lie inside the archive");
                Payload::Data {
                    transfer_id: offer.transfer_id,
                    first,
                    count: count as u8,
                    bytes: blob[range].to_vec(),
                }
            })
            .collect();
        opt.show_many(&payloads)?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }
    Ok(())
}

/// Waits up to `timeout` for a progress report not seen before and folds it
/// into `acked`.
fn await_progress(
    opt: &mut impl Optical,
    offer: &Offer,
    acked: &mut ChunkSet,
    last_serial: &mut Option<u16>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match opt.poll(remaining)? {
            Some(Payload::Progress {
                transfer_id,
                serial,
                contiguous,
                bitmaps,
            }) if transfer_id == offer.transfer_id && *last_serial != Some(serial) => {
                *last_serial = Some(serial);
                if acked.apply_report(contiguous, &bitmaps).is_some() {
                    return Ok(());
                }
            }
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => {}
            None => break,
        }
    }
    Ok(())
}

/// Sends every chunk `acked` does not already cover, then FIN, and returns
/// once the receiver says OK. `acked` is updated in place, so a caller can
/// tell whether this attempt moved anything even when it fails.
pub fn send_blob(
    opt: &mut impl Optical,
    session: &Session,
    blob: &[u8],
    acked: &mut ChunkSet,
    cfg: TransportConfig,
) -> Result<()> {
    let offer = &session.offer;
    let per_frame = chunks_per_frame(session.qr_version);
    if per_frame == 0 || blob.len() as u64 != offer.compressed_size || acked.total() != offer.chunk_count {
        return Err(Error::HandshakeFailed);
    }
    let windows = progress_windows(session.qr_version);
    let start = Instant::now();
    let held_at_start = acked.count();
    let mut last_serial = None;
    let mut stalls = 0;
    while !acked.is_complete() {
        let limit = report_limit(acked.contiguous(), windows, acked.total());
        let spans = plan_spans(acked, per_frame, limit, BURST_FRAMES);
        opt.set_status(&status_line(acked, acked.count() - held_at_start, start.elapsed()));
        show_spans(opt, blob, offer, &spans, cfg, session.dwell_ms)?;

        let before = acked.count();
        await_progress(opt, offer, acked, &mut last_serial, ack_timeout(cfg))?;
        if acked.count() > before {
            stalls = 0;
        } else {
            stalls += 1;
            if stalls >= STALL_LIMIT {
                return Err(Error::Stalled(missing_sample(acked)));
            }
        }
    }

    opt.set_status("all chunks acknowledged; waiting for the receiver to verify, sync and unpack");
    opt.show(&Payload::Fin {
        transfer_id: offer.transfer_id,
        sha256: offer.sha256,
    })?;
    let deadline = Instant::now() + fin_timeout(cfg);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match opt.poll(remaining)? {
            Some(Payload::Ok { transfer_id }) if transfer_id == offer.transfer_id => return Ok(()),
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => {}
            None => break,
        }
    }
    Err(Error::HandshakeFailed)
}

/// Assembles the archive into `store` until FIN arrives with every chunk
/// held and the SHA-256 matching, and returns a copy of it.
///
/// Never shows OK: the caller must persist, sync and unpack first (see
/// `crate::deliver`). On any error the chunks already in `store` stay there
/// for the next attempt.
pub fn recv_blob(
    opt: &mut impl Optical,
    session: &Session,
    store: &mut ChunkStore,
    cfg: TransportConfig,
) -> Result<Vec<u8>> {
    let offer = &session.offer;
    if store.size() != offer.compressed_size || store.have().total() != offer.chunk_count {
        return Err(Error::HandshakeFailed);
    }
    let windows = progress_windows(session.qr_version);
    let quiet = burst_quiet(cfg, session.dwell_ms);
    let start = Instant::now();
    let held_at_start = store.have().count();
    let mut last_new = Instant::now();
    let mut last_report = Instant::now();
    let mut unreported = false;
    let mut seen_data = false;
    let mut idle_rounds = 0;
    opt.set_status(&format!(
        "holding {held_at_start}/{} chunks; waiting for DATA",
        offer.chunk_count
    ));

    loop {
        let poll_for = if unreported && !cfg.fast {
            quiet
                .saturating_sub(last_new.elapsed())
                .max(Duration::from_millis(5))
        } else {
            ack_timeout(cfg)
        };

        let report_now = match opt.poll(poll_for)? {
            Some(Payload::Data {
                transfer_id,
                first,
                count,
                bytes,
            }) if transfer_id == offer.transfer_id => {
                seen_data = true;
                idle_rounds = 0;
                // The frame CRC passed and the id matches, so a span that
                // does not fit the offer is a protocol bug, not line noise.
                let accepted = store
                    .accept(first, u32::from(count), &bytes)
                    .map_err(|_| Error::HandshakeFailed)?;
                if accepted.conflict > 0 {
                    opt.log(&format!(
                        "kept the first copy of {} chunk(s) near #{first} that arrived again with different bytes",
                        accepted.conflict
                    ));
                }
                if accepted.new > 0 {
                    last_new = Instant::now();
                    unreported = true;
                    opt.set_status(&status_line(
                        store.have(),
                        store.have().count() - held_at_start,
                        start.elapsed(),
                    ));
                    let last = first + u32::from(count) - 1;
                    store.is_complete()
                        || store.have().window_bits(last / WINDOW_CHUNKS) == u32::MAX
                } else {
                    // Only repeats: the sender is resending what it thinks is
                    // missing, so it probably missed the last report.
                    !unreported && last_report.elapsed() >= quiet.max(report_refresh(cfg))
                }
            }
            // Another transfer's traffic, e.g. from before a restart.
            Some(Payload::Data { .. }) => false,
            Some(Payload::Fin {
                transfer_id,
                sha256,
            }) if transfer_id == offer.transfer_id => {
                if !store.is_complete() {
                    true
                } else {
                    if sha256 != offer.sha256 {
                        opt.show(&Payload::Fail {
                            transfer_id,
                            reason: FAIL_PROTOCOL,
                        })?;
                        return Err(Error::HandshakeFailed);
                    }
                    let digest: [u8; 32] = Sha256::digest(store.blob()).into();
                    if digest != offer.sha256 {
                        opt.show(&Payload::Fail {
                            transfer_id,
                            reason: FAIL_HASH,
                        })?;
                        return Err(Error::HashMismatch);
                    }
                    return Ok(store.blob().to_vec());
                }
            }
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => unreported && last_new.elapsed() >= quiet,
            None => {
                if unreported && last_new.elapsed() >= quiet {
                    true
                } else if !seen_data {
                    if first_data_budget(cfg).is_some_and(|budget| start.elapsed() >= budget) {
                        return Err(Error::Stalled(missing_sample(store.have())));
                    }
                    false
                } else {
                    idle_rounds += 1;
                    if idle_rounds >= STALL_LIMIT {
                        return Err(Error::Stalled(missing_sample(store.have())));
                    }
                    // A whole timeout with nothing new: repaint the report so
                    // a reconnecting sender, or one that missed the last
                    // report, has a fresh one to act on.
                    true
                }
            }
        };

        if report_now {
            opt.show(&progress_payload(offer, store, windows))?;
            unreported = false;
            last_report = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frame::{PROTOCOL_VERSION, ROLE_RECV},
        optical::{pair, Lossy},
    };
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    fn offer_for(blob: &[u8]) -> Offer {
        Offer::new("dir", blob.len() as u64, blob.len() as u64, Sha256::digest(blob).into(), 1)
            .unwrap()
    }

    fn session_for(blob: &[u8], version: u8) -> Session {
        Session {
            qr_version: version,
            dwell_ms: 1,
            offer: offer_for(blob),
        }
    }

    fn ramp(len: u32) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    /// Runs both ends over `s`/`r`. The receiver shows OK once `recv_blob`
    /// returns, standing in for `deliver`.
    fn transfer<S, R>(mut s: S, mut r: R, session: &Session, blob: &[u8]) -> (Result<()>, Result<Vec<u8>>, ChunkStore)
    where
        S: Optical + Send + 'static,
        R: Optical,
    {
        let cfg = TransportConfig::fast();
        let sender = thread::spawn({
            let session = session.clone();
            let blob = blob.to_vec();
            move || {
                let mut acked = ChunkSet::new(session.offer.chunk_count);
                send_blob(&mut s, &session, &blob, &mut acked, cfg)
            }
        });
        let mut store = ChunkStore::new(session.offer.compressed_size).unwrap();
        let got = recv_blob(&mut r, session, &mut store, cfg);
        if got.is_ok() {
            r.show(&Payload::Ok {
                transfer_id: session.offer.transfer_id,
            })
            .unwrap();
        }
        (sender.join().unwrap(), got, store)
    }

    struct Recorder<T> {
        inner: T,
        shown: Arc<Mutex<Vec<Payload>>>,
    }

    impl<T: Optical> Optical for Recorder<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.lock().unwrap().push(payload.clone());
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    /// Loses the receiver's first "everything is here" report.
    struct DropTheCompletionReport<T> {
        inner: T,
        dropped: bool,
    }

    impl<T: Optical> Optical for DropTheCompletionReport<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Progress { bitmaps, .. } if bitmaps.is_empty()) && !self.dropped {
                self.dropped = true;
                return Ok(());
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    /// Drops anything the peer sent while this side was painting a frame —
    /// the live camera's behaviour while the UI thread is busy.
    struct DropInboundWhileShowing<T> {
        inner: T,
    }

    impl<T: Optical> Optical for DropInboundWhileShowing<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            while matches!(self.inner.poll(Duration::ZERO), Ok(Some(_))) {}
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    fn assert_delivered((sent, got, _): (Result<()>, Result<Vec<u8>>, ChunkStore), blob: &[u8]) {
        sent.unwrap();
        assert_eq!(got.unwrap(), blob);
    }

    #[test]
    fn transfer_zero_loss() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn the_first_burst_packs_adjacent_chunks_into_each_frame() {
        let blob = ramp(3000);
        let session = session_for(&blob, 40);
        assert_eq!(session.offer.chunk_count, 32);
        let (s, r) = pair();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            inner: s,
            shown: Arc::clone(&shown),
        };
        assert_delivered(transfer(recorder, r, &session, &blob), &blob);
        let spans: Vec<(u32, u8)> = shown
            .lock()
            .unwrap()
            .iter()
            .filter_map(|p| match p {
                Payload::Data { first, count, .. } => Some((*first, *count)),
                _ => None,
            })
            .collect();
        assert_eq!(&spans[..2], &[(0, 17), (17, 15)]);
    }

    #[test]
    fn a_dropped_span_is_sent_again() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = Lossy {
            inner: r,
            drop_first: [0].into_iter().collect(),
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn a_burst_of_drops_is_sent_again() {
        let blob = ramp(4000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = Lossy {
            inner: r,
            drop_first: (2..10).collect(),
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn transfers_span_many_progress_windows() {
        let blob = ramp(6401);
        let session = session_for(&blob, 10);
        assert!(session.offer.chunk_count > 2 * WINDOW_CHUNKS);
        let (s, r) = pair();
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn a_lost_completion_report_is_recovered() {
        let blob = ramp(6401);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = DropTheCompletionReport {
            inner: r,
            dropped: false,
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn reporting_progress_must_not_drop_the_rest_of_the_burst() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = DropInboundWhileShowing { inner: r };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn duplicates_and_conflicts_never_override_accepted_bytes() {
        let good: Vec<u8> = (0..96).collect();
        let session = session_for(&good, 10);
        let tid = session.offer.transfer_id;
        let (mut s, mut r) = pair();
        for bytes in [good.clone(), good.clone(), vec![0xEE; 96]] {
            s.show(&Payload::Data {
                transfer_id: tid,
                first: 0,
                count: 1,
                bytes,
            })
            .unwrap();
        }
        s.show(&Payload::Fin {
            transfer_id: tid,
            sha256: session.offer.sha256,
        })
        .unwrap();
        let mut store = ChunkStore::new(96).unwrap();
        let got = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap();
        assert_eq!(got, good);
        assert_eq!(store.have().count(), 1, "repeats must not inflate progress");
        assert_eq!(store.conflicts(), 1);
    }

    #[test]
    fn traffic_for_another_transfer_is_ignored() {
        let good: Vec<u8> = (0..96).collect();
        let session = session_for(&good, 10);
        let tid = session.offer.transfer_id;
        let (mut s, mut r) = pair();
        s.show(&Payload::Data {
            transfer_id: tid ^ 1,
            first: 0,
            count: 1,
            bytes: vec![0xEE; 96],
        })
        .unwrap();
        s.show(&Payload::Data {
            transfer_id: tid,
            first: 0,
            count: 1,
            bytes: good.clone(),
        })
        .unwrap();
        s.show(&Payload::Fin {
            transfer_id: tid,
            sha256: session.offer.sha256,
        })
        .unwrap();
        let mut store = ChunkStore::new(96).unwrap();
        assert_eq!(recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap(), good);
        assert_eq!(store.conflicts(), 0);
    }

    #[test]
    fn recv_rejects_a_span_of_the_wrong_length() {
        let blob = ramp(160);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        s.show(&Payload::Data {
            transfer_id: session.offer.transfer_id,
            first: 0,
            count: 1,
            bytes: vec![1; 95],
        })
        .unwrap();
        let mut store = ChunkStore::new(160).unwrap();
        assert!(matches!(
            recv_blob(&mut r, &session, &mut store, TransportConfig::fast()),
            Err(Error::HandshakeFailed)
        ));
    }

    #[test]
    fn hash_mismatch_does_not_return_blob() {
        let blob = vec![1, 2, 3, 4];
        let session = Session {
            qr_version: 10,
            dwell_ms: 1,
            offer: Offer::new("dir", 4, 4, [0; 32], 1).unwrap(),
        };
        let (s, r) = pair();
        let (sent, got, _) = transfer(s, r, &session, &blob);
        assert!(matches!(got, Err(Error::HashMismatch)), "{got:?}");
        assert!(matches!(sent, Err(Error::HashMismatch)), "{sent:?}");
    }

    #[test]
    fn recv_blob_stops_when_the_sender_reports_fail() {
        let blob = ramp(10);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        s.show(&Payload::Fail {
            transfer_id: session.offer.transfer_id,
            reason: FAIL_PROTOCOL,
        })
        .unwrap();
        let mut store = ChunkStore::new(10).unwrap();
        let err = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap_err();
        assert!(matches!(err, Error::HandshakeFailed), "{err:?}");
    }

    #[test]
    fn a_handshake_start_seen_mid_transfer_is_handed_back_and_data_is_kept() {
        let blob = ramp(500);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        r.show(&Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        })
        .unwrap();
        let mut acked = ChunkSet::new(session.offer.chunk_count);
        let err = send_blob(&mut s, &session, &blob, &mut acked, TransportConfig::fast()).unwrap_err();
        assert!(
            matches!(&err, Error::Restart(p) if matches!(**p, Payload::Hello { .. })),
            "{err:?}"
        );

        let probe = Payload::Probe {
            id: 0,
            qr_version: 10,
            dwell_ms: 250,
            last: true,
        };
        s.show(&probe).unwrap();
        let mut store = ChunkStore::new(session.offer.compressed_size).unwrap();
        let err = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap_err();
        assert!(matches!(&err, Error::Restart(p) if **p == probe), "{err:?}");
        assert!(store.is_complete(), "chunks that arrived before the restart are kept");
    }

    #[test]
    fn recv_blob_gives_up_after_sustained_silence() {
        let blob = ramp(10);
        let session = session_for(&blob, 10);
        let (_s, mut r) = pair();
        let mut store = ChunkStore::new(10).unwrap();
        assert!(matches!(
            recv_blob(&mut r, &session, &mut store, TransportConfig::fast()),
            Err(Error::Stalled(_))
        ));
    }

    #[test]
    fn the_receiver_never_acknowledges_success_itself() {
        let blob = ramp(500);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            inner: r,
            shown: Arc::clone(&shown),
        };
        assert_delivered(transfer(s, recorder, &session, &blob), &blob);
        let shown = shown.lock().unwrap();
        let ok_at = shown
            .iter()
            .position(|p| matches!(p, Payload::Ok { .. }))
            .expect("the harness shows OK");
        assert_eq!(ok_at, shown.len() - 1, "recv_blob itself must never show OK");
    }
}
```

- [ ] **Step 6: `src/link.rs` (non-test code)**

1. Replace the `use crate::{...}` block with:

```rust
use crate::{
    chunks::ChunkSet,
    frame::{
        chunks_per_frame, fail_error, progress_windows, qr_byte_capacity, Payload, FAIL_ABORTED,
        FAIL_DISK, FAIL_PROTOCOL, PROTOCOL_VERSION, ROLE_RECV,
    },
    optical::Optical,
    pack::dest_exists,
    resume::{Offer, ReceiveCache, Reuse, UNSCOPED},
    transport::progress_payload,
    Error, Result,
};
```

2. In `is_retryable`, add `| Error::Restart(_)` after `| Error::HashMismatch`.

3. Replace the `Session` struct with:

```rust
/// What both sides agreed on for one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub qr_version: u8,
    pub dwell_ms: u16,
    pub offer: Offer,
}

/// The sender's side of a completed handshake: the session, plus what the
/// receiver reported it already holds.
#[derive(Clone, Debug)]
pub struct Linked {
    pub session: Session,
    pub acked: ChunkSet,
}
```

4. In `hello_timeout`, replace `Duration::from_millis(100)` with `Duration::from_secs(1)` and extend its comment with: *"It is still a full second: long enough to outlast the peer's fast-mode stall detection (20 rounds of 50 ms), so a test peer that is still giving up on its previous attempt is waited for rather than counted as another failure."*

5. Add this function directly above `run_send_handshake`:

```rust
fn offer_prompt(offer: &Offer, qr_version: u8) -> String {
    format!(
        "Linked at QR v{qr_version}. Send {} ({} B, {} chunks)? Enter sends, Esc aborts. The receiver accepts on its own",
        offer.basename, offer.compressed_size, offer.chunk_count
    )
}
```

6. Replace the whole `run_send_handshake` function with:

```rust
pub fn run_send_handshake(opt: &mut impl Optical, offer: &Offer, cfg: LinkConfig) -> Result<Linked> {
    let hello = poll_until(opt, hello_timeout(cfg), |payload| match payload {
        Payload::Hello { .. } => Some(()),
        _ => None,
    })?;
    if hello.is_none() {
        return Err(Error::HandshakeTimeout);
    }
    opt.log("receiver said HELLO");

    // Probes above what this terminal can draw would render clipped and
    // never decode, so they are dropped rather than offered.
    let probes: Vec<_> = PROBES
        .iter()
        .copied()
        .filter(|(_, qr_version, _)| *qr_version <= cfg.max_qr_version)
        .collect();
    let Some(&(last_id, _, _)) = probes.last() else {
        return Err(Error::NoUsableProbe);
    };
    for (id, qr_version, dwell_ms) in probes {
        opt.show(&Payload::Probe {
            id,
            qr_version,
            dwell_ms,
            last: id == last_id,
        })?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }

    let Some((qr_version, linked_dwell_ms)) =
        poll_until(opt, link_timeout(cfg), |payload| match payload {
            Payload::Link {
                qr_version,
                dwell_ms,
                ..
            } => Some((qr_version, dwell_ms)),
            _ => None,
        })?
    else {
        return Err(Error::HandshakeFailed);
    };
    let dwell_ms = linked_dwell_ms.max(cfg.dwell_floor_ms);
    if qr_version > cfg.max_qr_version {
        return Err(Error::NoUsableProbe);
    }

    // Lock the QR version before GO is shown so GO (and everything after
    // it) is encoded at the negotiated version instead of the probe default.
    opt.set_version(qr_version);
    opt.log(&format!("linked at QR v{qr_version}, {dwell_ms} ms dwell"));

    if chunks_per_frame(qr_version) == 0 {
        return Err(Error::HandshakeFailed);
    }
    crate::qr::ensure_full_data_frame_fits(qr_version)?;
    let go = Payload::Go(offer.clone());
    let go_len = crate::frame::encode(&go)?.len();
    let cap = qr_byte_capacity(qr_version);
    if go_len > cap {
        return Err(Error::Message(format!(
            "filename {:?} makes the offer {go_len} bytes; QR v{qr_version} holds {cap}",
            offer.basename
        )));
    }
    // Confirm here, not after the receiver has already accepted: the
    // receiver is still waiting for the offer, not timing DATA.
    if cfg.confirm_offer && !opt.gate(&offer_prompt(offer, qr_version))? {
        return Err(Error::Aborted);
    }
    opt.show(&go)?;

    // Acceptance is a progress report: empty for a new offer, or everything
    // already held when the receiver is resuming this exact offer.
    let reply = poll_until(opt, go_timeout(cfg), |payload| match payload {
        Payload::Progress {
            transfer_id,
            contiguous,
            bitmaps,
            ..
        } if transfer_id == offer.transfer_id => Some(Ok((contiguous, bitmaps))),
        Payload::Fail {
            transfer_id,
            reason,
        } if transfer_id == offer.transfer_id || transfer_id == UNSCOPED => {
            Some(Err(fail_error(reason)))
        }
        _ => None,
    })?;
    let Some(reply) = reply else {
        return Err(Error::HandshakeFailed);
    };
    let (contiguous, bitmaps) = reply?;
    let acked = ChunkSet::from_report(offer.chunk_count, contiguous, &bitmaps)
        .ok_or(Error::HandshakeFailed)?;
    if acked.count() == 0 {
        opt.log("receiver accepted the offer");
    } else {
        opt.log(&format!(
            "receiver already holds {}/{} chunks; resuming",
            acked.count(),
            offer.chunk_count
        ));
    }

    Ok(Linked {
        session: Session {
            qr_version,
            dwell_ms,
            offer: offer.clone(),
        },
        acked,
    })
}
```

7. In `run_recv_handshake`:
   - Change the signature to `pub fn run_recv_handshake(opt: &mut impl Optical, outdir: &Path, force: bool, cfg: LinkConfig, cache: &mut ReceiveCache) -> Result<Session>`.
   - Change `protocol_ver: 1,` to `protocol_ver: PROTOCOL_VERSION,`.
   - Replace everything from `let Some((basename, uncompressed_hint, compressed_size, chunk_count, sha256)) =` to the end of the function with:

```rust
    let Some(offer) = poll_until(opt, go_timeout(cfg), |payload| match payload {
        Payload::Go(offer) => Some(offer),
        _ => None,
    })?
    else {
        return Err(Error::HandshakeFailed);
    };

    if !offer.is_consistent() {
        opt.log("refusing an offer whose transfer id does not match its contents");
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_PROTOCOL,
        })?;
        return Err(Error::HandshakeFailed);
    }
    if dest_exists(outdir, &offer.basename) && !force {
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_ABORTED,
        })?;
        return Err(Error::DestExists(outdir.join(&offer.basename)));
    }
    let Some(reuse) = cache.adopt(&offer) else {
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_DISK,
        })?;
        return Err(Error::Message(format!(
            "{} B is too large to receive on this machine",
            offer.compressed_size
        )));
    };
    opt.log(&match reuse {
        Reuse::Fresh => format!(
            "offer: {}, {} B compressed, {} chunks",
            offer.basename, offer.compressed_size, offer.chunk_count
        ),
        Reuse::Resumed { held } => format!(
            "offer matches the interrupted transfer: resuming with {held}/{} chunks",
            offer.chunk_count
        ),
        Reuse::Replaced { dropped } => format!(
            "offer: {} replaces an interrupted transfer ({dropped} chunks discarded)",
            offer.basename
        ),
    });

    let store = cache.store(&offer).ok_or(Error::HandshakeFailed)?;
    // Accepting is reporting progress, so a resumed sender skips what is
    // already here.
    opt.show(&progress_payload(&offer, store, progress_windows(qr_version)))?;

    Ok(Session {
        qr_version,
        dwell_ms,
        offer,
    })
}
```

- [ ] **Step 7: `src/link.rs` tests**

Add to the `mod tests` imports: `use crate::resume::{Offer, ReceiveCache};` and `use std::path::Path;`. Add these helpers below `scratch_dir`:

```rust
    fn test_offer(size: u64) -> Offer {
        Offer::new("dir", 10, size, [7; 32], 1).unwrap()
    }

    fn fresh_recv(opt: &mut impl Optical, out: &Path, cfg: LinkConfig) -> Result<Session> {
        run_recv_handshake(opt, out, false, cfg, &mut ReceiveCache::default())
    }

    /// Replays a fixed script of payloads, then goes quiet.
    struct ScriptedPeer {
        script: std::collections::VecDeque<Payload>,
        shown: Vec<Payload>,
    }

    impl Optical for ScriptedPeer {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.push(payload.clone());
            Ok(())
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            match self.script.pop_front() {
                Some(payload) => Ok(Some(payload)),
                None => {
                    thread::sleep(timeout);
                    Ok(None)
                }
            }
        }
    }
```

Apply these mechanical edits across the existing tests:

| Old call | New call |
|---|---|
| `run_send_handshake(&mut X, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg)` | `run_send_handshake(&mut X, &test_offer(3), cfg).map(\|linked\| linked.session)` |
| `run_send_handshake(&mut X, "dir".into(), 10, &blob, sha256, cfg)` and `run_send_handshake(&mut X, "dir".into(), 5, &blob, sha256, cfg)` | `run_send_handshake(&mut X, &test_offer(5), cfg).map(\|linked\| linked.session)`, then delete the now-unused `blob`/`sha256` locals |
| The multi-line `run_send_handshake(&mut send_opt, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg)` in `declining_the_offer_sends_no_go_at_all` | `run_send_handshake(&mut send_opt, &test_offer(3), cfg)` |
| `run_recv_handshake(&mut X, PATH, false, cfg)` / `run_recv_handshake(&mut opt, &out, false, LinkConfig::fast())` / the `FailingPollOptical` and `UnknownProbeOptical` calls | Add a fifth argument `&mut ReceiveCache::default()` |

Then make these individual fixes:
- **`LastProbeOptical::poll`:** return `Ok(Some(Payload::Go(test_offer(1))))`.
- **`send_handshake_sets_version_before_go_is_shown`:** use `matches!(payload, Payload::Go(_))`.
- **`handshake_picks_highest_passing_probe`:** assert `s.offer.basename == "dir"` and `r.offer.chunk_count == 1`.
- **`the_sender_confirms_once_before_go_not_before_each_probe`:** assert `prompts[0].contains("Enter sends")`, and change its closure to `run_send_handshake(&mut send_opt, &test_offer(3), cfg).map(|linked| (linked.session, send_opt.prompts, send_opt.shown))`.

Append these new tests:

```rust
    fn held_cache(offer: &Offer, chunks: u32) -> ReceiveCache {
        let mut cache = ReceiveCache::default();
        cache.adopt(offer).unwrap();
        let bytes = vec![0u8; chunks as usize * crate::chunks::CHUNK_BYTES];
        cache.store(offer).unwrap().accept(0, chunks, &bytes).unwrap();
        cache
    }

    #[test]
    fn a_matching_offer_resumes_with_the_chunks_already_held() {
        let offer = test_offer(500);
        let mut cache = held_cache(&offer, 2);
        let (mut send_end, mut recv_end) = pair();
        let cfg = LinkConfig::fast();
        let sent = offer.clone();
        let send_t = thread::spawn(move || run_send_handshake(&mut send_end, &sent, cfg).unwrap());
        let session =
            run_recv_handshake(&mut recv_end, &scratch_dir("resume-match"), false, cfg, &mut cache)
                .unwrap();
        let linked = send_t.join().unwrap();
        assert_eq!(session.offer, offer);
        assert_eq!(linked.acked.count(), 2, "the sender must learn what is already held");
        assert!(linked.acked.contains(0) && linked.acked.contains(1));
        assert_eq!(cache.held(), 2);
    }

    #[test]
    fn an_offer_that_differs_in_any_field_starts_from_nothing() {
        let held = test_offer(500);
        for other in [
            Offer::new("dir", 10, 500, [8; 32], 1).unwrap(),
            Offer::new("dir", 10, 500, [7; 32], 2).unwrap(),
            Offer::new("other", 10, 500, [7; 32], 1).unwrap(),
            Offer::new("dir", 11, 500, [7; 32], 1).unwrap(),
        ] {
            let mut cache = held_cache(&held, 2);
            let (mut send_end, mut recv_end) = pair();
            let cfg = LinkConfig::fast();
            let sent = other.clone();
            let send_t = thread::spawn(move || run_send_handshake(&mut send_end, &sent, cfg).unwrap());
            run_recv_handshake(&mut recv_end, &scratch_dir("resume-other"), false, cfg, &mut cache)
                .unwrap();
            let linked = send_t.join().unwrap();
            assert_eq!(linked.acked.count(), 0, "{other:?} must not inherit chunks");
            assert_eq!(cache.held(), 0);
        }
    }

    #[test]
    fn an_inconsistent_offer_is_refused_before_touching_the_cache() {
        let held = test_offer(500);
        let mut forged = test_offer(500);
        forged.transfer_id ^= 1;
        let mut cache = held_cache(&held, 2);
        let mut peer = ScriptedPeer {
            script: [
                Payload::Probe {
                    id: 0,
                    qr_version: 10,
                    dwell_ms: 250,
                    last: true,
                },
                Payload::Go(forged),
            ]
            .into(),
            shown: Vec::new(),
        };
        let err = run_recv_handshake(&mut peer, &scratch_dir("forged"), false, LinkConfig::fast(), &mut cache)
            .unwrap_err();
        assert!(matches!(err, Error::HandshakeFailed), "{err:?}");
        assert!(peer
            .shown
            .iter()
            .any(|p| matches!(p, Payload::Fail { reason: FAIL_PROTOCOL, .. })));
        assert_eq!(cache.held(), 2, "the interrupted transfer is untouched");
    }
```

- [ ] **Step 8: `src/session.rs`**

1. Replace the `use crate::{...}` block with:

```rust
use crate::{
    link::{self, Attempt, LinkConfig, Linked, Session, MAX_ATTEMPTS},
    optical::Optical,
    resume::{Offer, ReceiveCache},
    transport::{self, TransportConfig},
    Error, Result,
};
```

2. Replace `send_with_fallback` and `recv_with_fallback` with:

```rust
/// Sends `blob`, stepping down the ladder until it lands or the attempts run
/// out.
pub fn send_with_fallback(
    opt: &mut impl Optical,
    offer: &Offer,
    blob: &[u8],
    terminal_max: u8,
) -> Result<Completed> {
    let mut last = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (link_cfg, transport_cfg, _) = configs_for(attempt, terminal_max);
        let outcome = link::run_send_handshake(opt, offer, link_cfg).and_then(
            |Linked { session, mut acked }| {
                transport::send_blob(opt, &session, blob, &mut acked, transport_cfg)?;
                Ok(session)
            },
        );
        match outcome {
            Ok(session) => return Ok(Completed { session, attempt }),
            Err(err) if link::is_retryable(&err) && attempt + 1 < MAX_ATTEMPTS => {
                note_retry(opt, attempt, link::attempt_plan(attempt + 1, terminal_max), &err);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or(Error::HandshakeFailed))
}

/// Receives a blob into `outdir`'s namespace, stepping down the same ladder.
/// Chunks already assembled survive every retry for an identical offer.
///
/// Returns the blob rather than writing it: the caller owns the durable write
/// and is the only thing that may tell the sender OK.
pub fn recv_with_fallback(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
) -> Result<(Completed, Vec<u8>)> {
    let mut cache = ReceiveCache::default();
    let mut last = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (link_cfg, transport_cfg, _) = configs_for(attempt, terminal_max);
        let outcome = link::run_recv_handshake(opt, outdir, force, link_cfg, &mut cache).and_then(|session| {
            let store = cache.store(&session.offer).ok_or(Error::HandshakeFailed)?;
            let blob = transport::recv_blob(opt, &session, store, transport_cfg)?;
            Ok((session, blob))
        });
        if matches!(outcome, Err(Error::HashMismatch)) {
            // Some accepted chunk was wrong and there is no telling which.
            cache.clear();
        }
        match outcome {
            Ok((session, blob)) => return Ok((Completed { session, attempt }, blob)),
            Err(err) if link::is_retryable(&err) && attempt + 1 < MAX_ATTEMPTS => {
                note_retry(opt, attempt, link::attempt_plan(attempt + 1, terminal_max), &err);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or(Error::HandshakeFailed))
}
```

3. In the tests:
   - Add `use crate::resume::{Offer, ReceiveCache};` and `use crate::link::Linked;`.
   - In `a_link_that_cannot_sustain_big_codes_is_walked_down_until_it_can`, insert this after `sha256` is computed: `let offer = Offer::new("dir", blob.len() as u64, blob.len() as u64, sha256, 1).unwrap();`.
   - Replace the two `drive(...)` closures with:

```rust
        let send_t = thread::spawn(move || {
            drive(|_, link_cfg, transport_cfg| {
                let Linked { session, mut acked } =
                    link::run_send_handshake(&mut sender, &offer, link_cfg)?;
                transport::send_blob(&mut sender, &session, &send_blob, &mut acked, transport_cfg)?;
                Ok(session)
            })
        });

        let out = scratch_dir("ladder");
        let mut cache = ReceiveCache::default();
        let (received, recv_attempt) = drive(|_, link_cfg, transport_cfg| {
            let session = link::run_recv_handshake(&mut receiver, &out, false, link_cfg, &mut cache)?;
            let store = cache.store(&session.offer).ok_or(Error::HandshakeFailed)?;
            let got = transport::recv_blob(&mut receiver, &session, store, transport_cfg)?;
            receiver.show(&Payload::Ok {
                transfer_id: session.offer.transfer_id,
            })?;
            Ok((session, got))
        });
```

- [ ] **Step 9: `src/cli.rs`**

1. Change the crate import to `use crate::{chunks, frame, link, live, optical::Optical, pack, resume::{self, Offer}, session, tui};`.

2. In `send`, replace the body of the `tui::run` closure with:

```rust
        let blob = fs::read(&packed.temp_path)?;
        let offer = Offer::new(
            packed.basename.clone(),
            packed.uncompressed_hint,
            blob.len() as u64,
            packed.sha256,
            resume::process_nonce(),
        )
        .ok_or_else(|| {
            crate::Error::Message(format!("{} has too many chunks to number", packed.basename))
        })?;
        let done = session::send_with_fallback(&mut opt, &offer, &blob, max_version)?;
        Ok(format!(
            "sent {} ({} B in {} chunks of {} B, finished at QR v{}{})",
            offer.basename,
            offer.compressed_size,
            offer.chunk_count,
            chunks::CHUNK_BYTES,
            done.session.qr_version,
            attempt_note(done.attempt)
        ))
```

3. Replace `attempt_note` with:

```rust
/// Says how many reconnects the transfer needed, and nothing when the first
/// link carried it all the way.
fn attempt_note(attempt: usize) -> String {
    match attempt {
        0 => String::new(),
        1 => ", after 1 reconnect".into(),
        n => format!(", after {n} reconnects"),
    }
}
```

4. In `recv`, after the `recv_with_fallback` line, add `let offer = done.session.offer.clone();`. Then:
   - Replace `done.session.basename` with `offer.basename`.
   - Use `frame::Payload::Ok { transfer_id: offer.transfer_id }`.
   - Use `frame::Payload::Fail { transfer_id: offer.transfer_id, reason: frame::FAIL_DISK }`, and the same shape with `FAIL_PROTOCOL`.
   - Remove `link` from the imports if it is now unused. It is still used by nothing else in `cli.rs` once `attempt_note` changes, so remove it.

- [ ] **Step 10: `src/live.rs`, `src/tui/optical.rs`, `src/tui/camera.rs`**

`src/live.rs` test: change `&frame::Payload::Ok` to `&frame::Payload::Ok { transfer_id: 1 }`.

`src/tui/optical.rs`:
- Add `resume::UNSCOPED` to the crate imports.
- In both the `show` fallback and `show_many`, use `Payload::Fail { transfer_id: UNSCOPED, reason: crate::frame::FAIL_PROTOCOL }`.
- Replace `payload_label` with:

```rust
pub fn payload_label(payload: &Payload) -> String {
    match payload {
        Payload::Hello { .. } => "HELLO".into(),
        Payload::Probe { id, qr_version, .. } => format!("PROBE {id} (v{qr_version})"),
        Payload::Link { qr_version, .. } => format!("LINK (v{qr_version})"),
        Payload::Go(offer) => format!("GO {}", offer.basename),
        Payload::Progress { contiguous, .. } => format!("PROGRESS @{contiguous}"),
        Payload::Data { first, count, .. } if *count > 1 => {
            format!("DATA #{first}-#{}", first.saturating_add(u32::from(*count) - 1))
        }
        Payload::Data { first, .. } => format!("DATA #{first}"),
        Payload::Fin { .. } => "FIN".into(),
        Payload::Ok { .. } => "OK".into(),
        Payload::Fail { reason, .. } => format!("FAIL {reason}"),
    }
}
```

and replace its test with:

```rust
    #[test]
    fn labels_name_the_frame_kind() {
        assert_eq!(payload_label(&Payload::Ok { transfer_id: 1 }), "OK");
        assert_eq!(
            payload_label(&Payload::Probe {
                id: 2,
                qr_version: 20,
                dwell_ms: 200,
                last: false
            }),
            "PROBE 2 (v20)"
        );
        let data = |count| Payload::Data {
            transfer_id: 1,
            first: 7,
            count,
            bytes: vec![],
        };
        assert_eq!(payload_label(&data(1)), "DATA #7");
        assert_eq!(payload_label(&data(3)), "DATA #7-#9");
    }
```

`src/tui/camera.rs` tests: change every `Payload::Data { seq: N, chunk: V }` to `Payload::Data { transfer_id: 1, first: N, count: 1, bytes: V }`. That covers `data0`, `data1`, and the loop in `a_full_queue_drops_the_oldest_unique_payload`, including its `assert_eq!` expectation. Task 8 rewrites this file.

- [ ] **Step 11: Build and run everything**

Run: `cargo build && cargo test 2>&1 | grep -E "test result|FAILED|panicked"`
Expected:
- `cargo build` finishes with no errors and no new warnings.
- Every test passes. The suite takes longer than before because `handshake_no_probe_response_times_out` now waits 1 s.

- [ ] **Step 12: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/frame.rs src/error.rs src/qr.rs src/optical.rs src/link.rs src/transport.rs src/session.rs src/cli.rs src/live.rs src/tui/mod.rs
cargo test 2>&1 | grep "test result"
git status --short
```
`src/tui/mod.rs` is listed so that rustfmt reaches `tui/optical.rs` and `tui/camera.rs` through it. This reformats `tui/mod.rs` and `tui/widget.rs` too, which is acceptable because Tasks 6–9 edit both.

---

### Task 4: Progress-aware fallback ladder with handshake replay

**Files:**
- Modify: `src/optical.rs` (add `Replay`)
- Rewrite: `src/session.rs`

**Interfaces:**
- Consumes:
  - From Task 3: `link::{run_send_handshake, run_recv_handshake, attempt_plan, is_retryable, Linked, Session, LinkConfig, Attempt, MAX_ATTEMPTS}`, `transport::{send_blob, recv_blob, TransportConfig}`, `Error::Restart`
  - From Task 2: `ReceiveCache`
- Produces:
  - `optical::Replay<'a, T>` with `new(&'a mut T)`, `push_back(Payload)`, `pending() -> usize`, implementing `Optical`
  - `session::Ladder { rung, failures, attempts }` with `record_failure(progressed: bool)` and `exhausted()`
  - `session::configs_for(&Ladder, u8) -> (LinkConfig, TransportConfig, Attempt)`
  - Unchanged public signatures: `send_with_fallback(opt, &Offer, &[u8], u8) -> Result<Completed>` and `recv_with_fallback(opt, &Path, bool, u8) -> Result<(Completed, Vec<u8>)>`
  - `Completed { session, attempt }`, where `attempt` is the number of handshakes before the one that finished

- [ ] **Step 1: Write the failing `Replay` test**

Append to `src/optical.rs` tests:

```rust
    #[test]
    fn replay_hands_back_what_was_pushed_before_anything_new() {
        let (mut a, mut b) = pair();
        b.show(&Payload::Ok { transfer_id: 2 }).unwrap();
        let mut replay = Replay::new(&mut a);
        replay.push_back(Payload::Hello {
            protocol_ver: crate::frame::PROTOCOL_VERSION,
            role: 2,
        });
        assert_eq!(replay.pending(), 1);
        assert!(matches!(
            replay.poll(Duration::from_millis(1)).unwrap(),
            Some(Payload::Hello { .. })
        ));
        assert_eq!(
            replay.poll(Duration::from_millis(1)).unwrap(),
            Some(Payload::Ok { transfer_id: 2 })
        );
    }
```

Run: `cargo test optical::` → expected to fail to compile because `Replay` is not defined.

- [ ] **Step 2: Implement `Replay`**

In `src/optical.rs`, change `collections::HashSet` to `collections::{HashSet, VecDeque}` and add:

```rust
/// Hands back payloads one protocol layer read but the next one must see.
///
/// The transport stops as soon as it sees the peer restart the handshake,
/// and the frame that told it so — a HELLO, PROBE or LINK — is still the one
/// on the peer's screen. A live camera reports an unchanged code only once,
/// so if that read were dropped the next handshake would wait for a HELLO
/// that never arrives "again".
pub struct Replay<'a, T> {
    inner: &'a mut T,
    pending: VecDeque<Payload>,
}

impl<'a, T: Optical> Replay<'a, T> {
    pub fn new(inner: &'a mut T) -> Self {
        Self {
            inner,
            pending: VecDeque::new(),
        }
    }

    /// Queues `payload` to be the next thing [`Optical::poll`] returns.
    pub fn push_back(&mut self, payload: Payload) {
        self.pending.push_back(payload);
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

impl<T: Optical> Optical for Replay<'_, T> {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        self.inner.show(payload)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        match self.pending.pop_front() {
            Some(payload) => Ok(Some(payload)),
            None => self.inner.poll(timeout),
        }
    }

    fn set_status(&mut self, status: &str) {
        self.inner.set_status(status)
    }

    fn set_version(&mut self, version: u8) {
        self.inner.set_version(version)
    }

    fn log(&mut self, line: &str) {
        self.inner.log(line)
    }

    fn suggested_dwell_ms(&mut self) -> u16 {
        self.inner.suggested_dwell_ms()
    }

    fn gate(&mut self, prompt: &str) -> Result<bool> {
        self.inner.gate(prompt)
    }

    fn gate_locked(&mut self, prompt: &str) -> Result<bool> {
        self.inner.gate_locked(prompt)
    }

    fn wait_locked(&mut self, prompt: &str) -> Result<bool> {
        self.inner.wait_locked(prompt)
    }

    fn tile_count(&self) -> usize {
        self.inner.tile_count()
    }

    fn show_many(&mut self, payloads: &[Payload]) -> Result<()> {
        self.inner.show_many(payloads)
    }
}
```

Run: `cargo test optical::` → all pass.

- [ ] **Step 3: Rewrite `src/session.rs` with its tests**

Replace the whole file with:

```rust
//! One complete transfer attempt, and the fallback ladder around it.
//!
//! A failed attempt that still moved chunks was an *interruption* — a
//! covered camera, a bumped lid — not evidence that the code is too dense.
//! The next attempt reconnects at the same rung, the receiver's in-memory
//! chunks carry over, and it costs nothing against the retry budget. An
//! attempt that moved nothing steps down: a smaller QR version (fewer,
//! larger modules) held on screen longer (more camera frames per code).
//!
//! Both sides walk the ladder independently. They need not agree on the
//! rung: the handshake re-runs every attempt, and the negotiated version is
//! whatever the *more* constrained side can manage.

use std::path::Path;

use crate::{
    link::{self, Attempt, LinkConfig, Linked, Session, MAX_ATTEMPTS},
    optical::{Optical, Replay},
    resume::{Offer, ReceiveCache},
    transport::{self, TransportConfig},
    Error, Result,
};

/// What an attempt settled on, for the operator-facing summary.
#[derive(Clone, Debug)]
pub struct Completed {
    pub session: Session,
    /// Handshakes before the one that finished.
    pub attempt: usize,
}

/// Where the fallback ladder stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ladder {
    /// Index into [`link::attempt_plan`]: higher is smaller and slower.
    pub rung: usize,
    /// Attempts that moved nothing; bounded by [`MAX_ATTEMPTS`].
    pub failures: usize,
    /// Every attempt so far, including ones that made progress.
    pub attempts: usize,
}

impl Ladder {
    /// Records a failed attempt. One that moved chunks keeps the rung and
    /// the budget; one that moved nothing steps down and spends the budget.
    pub fn record_failure(&mut self, progressed: bool) {
        self.attempts += 1;
        if !progressed {
            self.rung += 1;
            self.failures += 1;
        }
    }

    pub fn exhausted(&self) -> bool {
        self.failures >= MAX_ATTEMPTS
    }
}

type Configs = fn(&Ladder, u8) -> (LinkConfig, TransportConfig, Attempt);

/// The link and transport settings for the ladder's current position.
///
/// Only the very first attempt is interactive: it waits for a human to aim
/// and has the sender confirm once before GO. Every later attempt —
/// whether it reconnects at the same rung or steps down — runs unattended.
pub fn configs_for(ladder: &Ladder, terminal_max: u8) -> (LinkConfig, TransportConfig, Attempt) {
    let plan = link::attempt_plan(ladder.rung, terminal_max);
    let link_cfg = if ladder.attempts == 0 {
        LinkConfig::gated(plan.max_qr_version)
    } else {
        LinkConfig::retry(plan)
    };
    (link_cfg, TransportConfig { fast: false }, plan)
}

fn note_retry(opt: &mut impl Optical, ladder: &Ladder, terminal_max: u8, err: &Error, progressed: bool) {
    let next = link::attempt_plan(ladder.rung, terminal_max);
    opt.log(&format!("attempt {} interrupted: {err}", ladder.attempts));
    if progressed {
        opt.log(&format!(
            "reconnecting at QR v{} max; everything received so far is kept",
            next.max_qr_version
        ));
    } else {
        opt.log(&format!(
            "retry {}/{}: QR v{} max, {} ms per code",
            ladder.failures + 1,
            MAX_ATTEMPTS,
            next.max_qr_version,
            next.dwell_floor_ms
        ));
    }
}

/// Runs `attempt` until it succeeds, fails in a way no retry can fix, or
/// the ladder runs out. `attempt` reports whether it moved any chunks.
///
/// A handshake start the transport read on its way out is pushed back, so
/// the next attempt's handshake reads it first.
fn climb<O: Optical, T>(
    opt: &mut O,
    terminal_max: u8,
    configs: Configs,
    mut attempt: impl FnMut(&mut Replay<'_, O>, LinkConfig, TransportConfig) -> (Result<T>, bool),
) -> Result<(T, usize)> {
    let mut opt = Replay::new(opt);
    let mut ladder = Ladder::default();
    loop {
        let (link_cfg, transport_cfg, _) = configs(&ladder, terminal_max);
        let (outcome, progressed) = attempt(&mut opt, link_cfg, transport_cfg);
        let err = match outcome {
            Ok(value) => return Ok((value, ladder.attempts)),
            Err(err) if link::is_retryable(&err) => err,
            Err(err) => return Err(err),
        };
        if let Error::Restart(payload) = &err {
            opt.push_back(payload.as_ref().clone());
        }
        ladder.record_failure(progressed);
        if ladder.exhausted() {
            return Err(err);
        }
        note_retry(&mut opt, &ladder, terminal_max, &err, progressed);
    }
}

fn climb_send<O: Optical>(
    opt: &mut O,
    offer: &Offer,
    blob: &[u8],
    terminal_max: u8,
    configs: Configs,
) -> Result<Completed> {
    let (session, attempt) = climb(opt, terminal_max, configs, |opt, link_cfg, transport_cfg| {
        let mut progressed = false;
        let outcome = link::run_send_handshake(opt, offer, link_cfg).and_then(
            |Linked { session, mut acked }| {
                let before = acked.count();
                let sent = transport::send_blob(opt, &session, blob, &mut acked, transport_cfg);
                progressed = acked.count() > before;
                sent.map(|()| session)
            },
        );
        (outcome, progressed)
    })?;
    Ok(Completed { session, attempt })
}

fn climb_recv<O: Optical>(
    opt: &mut O,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
    configs: Configs,
) -> Result<(Completed, Vec<u8>)> {
    let mut cache = ReceiveCache::default();
    let ((session, blob), attempt) = climb(opt, terminal_max, configs, |opt, link_cfg, transport_cfg| {
        let held_before = cache.held();
        let outcome = link::run_recv_handshake(opt, outdir, force, link_cfg, &mut cache).and_then(|session| {
            let store = cache.store(&session.offer).ok_or(Error::HandshakeFailed)?;
            let blob = transport::recv_blob(opt, &session, store, transport_cfg)?;
            Ok((session, blob))
        });
        if matches!(outcome, Err(Error::HashMismatch)) {
            // Some accepted chunk was wrong and there is no telling which;
            // resuming would fail the same way.
            cache.clear();
        }
        let progressed = cache.held() > held_before;
        (outcome, progressed)
    })?;
    Ok((Completed { session, attempt }, blob))
}

/// Sends `blob`, reconnecting after interruptions and stepping down the
/// ladder after attempts that moved nothing.
pub fn send_with_fallback(
    opt: &mut impl Optical,
    offer: &Offer,
    blob: &[u8],
    terminal_max: u8,
) -> Result<Completed> {
    climb_send(opt, offer, blob, terminal_max, configs_for)
}

/// Receives a blob into `outdir`'s namespace. Chunks already assembled
/// survive every retry for an identical offer.
///
/// Returns the blob rather than writing it: the caller owns the durable
/// write and is the only thing that may tell the sender OK.
pub fn recv_with_fallback(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
) -> Result<(Completed, Vec<u8>)> {
    climb_recv(opt, outdir, force, terminal_max, configs_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chunks::ChunkStore,
        frame::{Payload, PROTOCOL_VERSION, ROLE_RECV},
        optical::{pair, Optical},
    };
    use sha2::{Digest, Sha256};
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    fn fast_configs(ladder: &Ladder, terminal_max: u8) -> (LinkConfig, TransportConfig, Attempt) {
        let plan = link::attempt_plan(ladder.rung, terminal_max);
        (
            LinkConfig {
                fast: true,
                max_qr_version: plan.max_qr_version,
                dwell_floor_ms: plan.dwell_floor_ms,
                ..LinkConfig::default()
            },
            TransportConfig::fast(),
            plan,
        )
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let out = std::env::temp_dir().join(format!("ag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        out
    }

    fn offer_for(blob: &[u8]) -> Offer {
        Offer::new("dir", blob.len() as u64, blob.len() as u64, Sha256::digest(blob).into(), 1).unwrap()
    }

    fn hello() -> Payload {
        Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        }
    }

    type Shown = Arc<Mutex<Vec<(u8, Payload)>>>;
    type Logs = Arc<Mutex<Vec<String>>>;

    /// Records what one side shows (with the version locked at the time)
    /// and what it logs.
    struct Tap<T> {
        inner: T,
        version: u8,
        shown: Shown,
        logs: Logs,
    }

    impl<T> Tap<T> {
        fn new(inner: T, shown: Shown, logs: Logs) -> Self {
            Self {
                inner,
                version: 0,
                shown,
                logs,
            }
        }
    }

    impl<T: Optical> Optical for Tap<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.lock().unwrap().push((self.version, payload.clone()));
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.version = version;
            self.inner.set_version(version)
        }

        fn log(&mut self, line: &str) {
            self.logs.lock().unwrap().push(line.to_string());
        }
    }

    /// The receiver's camera reads `open` more DATA frames, then is covered:
    /// every later DATA frame goes unseen until `open` is raised again.
    struct Covered<T> {
        inner: T,
        open: Arc<AtomicUsize>,
    }

    impl<T: Optical> Optical for Covered<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            let deadline = Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.inner.poll(remaining)? {
                    Some(Payload::Data { .. })
                        if self
                            .open
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                            .is_err() =>
                    {
                        continue
                    }
                    other => return Ok(other),
                }
            }
        }

        fn set_version(&mut self, version: u8) {
            self.inner.set_version(version)
        }
    }

    /// A link that carries `budget` DATA frames above `ceiling` and then no
    /// more: the lid slipped, big codes stopped reading, small ones still do.
    struct Degrading<T> {
        inner: T,
        ceiling: u8,
        version: u8,
        budget: usize,
    }

    impl<T: Optical> Optical for Degrading<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Data { .. }) && self.version > self.ceiling {
                if self.budget == 0 {
                    return Ok(());
                }
                self.budget -= 1;
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.version = version;
            self.inner.set_version(version)
        }
    }

    #[test]
    fn an_attempt_that_moved_chunks_reconnects_at_the_same_rung_for_free() {
        let mut ladder = Ladder::default();
        ladder.record_failure(true);
        assert_eq!(ladder, Ladder { rung: 0, failures: 0, attempts: 1 });
        ladder.record_failure(false);
        assert_eq!(ladder, Ladder { rung: 1, failures: 1, attempts: 2 });
        for _ in 1..MAX_ATTEMPTS {
            assert!(!ladder.exhausted());
            ladder.record_failure(false);
        }
        assert!(ladder.exhausted());
    }

    #[test]
    fn only_the_very_first_attempt_asks_the_operator_anything() {
        let (first, _, _) = configs_for(&Ladder::default(), 40);
        assert!(first.patient && first.confirm_offer);
        let resumed = Ladder {
            rung: 0,
            failures: 0,
            attempts: 1,
        };
        let (link_cfg, _, _) = configs_for(&resumed, 40);
        assert!(
            !link_cfg.confirm_offer,
            "a reconnect on the same rung must not ask again"
        );
        assert!(link_cfg.patient);
        for failures in 1..MAX_ATTEMPTS {
            let ladder = Ladder {
                rung: failures,
                failures,
                attempts: failures,
            };
            let (link_cfg, _, _) = configs_for(&ladder, 40);
            assert!(!link_cfg.confirm_offer, "attempt {failures} must not prompt");
            assert!(link_cfg.patient);
        }
    }

    #[test]
    fn a_handshake_start_seen_mid_transfer_is_replayed_to_the_next_attempt() {
        let (mut end, _peer) = pair();
        let mut calls = 0;
        let (seen, attempt) = climb(&mut end, 40, fast_configs, |opt, _, _| {
            calls += 1;
            if calls == 1 {
                return (Err(Error::Restart(Box::new(hello()))), true);
            }
            (opt.poll(Duration::ZERO), false)
        })
        .unwrap();
        assert_eq!(seen, Some(hello()));
        assert_eq!(attempt, 1);
    }

    #[test]
    fn errors_no_retry_can_fix_end_the_climb_at_once() {
        let (mut end, _peer) = pair();
        let mut calls = 0;
        let err = climb(&mut end, 40, fast_configs, |_, _, _| {
            calls += 1;
            (Err::<(), _>(Error::Aborted), false)
        })
        .unwrap_err();
        assert!(matches!(err, Error::Aborted));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_covered_camera_mid_transfer_resumes_instead_of_restarting() {
        let blob: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
        let offer = offer_for(&blob);
        assert_eq!(offer.chunk_count, 42);
        let (send_end, recv_end) = pair();
        let open = Arc::new(AtomicUsize::new(10));
        let shown: Shown = Arc::default();
        let mut sender = Tap::new(send_end, Arc::clone(&shown), Arc::default());
        let mut receiver = Covered {
            inner: recv_end,
            open: Arc::clone(&open),
        };
        let out = scratch_dir("covered");
        let mut cache = ReceiveCache::default();
        let link_cfg = LinkConfig::fast();
        let transport_cfg = TransportConfig::fast();

        // Round 1: the camera is covered after ten frames; both sides give up.
        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || {
                let result = link::run_send_handshake(&mut sender, &offer, link_cfg).and_then(
                    |Linked { session, mut acked }| {
                        transport::send_blob(&mut sender, &session, &blob, &mut acked, transport_cfg)
                    },
                );
                (sender, result)
            }
        });
        let session = link::run_recv_handshake(&mut receiver, &out, false, link_cfg, &mut cache).unwrap();
        let store: &mut ChunkStore = cache.store(&session.offer).unwrap();
        let err = transport::recv_blob(&mut receiver, &session, store, transport_cfg).unwrap_err();
        assert!(matches!(err, Error::Stalled(_)), "{err:?}");
        let (mut sender, first) = send_t.join().unwrap();
        assert!(matches!(first, Err(Error::Stalled(_))), "{first:?}");
        assert_eq!(cache.held(), 10, "everything read before the cover is kept");

        // Round 2: uncovered. Only the missing chunks travel.
        open.store(usize::MAX, Ordering::SeqCst);
        shown.lock().unwrap().clear();
        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || {
                link::run_send_handshake(&mut sender, &offer, link_cfg).and_then(
                    |Linked { session, mut acked }| {
                        assert_eq!(acked.count(), 10, "the sender learns what is held");
                        transport::send_blob(&mut sender, &session, &blob, &mut acked, transport_cfg)
                    },
                )
            }
        });
        let session = link::run_recv_handshake(&mut receiver, &out, false, link_cfg, &mut cache).unwrap();
        let store = cache.store(&session.offer).unwrap();
        let got = transport::recv_blob(&mut receiver, &session, store, transport_cfg).unwrap();
        receiver
            .show(&Payload::Ok {
                transfer_id: offer.transfer_id,
            })
            .unwrap();
        send_t.join().unwrap().unwrap();
        assert_eq!(got, blob);
        assert!(
            !shown
                .lock()
                .unwrap()
                .iter()
                .any(|(_, p)| matches!(p, Payload::Data { first, .. } if *first < 10)),
            "chunks the receiver already held must not be sent again"
        );
    }

    #[test]
    fn a_link_that_degrades_mid_transfer_falls_back_and_keeps_what_arrived() {
        let blob: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
        let offer = offer_for(&blob);
        let (send_end, recv_end) = pair();
        let shown: Shown = Arc::default();
        let recv_logs: Logs = Arc::default();
        let mut sender = Tap::new(
            Degrading {
                inner: send_end,
                ceiling: 15,
                version: 0,
                budget: 3,
            },
            Arc::clone(&shown),
            Arc::default(),
        );
        let mut receiver = Tap::new(recv_end, Arc::default(), Arc::clone(&recv_logs));

        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || climb_send(&mut sender, &offer, &blob, 20, fast_configs)
        });
        let out = scratch_dir("degrade");
        let (done, got) =
            climb_recv(&mut receiver, &out, false, 20, fast_configs).expect("the receiver converges");
        receiver
            .show(&Payload::Ok {
                transfer_id: offer.transfer_id,
            })
            .unwrap();
        let sent = send_t.join().unwrap().expect("the sender converges");

        assert_eq!(got, blob);
        assert!(done.session.qr_version <= 15, "got v{}", done.session.qr_version);
        assert!(sent.session.qr_version <= 15);
        let logs = recv_logs.lock().unwrap().clone();
        assert!(
            logs.iter().any(|line| line.contains("resuming with 12/42")),
            "the fallback must resume, not restart: {logs:?}"
        );
        assert!(
            !shown.lock().unwrap().iter().any(|(version, p)| *version <= 15
                && matches!(p, Payload::Data { first, .. } if *first < 12)),
            "chunks received at v20 must not be resent after the fallback"
        );
    }

    #[test]
    fn the_ladder_shrinks_the_code_then_slows_it_down() {
        let plans: Vec<Attempt> = (0..MAX_ATTEMPTS)
            .map(|attempt| link::attempt_plan(attempt, 40))
            .collect();
        assert_eq!(plans[0].max_qr_version, 40);
        assert_eq!(plans[0].dwell_floor_ms, 0, "the first try uses probe dwell");
        for pair in plans.windows(2) {
            assert!(pair[1].max_qr_version <= pair[0].max_qr_version);
            assert!(pair[1].dwell_floor_ms >= pair[0].dwell_floor_ms);
        }
        assert_eq!(plans[MAX_ATTEMPTS - 1].max_qr_version, crate::qr::smallest_version());
        assert!(plans[MAX_ATTEMPTS - 1].dwell_floor_ms >= 1000);
    }

    #[test]
    fn a_small_terminal_starts_lower_and_still_bottoms_out_safely() {
        let plans: Vec<Attempt> = (0..MAX_ATTEMPTS)
            .map(|attempt| link::attempt_plan(attempt, 15))
            .collect();
        assert_eq!(plans[0].max_qr_version, 15);
        assert_eq!(plans[1].max_qr_version, 10);
        assert!(plans[2..]
            .iter()
            .all(|plan| plan.max_qr_version == crate::qr::smallest_version()));
    }

    #[test]
    fn operator_decisions_are_never_retried_away() {
        assert!(!link::is_retryable(&Error::Aborted));
        assert!(!link::is_retryable(&Error::Interrupted));
        assert!(!link::is_retryable(&Error::DestExists("x".into())));
        assert!(!link::is_retryable(&Error::Refused("x")));
        assert!(!link::is_retryable(&Error::ProtocolMismatch { peer: 1, ours: 2 }));
        assert!(link::is_retryable(&Error::Stalled(vec![3])));
        assert!(link::is_retryable(&Error::HandshakeTimeout));
        assert!(link::is_retryable(&Error::HashMismatch));
        assert!(link::is_retryable(&Error::Restart(Box::new(hello()))));
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test session:: && cargo test 2>&1 | grep "test result"`
Expected: all session tests pass, including `a_covered_camera_mid_transfer_resumes_instead_of_restarting` and `a_link_that_degrades_mid_transfer_falls_back_and_keeps_what_arrived`.

Then check stability. Run `for i in 1 2 3 4 5; do cargo test session:: 2>&1 | grep "test result"; done`; all five runs must pass. If one fails, use @superpowers:systematic-debugging. Do **not** add sleeps.

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/optical.rs src/session.rs
git status --short
```

---

### Task 5: Completion ordering — persist, sync, unpack, then OK

**Files:**
- Create: `src/deliver.rs`
- Modify: `src/lib.rs`, `src/cli.rs`

**Interfaces:**
- Consumes: `Offer` (Task 2), `frame::{Payload, FAIL_*}` (Task 3), `pack::unpack`.
- Produces:
  - `pub trait ArchiveSink { fn persist(&mut self, blob: &[u8]) -> Result<()>; fn unpack(&mut self) -> Result<()>; }`
  - `pub struct DiskSink { pub archive_path: PathBuf, pub outdir: PathBuf, pub force: bool }`
  - `pub fn deliver(opt: &mut impl Optical, offer: &Offer, blob: &[u8], sink: &mut impl ArchiveSink) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

Create `src/deliver.rs` containing only the test module below, and add `pub mod deliver;` to `src/lib.rs` under `pub mod cli;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack;
    use sha2::{Digest, Sha256};
    use std::{cell::RefCell, fs, rc::Rc, time::Duration};

    type Events = Rc<RefCell<Vec<String>>>;

    struct Watch {
        events: Events,
    }

    impl Optical for Watch {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.events.borrow_mut().push(match payload {
                Payload::Ok { .. } => "OK".into(),
                Payload::Fail { reason, .. } => format!("FAIL {reason}"),
                other => format!("{other:?}"),
            });
            Ok(())
        }

        fn poll(&mut self, _: Duration) -> Result<Option<Payload>> {
            Ok(None)
        }
    }

    struct Scripted {
        events: Events,
        fail_persist: bool,
        fail_unpack: bool,
    }

    impl ArchiveSink for Scripted {
        fn persist(&mut self, _blob: &[u8]) -> Result<()> {
            self.events.borrow_mut().push("persist+sync".into());
            if self.fail_persist {
                return Err(Error::Io(std::io::ErrorKind::StorageFull.into()));
            }
            Ok(())
        }

        fn unpack(&mut self) -> Result<()> {
            self.events.borrow_mut().push("unpack".into());
            if self.fail_unpack {
                return Err(Error::Message("bad archive".into()));
            }
            Ok(())
        }
    }

    fn run(blob: &[u8], claimed: &[u8], fail_persist: bool, fail_unpack: bool) -> (Result<()>, Vec<String>) {
        let events: Events = Rc::default();
        let offer = Offer::new("dir", 1, claimed.len() as u64, Sha256::digest(claimed).into(), 1).unwrap();
        let result = deliver(
            &mut Watch { events: Rc::clone(&events) },
            &offer,
            blob,
            &mut Scripted {
                events: Rc::clone(&events),
                fail_persist,
                fail_unpack,
            },
        );
        let log = events.borrow().clone();
        (result, log)
    }

    #[test]
    fn ok_is_shown_only_after_the_archive_is_synced_and_unpacked() {
        let (result, events) = run(b"blob", b"blob", false, false);
        result.unwrap();
        assert_eq!(events, ["persist+sync", "unpack", "OK"]);
    }

    #[test]
    fn a_failed_unpack_reports_fail_and_never_ok() {
        let (result, events) = run(b"blob", b"blob", false, true);
        assert!(result.is_err());
        assert_eq!(events, ["persist+sync", "unpack", "FAIL 3"]);
    }

    #[test]
    fn a_full_disk_reports_fail_disk_and_skips_unpacking() {
        let (result, events) = run(b"blob", b"blob", true, false);
        assert!(result.is_err());
        assert_eq!(events, ["persist+sync", "FAIL 2"]);
    }

    #[test]
    fn a_blob_that_is_not_the_offered_archive_is_never_written() {
        let (result, events) = run(b"blob", b"other", false, false);
        assert!(matches!(result, Err(Error::HashMismatch)));
        assert_eq!(events, ["FAIL 1"]);
    }

    #[test]
    fn the_disk_sink_leaves_the_archive_and_the_unpacked_tree() {
        let root = std::env::temp_dir().join(format!("ag-deliver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src/project");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"hello").unwrap();
        let packed = pack::pack(&src).unwrap();
        let blob = fs::read(&packed.temp_path).unwrap();
        pack::remove_temp(&packed.temp_path);

        let out = root.join("out");
        let mut sink = DiskSink {
            archive_path: out.join(pack::archive_filename("project")),
            outdir: out.clone(),
            force: false,
        };
        let offer = Offer::new("project", packed.uncompressed_hint, blob.len() as u64, packed.sha256, 1).unwrap();
        let events: Events = Rc::default();
        deliver(&mut Watch { events: Rc::clone(&events) }, &offer, &blob, &mut sink).unwrap();
        assert_eq!(fs::read(out.join("project/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(&sink.archive_path).unwrap(), blob);
        assert_eq!(*events.borrow(), ["OK"]);
        let _ = fs::remove_dir_all(&root);
    }
}
```

Run: `cargo test deliver::` → expected to fail to compile because `deliver`, `ArchiveSink` and `DiskSink` are not defined.

- [ ] **Step 2: Implement**

Put this above the tests in `src/deliver.rs`:

```rust
//! Turning a verified archive into files on disk, and only then telling the
//! sender.
//!
//! OK is the sender's only evidence the transfer worked, so it must never
//! run ahead of reality: the archive is returned by the transport, written,
//! flushed to stable storage, and unpacked — and only then is OK shown. Any
//! failure on the way shows FAIL instead.

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    frame::{Payload, FAIL_DISK, FAIL_HASH, FAIL_PROTOCOL},
    optical::Optical,
    resume::Offer,
    Error, Result,
};

/// Where a received archive goes.
pub trait ArchiveSink {
    /// Writes the archive and flushes it to stable storage.
    fn persist(&mut self, blob: &[u8]) -> Result<()>;
    /// Unpacks the persisted archive.
    fn unpack(&mut self) -> Result<()>;
}

/// Writes `<outdir>/<basename>.tar.zst`, fsyncs it, and unpacks it into
/// `outdir`. The archive is left in place either way: if unpacking fails the
/// operator can still run `tar --zstd -xf` on it.
pub struct DiskSink {
    pub archive_path: PathBuf,
    pub outdir: PathBuf,
    pub force: bool,
}

impl ArchiveSink for DiskSink {
    fn persist(&mut self, blob: &[u8]) -> Result<()> {
        let parent = self.archive_path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut file = File::create(&self.archive_path)?;
        file.write_all(blob)?;
        file.sync_all()?;
        sync_dir(parent)
    }

    fn unpack(&mut self) -> Result<()> {
        crate::pack::unpack(&self.archive_path, &self.outdir, self.force).map(|_| ())
    }
}

/// Makes the new directory entry itself durable. Only Unix can open a
/// directory for syncing; elsewhere the file sync is the best available.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

fn fail_reason(err: &Error) -> u8 {
    match err {
        Error::HashMismatch => FAIL_HASH,
        Error::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => FAIL_DISK,
        _ => FAIL_PROTOCOL,
    }
}

/// Re-checks `blob` against the offer, persists and unpacks it through
/// `sink`, and shows OK only once all of that succeeded. On failure shows
/// FAIL and returns the error.
pub fn deliver(
    opt: &mut impl Optical,
    offer: &Offer,
    blob: &[u8],
    sink: &mut impl ArchiveSink,
) -> Result<()> {
    let outcome = (|| {
        let digest: [u8; 32] = Sha256::digest(blob).into();
        if digest != offer.sha256 {
            return Err(Error::HashMismatch);
        }
        sink.persist(blob)?;
        sink.unpack()
    })();
    match outcome {
        Ok(()) => opt.show(&Payload::Ok {
            transfer_id: offer.transfer_id,
        }),
        Err(err) => {
            let _ = opt.show(&Payload::Fail {
                transfer_id: offer.transfer_id,
                reason: fail_reason(&err),
            });
            Err(err)
        }
    }
}
```

- [ ] **Step 3: Route `recv` through `deliver`**

In `src/cli.rs`:
- Add `deliver` to the crate imports.
- Remove `frame` from them if it becomes unused.
- Replace the body of the `recv` `tui::run` closure with:

```rust
        let (done, blob) = session::recv_with_fallback(&mut opt, &outdir, force, max_version)?;
        let offer = done.session.offer.clone();
        let archive_name = pack::archive_filename(&offer.basename);
        let archive_path = outdir.join(&archive_name);
        if !keep_temp {
            live::set_temp_path(Some(archive_path.clone()));
        }
        opt.log(&format!(
            "verified {archive_name}; writing, syncing and unpacking before telling the sender"
        ));
        let mut sink = deliver::DiskSink {
            archive_path,
            outdir: outdir.clone(),
            force,
        };
        let delivered = deliver::deliver(&mut opt, &offer, &blob, &mut sink);
        live::set_temp_path(None);
        delivered?;
        Ok(format!(
            "received {archive_name} into {} (unpacked {}){}",
            outdir.display(),
            offer.basename,
            attempt_note(done.attempt)
        ))
```

- [ ] **Step 4: Run the tests**

Run: `cargo test deliver:: && cargo build && cargo test 2>&1 | grep "test result"`
Expected: 5 new tests pass, the build is clean, and the full suite is green.

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/deliver.rs src/cli.rs
git status --short
```

---

### Task 6: Confirmation guards

**Files:**
- Create: `src/tui/gate.rs`
- Modify: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `tui::optical::GateReply`.
- Produces:
  - `pub const ARM_DELAY: Duration` (400 ms)
  - `GateSpec { require_lock: bool, auto_on_lock: bool, armed_at: Instant }` (Copy)
  - `Gate { prompt: String, reply: Sender<GateReply>, spec: GateSpec }`
  - `KeyAction { Proceed, Abort, Ignore }`
  - `on_key(&GateSpec, KeyCode, locked: bool, now: Instant) -> KeyAction`
  - `auto_proceed(&GateSpec, locked: bool) -> bool`

- [ ] **Step 1: Write the failing tests**

Create `src/tui/gate.rs` containing only the tests below, and add `pub mod gate;` to `src/tui/mod.rs` after `pub mod camera;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn spec(require_lock: bool, auto_on_lock: bool, armed_at: Instant) -> GateSpec {
        GateSpec {
            require_lock,
            auto_on_lock,
            armed_at,
        }
    }

    #[test]
    fn enter_is_ignored_until_tracking_locks() {
        let t0 = Instant::now();
        let aim = spec(true, false, t0);
        let later = t0 + ARM_DELAY;
        assert_eq!(on_key(&aim, KeyCode::Enter, false, later), KeyAction::Ignore);
        assert_eq!(on_key(&aim, KeyCode::Enter, true, later), KeyAction::Proceed);
    }

    #[test]
    fn the_receiver_never_confirms_by_key_and_proceeds_on_lock_alone() {
        let t0 = Instant::now();
        let auto = spec(true, true, t0);
        assert_eq!(on_key(&auto, KeyCode::Enter, true, t0 + ARM_DELAY), KeyAction::Ignore);
        assert!(!auto_proceed(&auto, false));
        assert!(auto_proceed(&auto, true));
        assert!(!auto_proceed(&spec(true, false, t0), true), "the sender must press Enter");
    }

    #[test]
    fn an_enter_that_arrives_with_the_gate_is_not_a_confirmation() {
        let t0 = Instant::now();
        let offer = spec(false, false, t0);
        assert_eq!(
            on_key(&offer, KeyCode::Enter, true, t0 + Duration::from_millis(100)),
            KeyAction::Ignore,
            "a held or bounced Enter must not also confirm the next gate"
        );
        assert_eq!(on_key(&offer, KeyCode::Enter, true, t0 + ARM_DELAY), KeyAction::Proceed);
    }

    #[test]
    fn esc_always_aborts_and_other_keys_do_nothing() {
        let t0 = Instant::now();
        for gate in [spec(true, false, t0), spec(true, true, t0), spec(false, false, t0)] {
            assert_eq!(on_key(&gate, KeyCode::Esc, false, t0), KeyAction::Abort);
            assert_eq!(on_key(&gate, KeyCode::Char('y'), true, t0 + ARM_DELAY), KeyAction::Ignore);
        }
    }
}
```

Run: `cargo test tui::gate::` → expected to fail to compile because the gate items are not defined.

- [ ] **Step 2: Implement**

Put this above the tests in `src/tui/gate.rs`:

```rust
//! What a keypress means at a confirmation gate.
//!
//! The confirmation model is deliberately lopsided: the sender aims until
//! tracking locks and confirms, and the receiver never confirms anything.
//! These rules keep that model honest — Enter cannot slip through before the
//! camera is locked, the receiver cannot be made to "accept" by key, and a
//! held or bounced Enter that opened one gate cannot also confirm the next.

use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use crossterm::event::KeyCode;

use crate::tui::optical::GateReply;

/// Enter presses this soon after a gate appears belong to the keypress that
/// ended the previous one (key repeat, a bounce), not to this gate.
pub const ARM_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateSpec {
    /// Enter does nothing until the camera is tracking the peer.
    pub require_lock: bool,
    /// Lock itself is the answer; Enter does nothing at all.
    pub auto_on_lock: bool,
    /// When the gate appeared on screen.
    pub armed_at: Instant,
}

/// A gate the UI is holding open for the protocol thread.
pub struct Gate {
    pub prompt: String,
    pub reply: Sender<GateReply>,
    pub spec: GateSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Proceed,
    Abort,
    Ignore,
}

pub fn on_key(spec: &GateSpec, key: KeyCode, locked: bool, now: Instant) -> KeyAction {
    match key {
        KeyCode::Esc => KeyAction::Abort,
        KeyCode::Enter if spec.auto_on_lock => KeyAction::Ignore,
        KeyCode::Enter if spec.require_lock && !locked => KeyAction::Ignore,
        KeyCode::Enter if now.saturating_duration_since(spec.armed_at) < ARM_DELAY => KeyAction::Ignore,
        KeyCode::Enter => KeyAction::Proceed,
        _ => KeyAction::Ignore,
    }
}

/// Whether an auto-on-lock gate should open now.
pub fn auto_proceed(spec: &GateSpec, locked: bool) -> bool {
    spec.auto_on_lock && locked
}
```

- [ ] **Step 3: Use the rules in `src/tui/mod.rs`**

1. Delete the local `struct Gate { ... }` (current lines 241-246).
2. Add `gate::{self, Gate, GateSpec, KeyAction},` inside the `tui::{...}` import.
3. Replace the `match key.code { KeyCode::Enter => {...} KeyCode::Esc => {...} _ => {} }` block in `event_loop` with:

```rust
                let action = ui.gate.as_ref().map_or(KeyAction::Ignore, |gate| {
                    gate::on_key(&gate.spec, key.code, feed.locked(), Instant::now())
                });
                let reply = match action {
                    KeyAction::Proceed => Some(GateReply::Proceed),
                    KeyAction::Abort => Some(GateReply::Abort),
                    KeyAction::Ignore => None,
                };
                if let Some(reply) = reply {
                    if let Some(gate) = ui.gate.take() {
                        let _ = gate.reply.send(reply);
                    }
                }
```

4. In the `Ok(UiEvent::Gate { ... })` arm, build the gate as:

```rust
                    ui.gate = Some(Gate {
                        prompt,
                        reply,
                        spec: GateSpec {
                            require_lock,
                            auto_on_lock,
                            armed_at: Instant::now(),
                        },
                    });
```

5. Replace the auto-proceed `if` after drawing with:

```rust
        if ui.done.is_none()
            && ui
                .gate
                .as_ref()
                .is_some_and(|gate| gate::auto_proceed(&gate.spec, feed.locked()))
        {
            if let Some(gate) = ui.gate.take() {
                let _ = gate.reply.send(GateReply::Proceed);
            }
        }
```

6. In `draw`, replace `gate.require_lock` with `gate.spec.require_lock` and `gate.auto_on_lock` with `gate.spec.auto_on_lock`. There are five occurrences across the `hints` and `prompt` matches.

- [ ] **Step 4: Run the tests**

Run: `cargo test tui:: && cargo build`
Expected: the 4 gate tests pass and the build has no warnings.

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/tui/mod.rs
git status --short
```

---

### Task 7: Shared layout for rendering and tile allocation

**Files:**
- Create: `src/tui/layout.rs`
- Rewrite: `src/tui/mod.rs`
- Modify: `src/tui/widget.rs`, `src/tui/optical.rs`

**Interfaces:**
- Consumes: `qr::{cell_size_for_version, SUPPORTED_VERSIONS, modules_of, encode_version}`, `gate::*` (Task 6).
- Produces (`tui::layout`):
  - Constants: `FLOOR_CHROME_ROWS = 2`, `BORDER = 2`, `MIN_SIDE_COLS = 24`
  - `Phase { Aiming, Linked }`
  - `Tiles { One, Fill }`
  - `Chrome { qr_border, log, prompt, footer }` with `Chrome::fit(..)` and `Chrome::bare()`
  - `TileGrid { across, down, tile_cols, tile_rows }` with `count()` and `cell(Rect, usize) -> Rect`
  - `Screen { title, qr, qr_frame, grid, side: Option<Rect>, log, prompt, footer, chrome }`
  - `plan(area: Rect, phase: Phase, tiles: Tiles, tile: (u16, u16)) -> Screen`
  - `data_tiles(area: Rect, tiles: Tiles, version: u8) -> usize`
  - `ScreenSize`, which publishes the last drawn area, with `set(Rect)` and `get() -> Option<Rect>`
- Produces (other modules):
  - `QrGridWidget { images, invert, grid: TileGrid }`
  - `TuiOptical::new(camera, events, invert, screen: Arc<ScreenSize>, tiles: Tiles)`
  - `UiEvent::Show { images, invert, label, linked: bool, version: u8, drawn }`

- [ ] **Step 1: Write the failing layout tests**

Create `src/tui/layout.rs` containing only this test module, and add `pub mod layout;` to `src/tui/mod.rs` after `pub mod gate;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr::{cell_size_for_version, SUPPORTED_VERSIONS};

    fn sizes() -> impl Iterator<Item = (u16, u16)> {
        (40..=330u16)
            .step_by(7)
            .flat_map(|w| (20..=100u16).step_by(4).map(move |h| (w, h)))
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x && inner.y >= outer.y && inner.right() <= outer.right() && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn no_layout_ever_clips_a_code_or_lets_the_side_pane_overlap_it() {
        for (w, h) in sizes() {
            let area = Rect::new(0, 0, w, h);
            for version in SUPPORTED_VERSIONS {
                let tile = cell_size_for_version(version);
                for phase in [Phase::Aiming, Phase::Linked] {
                    for tiles in [Tiles::One, Tiles::Fill] {
                        let s = plan(area, phase, tiles, tile);
                        let why = format!("{w}x{h} v{version} {phase:?} {tiles:?}: {s:?}");
                        assert!(s.grid.across * tile.0 <= s.qr.width, "{why}");
                        assert!(s.grid.down * tile.1 <= s.qr.height, "{why}");
                        for index in 0..s.grid.count() {
                            assert!(inside(s.qr, s.grid.cell(s.qr, index)), "{why}");
                        }
                        assert_eq!(s.qr_frame.x, area.x, "codes stay in the left column: {why}");
                        if let Some(side) = s.side {
                            assert!(side.x >= s.qr_frame.right(), "{why}");
                            assert!(side.width >= MIN_SIDE_COLS, "{why}");
                            assert!(s.grid.count() > 0, "{why}");
                        }
                        for rect in [s.title, s.qr_frame, s.log, s.prompt, s.footer] {
                            assert!(inside(area, rect), "{why}");
                        }
                        if phase == Phase::Aiming || tiles == Tiles::One {
                            assert!(s.grid.count() <= 1, "{why}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_terminals_hide_the_side_pane_rather_than_squeeze_the_code() {
        let tile = cell_size_for_version(10);
        let narrow = plan(Rect::new(0, 0, tile.0 + MIN_SIDE_COLS - 1, tile.1 + 2), Phase::Aiming, Tiles::Fill, tile);
        assert!(narrow.side.is_none());
        assert_eq!(narrow.grid.count(), 1);
        let wide_enough = plan(Rect::new(0, 0, tile.0 + MIN_SIDE_COLS, tile.1 + 2), Phase::Aiming, Tiles::Fill, tile);
        assert_eq!(wide_enough.side.map(|r| r.width), Some(MIN_SIDE_COLS));
    }

    #[test]
    fn a_sender_gives_up_a_third_column_of_codes_for_the_monitor_but_never_its_second() {
        let tile = cell_size_for_version(10);
        let three = plan(Rect::new(0, 0, 200, 70), Phase::Linked, Tiles::Fill, tile);
        assert_eq!((three.grid.across, three.grid.down), (2, 2));
        assert_eq!(three.side.map(|r| r.width), Some(70));
        let two = plan(Rect::new(0, 0, 140, 70), Phase::Linked, Tiles::Fill, tile);
        assert_eq!(two.grid.across, 2);
        assert!(two.side.is_none());
    }

    #[test]
    fn the_receiver_keeps_one_code_and_room_to_watch_the_sender() {
        let s = plan(Rect::new(0, 0, 148, 38), Phase::Linked, Tiles::One, cell_size_for_version(10));
        assert_eq!(s.grid.count(), 1);
        assert_eq!(s.side.map(|r| r.width), Some(148 - 65));
    }

    #[test]
    fn data_tiles_is_exactly_what_the_linked_plan_draws() {
        for (w, h) in sizes() {
            let area = Rect::new(0, 0, w, h);
            for version in SUPPORTED_VERSIONS {
                let drawn = plan(area, Phase::Linked, Tiles::Fill, cell_size_for_version(version)).grid.count();
                assert_eq!(data_tiles(area, Tiles::Fill, version), drawn.max(1));
                assert!(data_tiles(area, Tiles::One, version) <= 1);
            }
        }
    }

    #[test]
    fn the_published_screen_size_round_trips() {
        let size = ScreenSize::default();
        assert_eq!(size.get(), None);
        size.set(Rect::new(0, 0, 148, 38));
        assert_eq!(size.get(), Some(Rect::new(0, 0, 148, 38)));
    }

    #[test]
    fn chrome_sheds_decoration_from_the_outside_in_and_never_squeezes_the_code() {
        let (cols, rows) = cell_size_for_version(10);
        let roomy = Chrome::fit(200, 60, cols, rows);
        assert_eq!(roomy.qr_border, BORDER);
        assert_eq!(roomy.footer, 1);
        assert_eq!(roomy.prompt, 1 + BORDER);
        assert_eq!(roomy.log, LOG_ROWS);
        let reported = Chrome::fit(148, 38, cols, rows);
        assert_eq!(reported.log, BORDER + 1);
        assert_eq!(reported.qr_border, 0);
        assert_eq!(reported.footer, 0);
        assert_eq!(reported.prompt, 1);
        assert_eq!(Chrome::fit(cols, rows + FLOOR_CHROME_ROWS, cols, rows), Chrome::bare());
        assert_eq!(Chrome::fit(400, rows + FLOOR_CHROME_ROWS, cols, rows).qr_border, 0);
        assert_eq!(Chrome::fit(cols, 100, cols, rows).qr_border, 0);
    }

    #[test]
    fn every_chrome_layout_leaves_the_code_its_full_height() {
        let (cols, rows) = cell_size_for_version(10);
        for height in (rows + FLOOR_CHROME_ROWS)..=(rows + 30) {
            let chrome = Chrome::fit(148, height, cols, rows);
            let used = 1 + chrome.log + chrome.prompt + chrome.footer + chrome.qr_border;
            assert!(used + rows <= height, "chrome {chrome:?} overflows a {height}-row terminal");
        }
    }
}
```

Run: `cargo test tui::layout::` → expected to fail to compile.

- [ ] **Step 2: Implement `src/tui/layout.rs`**

Put this above the tests:

```rust
//! Where everything goes on screen, computed once and shared.
//!
//! The renderer and the protocol thread's tile allocation both read
//! [`plan`], so the number of DATA codes the sender pages through is exactly
//! the number the grid draws — never one more that would land clipped.
//! Outgoing codes always sit in a left-hand column; the side pane on the
//! right gets only columns the codes cannot use and is hidden, never
//! squeezed, when there are too few.

use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::layout::Rect;

/// Rows the frame can never give up: the title line and one prompt line.
pub(crate) const FLOOR_CHROME_ROWS: u16 = 2;
/// Rows the transcript takes when there is room to spare.
const LOG_ROWS: u16 = 6;
/// Rows (and columns) a bordered block consumes.
pub(crate) const BORDER: u16 = 2;
/// Narrowest side pane worth drawing.
pub const MIN_SIDE_COLS: u16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Before the link: one code at a time, camera preview alongside.
    Aiming,
    /// After the link: codes tiled, peer monitor alongside.
    Linked,
}

/// How many codes this side ever paints at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiles {
    /// The receiver: one progress report at a time.
    One,
    /// The sender: as many DATA codes as fit.
    Fill,
}

/// How much decoration this terminal can afford around the codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    /// Border around the QR pane: 0 or [`BORDER`].
    pub qr_border: u16,
    /// Transcript height including its border, or 0.
    pub log: u16,
    /// Prompt height: 1 bare, or 1 + [`BORDER`] when boxed.
    pub prompt: u16,
    /// Key-hint footer: 0 or 1.
    pub footer: u16,
}

impl Chrome {
    /// Title and one prompt line, nothing else.
    pub fn bare() -> Self {
        Chrome {
            qr_border: 0,
            log: 0,
            prompt: 1,
            footer: 0,
        }
    }

    /// Adds decoration in descending order of usefulness, stopping as soon
    /// as the next piece would eat into the code.
    pub fn fit(width: u16, height: u16, qr_cols: u16, qr_rows: u16) -> Self {
        let mut chrome = Chrome::bare();
        let mut spare = height.saturating_sub(FLOOR_CHROME_ROWS + qr_rows);
        if spare > BORDER {
            chrome.log = BORDER + 1;
            spare -= BORDER + 1;
        }
        if spare >= BORDER && width >= qr_cols + BORDER {
            chrome.qr_border = BORDER;
            spare -= BORDER;
        }
        if spare >= 1 {
            chrome.footer = 1;
            spare -= 1;
        }
        if spare >= BORDER {
            chrome.prompt += BORDER;
            spare -= BORDER;
        }
        if chrome.log > 0 {
            chrome.log += spare.min(LOG_ROWS.saturating_sub(chrome.log));
        }
        chrome
    }
}

/// Codes packed row-major from the top-left of the QR pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGrid {
    pub across: u16,
    pub down: u16,
    pub tile_cols: u16,
    pub tile_rows: u16,
}

impl TileGrid {
    pub fn count(&self) -> usize {
        usize::from(self.across) * usize::from(self.down)
    }

    /// Cell of tile `index` inside `area`.
    pub fn cell(&self, area: Rect, index: usize) -> Rect {
        let across = usize::from(self.across.max(1));
        let col = (index % across) as u16;
        let row = (index / across) as u16;
        Rect::new(
            area.x + col * self.tile_cols,
            area.y + row * self.tile_rows,
            self.tile_cols,
            self.tile_rows,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub title: Rect,
    /// Where codes are drawn, inside any border.
    pub qr: Rect,
    /// The left column including its border.
    pub qr_frame: Rect,
    pub grid: TileGrid,
    /// Camera preview while aiming, peer monitor once linked; `None` when
    /// the terminal cannot spare [`MIN_SIDE_COLS`].
    pub side: Option<Rect>,
    pub log: Rect,
    pub prompt: Rect,
    pub footer: Rect,
    pub chrome: Chrome,
}

/// Lays out one frame for codes of `tile` cells (columns, rows).
pub fn plan(area: Rect, phase: Phase, tiles: Tiles, tile: (u16, u16)) -> Screen {
    let (tile_cols, tile_rows) = tile;
    let chrome = match phase {
        Phase::Aiming => Chrome::fit(area.width, area.height, tile_cols, tile_rows),
        Phase::Linked => Chrome::bare(),
    };

    let title_h = area.height.min(1);
    let below = area.height - title_h;
    let body_h = below - (chrome.log + chrome.prompt + chrome.footer).min(below);
    let title = Rect::new(area.x, area.y, area.width, title_h);
    let body = Rect::new(area.x, area.y + title_h, area.width, body_h);
    let mut y = body.bottom();
    let mut stack = |height: u16| {
        let height = height.min(area.bottom().saturating_sub(y));
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let log = stack(chrome.log);
    let prompt = stack(chrome.prompt);
    let footer = stack(chrome.footer);

    let border = chrome.qr_border;
    let inner_w = body.width.saturating_sub(border);
    let inner_h = body.height.saturating_sub(border);
    let fit_across = inner_w.checked_div(tile_cols).unwrap_or(0);
    let fit_down = inner_h.checked_div(tile_rows).unwrap_or(0);
    let single = phase == Phase::Aiming || tiles == Tiles::One;
    let (want_across, want_down) = if single {
        (fit_across.min(1), fit_down.min(1))
    } else {
        (fit_across, fit_down)
    };
    let (across, side_w) = split_columns(body.width, border, tile_cols, want_across, !single);
    let grid = TileGrid {
        across,
        down: if across == 0 { 0 } else { want_down },
        tile_cols,
        tile_rows,
    };

    let frame_w = body.width - side_w.unwrap_or(0);
    let qr_frame = Rect::new(body.x, body.y, frame_w, body.height);
    let inset = border / 2;
    let qr = Rect::new(
        qr_frame.x + inset,
        qr_frame.y + inset,
        qr_frame.width.saturating_sub(border),
        qr_frame.height.saturating_sub(border),
    );
    let side = side_w.map(|width| Rect::new(body.x + frame_w, body.y, width, body.height));
    Screen {
        title,
        qr,
        qr_frame,
        grid,
        side,
        log,
        prompt,
        footer,
        chrome,
    }
}

/// Splits the body between the code column and the side pane. Codes come
/// first; the side pane gets only what is left over, except that a sender
/// filling three or more columns gives one up so the operator can still
/// watch the peer — never more than a third of the codes.
fn split_columns(width: u16, border: u16, tile_cols: u16, across: u16, may_yield: bool) -> (u16, Option<u16>) {
    if across == 0 {
        return (0, None);
    }
    let leftover = width.saturating_sub(across * tile_cols + border);
    if leftover >= MIN_SIDE_COLS {
        return (across, Some(leftover));
    }
    if may_yield && across >= 3 {
        return (across - 1, Some(leftover + tile_cols));
    }
    (across, None)
}

/// DATA codes the sender should page per dwell on a screen of `area`:
/// exactly what [`plan`] draws once linked, and never zero.
pub fn data_tiles(area: Rect, tiles: Tiles, version: u8) -> usize {
    plan(area, Phase::Linked, tiles, crate::qr::cell_size_for_version(version))
        .grid
        .count()
        .max(1)
}

/// The terminal size the UI last drew at, published for the protocol thread
/// so its tile count is computed from the same area the renderer used.
#[derive(Debug, Default)]
pub struct ScreenSize(AtomicU32);

impl ScreenSize {
    pub fn set(&self, area: Rect) {
        self.0.store(
            (u32::from(area.width) << 16) | u32::from(area.height),
            Ordering::Relaxed,
        );
    }

    pub fn get(&self) -> Option<Rect> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            packed => Some(Rect::new(0, 0, (packed >> 16) as u16, (packed & 0xffff) as u16)),
        }
    }
}
```

Run: `cargo test tui::layout::` → all 8 tests pass. The old `Chrome` in `mod.rs` still exists until Step 4, so you may see a dead-code warning until then.

- [ ] **Step 3: `QrGridWidget` draws from a `TileGrid` (`src/tui/widget.rs`)**

Add `use crate::tui::layout::TileGrid;`. Replace `QrGridWidget` and its `impl` with:

```rust
/// Several QR codes packed into the cells of a [`TileGrid`], used after the
/// link. The grid comes from the shared layout, so the protocol never pages
/// more codes than this draws.
pub struct QrGridWidget<'a> {
    pub images: &'a [Arc<GrayImage>],
    pub invert: bool,
    pub grid: TileGrid,
}

impl Widget for QrGridWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let quiet = Style::new().bg(Color::White).fg(Color::Black);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(' ').set_style(quiet);
                }
            }
        }
        if self.grid.count() == 0 {
            if !self.images.is_empty() {
                Paragraph::new(format!(
                    "QR needs {}x{} cells, pane is {}x{}. Zoom out (Ctrl/- or Cmd/-) or enlarge the window.",
                    self.grid.tile_cols, self.grid.tile_rows, area.width, area.height
                ))
                .style(quiet)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .render(area, buf);
            }
            return;
        }
        for (index, image) in self.images.iter().take(self.grid.count()).enumerate() {
            QrWidget {
                image: Some(image.as_ref()),
                invert: self.invert,
            }
            .render(self.grid.cell(area, index), buf);
        }
    }
}
```

Replace the test `qr_grid_still_draws_one_code_when_the_pane_cannot_hold_the_whole_grid` with these two tests:

```rust
    fn dark_cells(buf: &Buffer, rect: Rect) -> usize {
        (rect.y..rect.bottom())
            .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() != " ")
            .count()
    }

    #[test]
    fn a_grid_of_one_draws_one_code_even_when_given_more() {
        let a = std::sync::Arc::new(qr::encode_version(b"AX-a", 10).unwrap());
        let b = std::sync::Arc::new(qr::encode_version(b"AX-b", 10).unwrap());
        let (cols, rows) = qr::cell_size_for_version(10);
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        let grid = TileGrid { across: 1, down: 1, tile_cols: cols, tile_rows: rows };
        QrGridWidget { images: &[a, b], invert: false, grid }.render(area, &mut buf);
        assert!(dark_cells(&buf, area) > 0);
    }

    #[test]
    fn every_allocated_tile_is_drawn_whole() {
        use crate::tui::layout::{data_tiles, plan, Phase, Tiles};
        for (w, h) in [(140u16, 70u16), (200, 70), (330, 100), (66, 36)] {
            for version in [10u8, 15] {
                let area = Rect::new(0, 0, w, h);
                let screen = plan(area, Phase::Linked, Tiles::Fill, qr::cell_size_for_version(version));
                if screen.grid.count() == 0 {
                    continue;
                }
                let n = data_tiles(area, Tiles::Fill, version);
                assert_eq!(n, screen.grid.count());
                let images: Vec<_> = (0..n)
                    .map(|i| std::sync::Arc::new(qr::encode_version(format!("AX-{i}").as_bytes(), version).unwrap()))
                    .collect();
                let mut buf = Buffer::empty(area);
                QrGridWidget { images: &images, invert: false, grid: screen.grid }.render(screen.qr, &mut buf);
                for index in 0..n {
                    let cell = screen.grid.cell(screen.qr, index);
                    assert!(dark_cells(&buf, cell) > 0, "{w}x{h} v{version}: tile {index} missing");
                    assert!(
                        (cell.x..cell.right()).all(|x| !buf[(x, cell.y)].symbol().contains("QR needs")),
                        "tile {index} must be a code, not the enlarge message"
                    );
                }
            }
        }
    }
```

- [ ] **Step 4: `TuiOptical` uses the shared layout (`src/tui/optical.rs`)**

1. Add the imports `ratatui::layout::Rect` and `crate::tui::layout::{self, ScreenSize, Tiles}`.
2. In `UiEvent::Show`, replace the `compact: bool` field and its doc with:

```rust
        /// After the link: the side pane becomes the peer monitor and the
        /// codes are tiled.
        linked: bool,
        /// The locked QR version, which sizes the tile grid when no image is
        /// on screen yet.
        version: u8,
```

3. Replace the struct, `new`, `paint_images`'s `Show` construction, `set_version` and `tile_count` with:

```rust
pub struct TuiOptical {
    camera: Arc<CameraFeed>,
    events: Sender<UiEvent>,
    version: u8,
    invert: bool,
    /// Set once the link is up.
    linked: bool,
    /// The area the UI last drew, for the tile count.
    screen: Arc<ScreenSize>,
    tiles: Tiles,
}

impl TuiOptical {
    pub fn new(
        camera: Arc<CameraFeed>,
        events: Sender<UiEvent>,
        invert: bool,
        screen: Arc<ScreenSize>,
        tiles: Tiles,
    ) -> Self {
        Self {
            camera,
            events,
            version: 10,
            invert,
            linked: false,
            screen,
            tiles,
        }
    }
```

In `paint_images`:

```rust
        self.send(UiEvent::Show {
            images,
            invert: self.invert,
            label,
            linked: self.linked,
            version: self.version,
            drawn,
        })?;
```

In the `Optical` impl:

```rust
    fn set_version(&mut self, version: u8) {
        self.version = version;
        self.linked = true;
        self.camera.set_multi(true);
    }

    fn tile_count(&self) -> usize {
        if !self.linked {
            return 1;
        }
        let area = self.screen.get().or_else(|| {
            crossterm::terminal::size()
                .ok()
                .map(|(cols, rows)| Rect::new(0, 0, cols, rows))
        });
        area.map_or(1, |area| layout::data_tiles(area, self.tiles, self.version))
    }
```

- [ ] **Step 5: Rewrite `src/tui/mod.rs`**

Replace the whole file with the following. It keeps `max_qr_version`, `wait_for_terminal_size`, `draw_resize_prompt`, `enter`, `restore` and `leave` **verbatim** from the current file: copy those six functions unchanged where the comment says so.

```rust
//! A ratatui front end for the optical link.
//!
//! Terminals differ wildly in size, font, and colour theme, and a QR code is
//! unforgiving about all three. Every frame is laid out by [`layout::plan`]:
//! outgoing codes in a left-hand column at exactly one cell per module on a
//! forced white ground, and a side pane on the right — the camera preview
//! while aiming, then a view of the peer once linked — that is dropped
//! entirely, never squeezed into the codes, when the terminal is too
//! narrow. The protocol thread sizes its DATA tile grid from the same plan.
//!
//! The transfer is not turn-based after aiming. The receiver shows HELLO
//! straight away so the sender has a code to lock onto. The sender presses
//! Enter after lock to begin the handshake, then once more to send the file.
//! Handshake probes, progress reports, reconnects and the receive side then
//! run unattended.

pub mod camera;
pub mod gate;
pub mod layout;
pub mod optical;
pub mod widget;

use std::{
    io::{self, Stdout},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use image::GrayImage;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};

use crate::{
    live, qr,
    tui::{
        camera::CameraFeed,
        gate::{Gate, GateSpec, KeyAction},
        layout::{Phase, Screen, ScreenSize, Tiles, BORDER, FLOOR_CHROME_ROWS},
        optical::{GateReply, TuiOptical, UiEvent},
        widget::{PreviewWidget, QrGridWidget, QrWidget},
    },
    Error, Result,
};

/// How many transcript lines to keep.
const LOG_LINES: usize = 200;

// KEEP VERBATIM from the current file: the doc comment and body of
// `pub fn max_qr_version`, `fn wait_for_terminal_size`, `fn draw_resize_prompt`.

/// How aiming ends before the protocol thread starts the handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// Sender: tracking must lock, then Enter starts the handshake.
    EnterWhenLocked,
    /// Receiver: handshake starts at once. Its HELLO is the only code on
    /// either screen before the link, so it must not wait for a lock of its
    /// own: the sender shows nothing until it reads HELLO, and tracking only
    /// locks on a decoded code. The HELLO loop waits for probes on its own.
    Immediate,
}

impl Aim {
    /// The sender fills the screen with DATA codes; the receiver only ever
    /// shows one progress report at a time.
    fn tiles(self) -> Tiles {
        match self {
            Aim::EnterWhenLocked => Tiles::Fill,
            Aim::Immediate => Tiles::One,
        }
    }
}

struct Ui {
    title: String,
    camera_index: u32,
    /// The largest code this terminal agreed to display. The aiming layout
    /// is sized for it from the first draw, so it does not lurch when the
    /// first QR appears or when a smaller version is negotiated.
    max_version: u8,
    images: Vec<Arc<GrayImage>>,
    invert: bool,
    label: String,
    status: String,
    log: Vec<String>,
    gate: Option<Gate>,
    done: Option<String>,
    /// After the link: tiled codes and the peer monitor.
    linked: bool,
    /// The locked QR version once linked.
    version: u8,
    tiles: Tiles,
}

impl Ui {
    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_LINES {
            self.log.remove(0);
        }
    }
}

/// Runs `work` on a protocol thread with the TUI on the main thread.
///
/// `work` receives an [`Optical`](crate::optical::Optical) bound to this
/// display and the largest QR version the terminal can draw, and returns the
/// line to leave on screen when it finishes.
pub fn run<F>(title: &str, camera_index: u32, no_invert: bool, aim: Aim, work: F) -> Result<()>
where
    F: FnOnce(TuiOptical, u8) -> Result<String> + Send + 'static,
{
    live::install_ctrlc_handler();

    // Open the camera before taking over the screen so a permission prompt or
    // device error is readable in the normal terminal.
    let feed = Arc::new(CameraFeed::open(camera_index)?);

    let mut terminal = enter()?;
    let max_version = match wait_for_terminal_size(&mut terminal) {
        Ok(version) => version,
        Err(err) => {
            leave(&mut terminal);
            return Err(err);
        }
    };

    let tiles = aim.tiles();
    let screen = Arc::new(ScreenSize::default());
    let (events_tx, events_rx) = mpsc::channel::<UiEvent>();
    let worker_feed = Arc::clone(&feed);
    let worker_screen = Arc::clone(&screen);
    let worker = std::thread::spawn(move || {
        let mut optical = TuiOptical::new(worker_feed, events_tx.clone(), no_invert, worker_screen, tiles);
        let confirmed = {
            use crate::optical::Optical;
            match aim {
                Aim::EnterWhenLocked => optical.gate_locked(&format!(
                    "Aim camera {camera_index} at the other terminal until tracking locks, then press Enter"
                )),
                Aim::Immediate => {
                    optical.set_status(&format!(
                        "Showing HELLO. Aim camera {camera_index} at the sender; receive starts on its own"
                    ));
                    Ok(true)
                }
            }
        };
        let result = match confirmed {
            Ok(true) => work(optical, max_version),
            Ok(false) => Err(Error::Aborted),
            Err(err) => Err(err),
        };
        let summary = match &result {
            Ok(summary) => format!("done — {summary}"),
            Err(err) => format!("failed — {err}"),
        };
        let _ = events_tx.send(UiEvent::Finished { summary });
        result
    });

    let ui_result = event_loop(
        &mut terminal,
        &feed,
        &screen,
        Ui {
            title: title.to_string(),
            camera_index,
            max_version,
            images: Vec::new(),
            invert: no_invert,
            label: String::new(),
            status: String::new(),
            log: Vec::new(),
            gate: None,
            done: None,
            linked: false,
            version: max_version,
            tiles,
        },
        &events_rx,
    );
    leave(&mut terminal);

    // Drop the receiver before joining. If the operator quit while a frame or
    // a gate was in flight, the protocol thread is parked on the reply channel
    // for a message the UI will never answer; dropping the queue disconnects
    // those senders and lets it unwind instead of deadlocking the join.
    drop(events_rx);

    let work_result = worker
        .join()
        .unwrap_or_else(|_| Err(Error::Message("protocol thread panicked".into())));
    // The protocol's own error is the interesting one; a UI error only
    // matters if the protocol somehow succeeded anyway.
    work_result.and_then(|summary| {
        ui_result?;
        println!("{summary}");
        Ok(())
    })
}

// KEEP VERBATIM from the current file: `fn enter`, `fn restore`, `fn leave`.

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    feed: &CameraFeed,
    screen: &ScreenSize,
    mut ui: Ui,
    events: &mpsc::Receiver<UiEvent>,
) -> Result<()> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        let mut dirty = false;

        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                dirty = true;
                let quit = matches!(key.code, KeyCode::Char('q'))
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c')));
                if ui.done.is_some() {
                    return Ok(());
                }
                if quit {
                    // Quitting is an interrupt, not a decline: leave the gate
                    // unanswered so the protocol thread unwinds through
                    // `check_interrupted` and the process exits 130.
                    live::interrupt();
                    ui.gate = None;
                    return Ok(());
                }
                let action = ui.gate.as_ref().map_or(KeyAction::Ignore, |gate| {
                    gate::on_key(&gate.spec, key.code, feed.locked(), Instant::now())
                });
                let reply = match action {
                    KeyAction::Proceed => Some(GateReply::Proceed),
                    KeyAction::Abort => Some(GateReply::Abort),
                    KeyAction::Ignore => None,
                };
                if let Some(reply) = reply {
                    if let Some(gate) = ui.gate.take() {
                        let _ = gate.reply.send(reply);
                    }
                }
            } else {
                dirty = true;
            }
        }

        // Drain messages, but stop at a Show: its acknowledgement must follow
        // the draw that actually put it on screen.
        let mut pending_draw_ack = None;
        loop {
            match events.try_recv() {
                Ok(UiEvent::Show {
                    images,
                    invert,
                    label,
                    linked,
                    version,
                    drawn,
                }) => {
                    ui.images = images;
                    ui.invert = invert;
                    ui.label = label;
                    ui.linked = linked;
                    ui.version = version;
                    pending_draw_ack = Some(drawn);
                    dirty = true;
                    break;
                }
                Ok(UiEvent::Status(status)) => {
                    ui.status = status;
                    dirty = true;
                }
                Ok(UiEvent::Log(line)) => {
                    ui.push_log(line);
                    dirty = true;
                }
                Ok(UiEvent::Gate {
                    prompt,
                    reply,
                    require_lock,
                    auto_on_lock,
                }) => {
                    ui.gate = Some(Gate {
                        prompt,
                        reply,
                        spec: GateSpec {
                            require_lock,
                            auto_on_lock,
                            armed_at: Instant::now(),
                        },
                    });
                    dirty = true;
                }
                Ok(UiEvent::Finished { summary }) => {
                    ui.push_log(summary.clone());
                    ui.done = Some(summary);
                    ui.gate = None;
                    dirty = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.done.is_none() {
                        return Ok(());
                    }
                    break;
                }
            }
        }

        if dirty || last_draw.elapsed() >= Duration::from_millis(80) {
            let view = FeedView {
                counters: feed.counters(),
                locked: feed.locked(),
                preview: feed.preview(),
            };
            terminal.draw(|frame| {
                screen.set(frame.area());
                draw(frame, &ui, &view);
            })?;
            last_draw = Instant::now();
        }
        if ui.done.is_none()
            && ui
                .gate
                .as_ref()
                .is_some_and(|gate| gate::auto_proceed(&gate.spec, feed.locked()))
        {
            if let Some(gate) = ui.gate.take() {
                let _ = gate.reply.send(GateReply::Proceed);
            }
        }
        if let Some(ack) = pending_draw_ack {
            let _ = ack.send(());
        }
        if !dirty {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// What the camera thread has to say, sampled once per draw.
struct FeedView {
    counters: (u64, u64),
    locked: bool,
    preview: Option<Arc<GrayImage>>,
}

/// Terminal cells a rendered code occupies: one column per module, two
/// modules per row.
fn cells_of(image: &GrayImage) -> (u16, u16) {
    let modules = qr::modules_of(image) as u16;
    (modules, modules.div_ceil(2))
}

fn draw(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView) {
    // Linked frames are sized by what is actually on screen: a re-handshake
    // after a fallback shows probes larger than the locked version.
    let (phase, tile) = if ui.linked {
        let tile = ui
            .images
            .first()
            .map(|image| cells_of(image))
            .unwrap_or_else(|| qr::cell_size_for_version(ui.version));
        (Phase::Linked, tile)
    } else {
        (Phase::Aiming, qr::cell_size_for_version(ui.max_version))
    };
    let screen = layout::plan(frame.area(), phase, ui.tiles, tile);
    draw_title(frame, ui, view, &screen);
    draw_codes(frame, ui, &screen);
    draw_side(frame, view, &screen);
    draw_log(frame, ui, &screen);
    draw_prompt(frame, ui, view, &screen);
}

fn draw_title(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let (frames, decodes) = view.counters;
    let spans = vec![
        Span::styled(
            format!(" {} ", ui.title),
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  cam {}  ·  ", ui.camera_index)),
        if view.locked {
            Span::styled("● tracking", Style::new().fg(Color::Green))
        } else {
            Span::styled("○ searching", Style::new().fg(Color::Yellow))
        },
        Span::raw(format!(
            "  ·  {frames} frames  ·  {decodes} decodes{}",
            if ui.label.is_empty() {
                String::new()
            } else {
                format!("  ·  showing {}", ui.label)
            }
        )),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), screen.title);
}

fn draw_codes(frame: &mut ratatui::Frame, ui: &Ui, screen: &Screen) {
    if screen.chrome.qr_border > 0 {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" showing {} ", ui.label)),
            screen.qr_frame,
        );
    }
    if ui.linked {
        frame.render_widget(
            QrGridWidget {
                images: &ui.images,
                invert: ui.invert,
                grid: screen.grid,
            },
            screen.qr,
        );
    } else {
        frame.render_widget(
            QrWidget {
                image: ui.images.first().map(|image| image.as_ref()),
                invert: ui.invert,
            },
            screen.qr,
        );
    }
}

fn draw_side(frame: &mut ratatui::Frame, view: &FeedView, screen: &Screen) {
    let Some(side) = screen.side else {
        return;
    };
    let inner = if screen.chrome.qr_border > 0 {
        let block = Block::default().borders(Borders::ALL).title(" camera ");
        let inner = block.inner(side);
        frame.render_widget(block, side);
        inner
    } else {
        side
    };
    frame.render_widget(
        PreviewWidget {
            frame: view.preview.as_deref(),
        },
        inner,
    );
}

fn draw_log(frame: &mut ratatui::Frame, ui: &Ui, screen: &Screen) {
    if screen.chrome.log == 0 {
        return;
    }
    let visible = screen.log.height.saturating_sub(BORDER) as usize;
    let start = ui.log.len().saturating_sub(visible);
    frame.render_widget(
        Paragraph::new(
            ui.log[start..]
                .iter()
                .map(|line| Line::raw(line.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" transcript ")),
        screen.log,
    );
}

fn draw_prompt(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let hints = match (&ui.done, &ui.gate) {
        (Some(_), _) => "any key: exit",
        (None, Some(gate)) if gate.spec.require_lock && !view.locked => {
            "keep aiming until ● tracking   Esc: abort   q: quit"
        }
        (None, Some(gate)) if gate.spec.auto_on_lock => "Esc: abort   q: quit",
        (None, Some(_)) => "Enter: continue   Esc: abort   q: quit",
        (None, None) => "q: quit",
    };
    let (prompt_style, mut prompt) = match (&ui.done, &ui.gate) {
        (Some(summary), _) => (Style::new().fg(Color::Cyan), summary.clone()),
        (None, Some(gate)) if gate.spec.require_lock && !view.locked => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            if gate.spec.auto_on_lock {
                "▶ Aim at the other terminal until tracking locks — receive starts on its own".into()
            } else {
                "▶ Aim at the other terminal until tracking locks, then press Enter".into()
            },
        ),
        (None, Some(gate)) if gate.spec.require_lock && view.locked => (
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            format!("▶ Tracking locked. {}", gate.prompt),
        ),
        (None, Some(gate)) => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            format!("▶ {}", gate.prompt),
        ),
        (None, None) => (
            Style::new().fg(Color::DarkGray),
            if ui.status.is_empty() {
                "working…".to_string()
            } else {
                ui.status.clone()
            },
        ),
    };
    if screen.chrome.footer == 0 {
        // No footer row to spare: fold the key hints into the prompt so the
        // operator can still see that Enter is what moves things along.
        prompt = format!("{prompt}   [{hints}]");
    }
    let prompt_widget = Paragraph::new(prompt).style(prompt_style).wrap(Wrap { trim: true });
    frame.render_widget(
        if screen.chrome.prompt > 1 {
            prompt_widget.block(Block::default().borders(Borders::ALL).title(" prompt "))
        } else {
            prompt_widget
        },
        screen.prompt,
    );
    if screen.chrome.footer > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(hints, Style::new().fg(Color::DarkGray))),
            screen.footer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    // KEEP VERBATIM: `max_qr_version_only_reserves_rows_the_layout_cannot_give_up`
    // and `max_qr_version_reports_the_minimum_terminal_size`. The two
    // `Chrome` tests moved to `layout.rs` in Step 1; delete them here.

    fn ui_for(linked: bool, images: Vec<Arc<GrayImage>>, tiles: Tiles) -> Ui {
        Ui {
            title: "t".into(),
            camera_index: 0,
            max_version: 10,
            images,
            invert: false,
            label: String::new(),
            status: String::new(),
            log: Vec::new(),
            gate: None,
            done: None,
            linked,
            version: 10,
            tiles,
        }
    }

    fn render(cols: u16, rows: u16, ui: &Ui) -> ratatui::buffer::Buffer {
        let view = FeedView {
            counters: (0, 0),
            locked: false,
            preview: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|frame| draw(frame, ui, &view)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &ratatui::buffer::Buffer) -> String {
        let area = buf.area;
        (area.y..area.bottom())
            .map(|y| (area.x..area.right()).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn code_at(buf: &ratatui::buffer::Buffer, x: u16) -> bool {
        (0..buf.area.height).any(|y| {
            let cell = &buf[(x, y)];
            cell.bg == Color::White && matches!(cell.symbol(), "█" | "▀" | "▄")
        })
    }

    #[test]
    fn while_aiming_the_code_is_on_the_left_and_the_camera_on_the_right() {
        let image = Arc::new(qr::encode_version(b"AX-aim", 10).unwrap());
        let buf = render(148, 38, &ui_for(false, vec![image], Tiles::Fill));
        assert!(code_at(&buf, 10));
        assert!((65..148).all(|x| !code_at(&buf, x)), "nothing scannable in the side pane");
        assert!(text(&buf).contains("waiting for camera"));
    }

    #[test]
    fn a_narrow_terminal_drops_the_side_pane_and_keeps_the_code_whole() {
        let image = Arc::new(qr::encode_version(b"AX-narrow", 10).unwrap());
        let buf = render(80, 38, &ui_for(false, vec![image], Tiles::Fill));
        let text = text(&buf);
        assert!(!text.contains("waiting for camera"));
        assert!(!text.contains("QR needs"), "the code must not be clipped");
        assert!(code_at(&buf, 0) || code_at(&buf, 7));
    }
}
```

- [ ] **Step 6: Build and test**

Run: `cargo build && cargo test tui:: && cargo test 2>&1 | grep "test result"`
Expected: no warnings, and every `tui::` test passes, including `every_allocated_tile_is_drawn_whole` and both frame tests.

- [ ] **Step 7: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/tui/mod.rs
git status --short
```

---

### Task 8: Camera — screen-hold dedupe, epoch, peer crop, cheaper linked frames

**Files:**
- Modify: `src/detect.rs`, `src/tui/optical.rs`
- Rewrite: `src/tui/camera.rs`

**Interfaces:**
- Consumes: `detect::{Tracker, rectify, order_quad, Quad}`, `frame::{decode, foreign_version, PROTOCOL_VERSION}`, `tui::layout::Tiles`.
- Produces:
  - In `detect`: `Quad::area() -> f32`, `pub fn hull_quad(&[(f32, f32)]) -> Option<Quad>`, `Tracker::region() -> Option<Quad>`
  - In `tui::camera`:
    - `pub const MONITOR_CROP: u32 = 96`, `pub const SCREEN_HOLD: Duration` (1500 ms)
    - `#[derive(Clone)] pub struct PeerCrop { pub image: Arc<GrayImage>, pub epoch: u64, pub at: Instant }`
    - `CameraFeed` methods: `peer_crop() -> Option<PeerCrop>`, `epoch() -> u64`, `foreign_version() -> Option<u8>`, `set_linked(bool)`
    - Existing methods are kept: `take_next`, `preview`, `failure`, `counters`, `locked`, `set_multi`, `decode_gap_ms`, `open`

- [ ] **Step 1: Write the failing detector tests**

Append to `src/detect.rs` tests:

```rust
    #[test]
    fn the_hull_of_several_codes_spans_all_of_them() {
        let left = [(100.0, 100.0), (200.0, 100.0), (200.0, 200.0), (100.0, 200.0)];
        let right = [(400.0, 110.0), (500.0, 110.0), (500.0, 210.0), (400.0, 210.0)];
        let quad = hull_quad(&[left, right].concat()).unwrap();
        assert_eq!(
            quad.corners,
            [(100.0, 100.0), (500.0, 110.0), (500.0, 210.0), (100.0, 200.0)]
        );
    }

    #[test]
    fn points_on_a_line_have_no_hull() {
        assert!(hull_quad(&[(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)]).is_none());
        assert!(hull_quad(&[(0.0, 0.0), (1.0, 1.0)]).is_none());
    }

    #[test]
    fn the_tracker_remembers_where_it_last_read_the_peer() {
        let code = qr::encode_version(b"AX-region", 10).unwrap();
        let quad = keystone(500.0, 0.2);
        let canvas = project_onto_canvas(&code, &quad, 1920, 1080);
        let blank = GrayImage::from_pixel(1920, 1080, Luma([190]));
        let mut tracker = Tracker::new();
        assert!(tracker.region().is_none());
        tracker.decode(&canvas).unwrap();
        let region = tracker.region().expect("a decode leaves a region");
        for _ in 0..MAX_MISSES {
            tracker.decode(&blank);
        }
        assert!(!tracker.locked());
        assert_eq!(tracker.region(), Some(region), "the last place is kept for the monitor");
    }
```

Run: `cargo test detect::` → expected to fail to compile because `hull_quad` and `region` are not defined.

- [ ] **Step 2: Implement the detector additions**

In `src/detect.rs`:

1. Add this to `impl Quad`:

```rust
    /// Enclosed area in square pixels (shoelace formula).
    pub fn area(&self) -> f32 {
        let c = &self.corners;
        ((0..4)
            .map(|i| {
                let (x0, y0) = c[i];
                let (x1, y1) = c[(i + 1) % 4];
                x0 * y1 - x1 * y0
            })
            .sum::<f32>()
            / 2.0)
            .abs()
    }
```

2. Add this free function after `order_quad`:

```rust
/// The quad spanning every point: top-left is the point with the smallest
/// `x + y`, top-right the largest `x - y`, and so on round. Used when the
/// sender tiles several codes, so the monitor shows the whole grid.
pub fn hull_quad(points: &[(f32, f32)]) -> Option<Quad> {
    if points.len() < 3 {
        return None;
    }
    let pick = |key: fn(&(f32, f32)) -> f32, largest: bool| {
        points.iter().copied().fold(points[0], |best, p| {
            let better = if largest { key(&p) > key(&best) } else { key(&p) < key(&best) };
            if better {
                p
            } else {
                best
            }
        })
    };
    let quad = Quad {
        corners: [
            pick(|p| p.0 + p.1, false),
            pick(|p| p.0 - p.1, true),
            pick(|p| p.0 + p.1, true),
            pick(|p| p.0 - p.1, false),
        ],
    };
    (quad.area() >= 1.0).then_some(quad)
}
```

3. Add a `region: Option<Quad>` field to `Tracker` (it derives `Default`). Add the method:

```rust
    /// Where the peer's code(s) were last read, kept after the lock drops so
    /// the monitor can go on showing the last place it saw them.
    pub fn region(&self) -> Option<Quad> {
        self.region
    }
```

4. In `decode`:
   - In the locked branch, after `self.quad = project_back(...).or(Some(quad));`, add `self.region = self.quad;`.
   - In the full-frame branch, replace `self.quad = order_quad(&points);` with:

```rust
        self.quad = order_quad(&points);
        self.region = self.quad.or(self.region);
```

5. In `decode_all`, inside `if !found.is_empty() {`, after the existing `self.quad` update, add:

```rust
            let region = if found.len() == 1 {
                found.first().and_then(|(_, points)| order_quad(points))
            } else {
                let all: Vec<(f32, f32)> = found.iter().flat_map(|(_, points)| points.iter().copied()).collect();
                hull_quad(&all)
            };
            self.region = region.or(self.region);
```

Run: `cargo test detect::` → all pass.

- [ ] **Step 3: Rewrite `src/tui/camera.rs`**

Replace the whole file with:

```rust
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use image::{imageops::FilterType, GrayImage};
use nokhwa::{
    pixel_format::LumaFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};

use crate::{
    detect::{self, Tracker},
    frame::{self, Payload},
    live::camera_error,
    Error, Result,
};

/// Width of the downscaled copy kept for the on-screen preview.
const PREVIEW_WIDTH: u32 = 320;
/// Payloads the protocol thread can lag by before the oldest is dropped.
const DECODE_QUEUE: usize = 64;
/// Side of the square, rectified crop of the peer's code kept for the
/// monitor. Small on purpose: it is drawn a few dozen cells wide, and
/// warping it costs next to nothing per frame.
pub const MONITOR_CROP: u32 = 96;
/// How long a decoded code counts as still on the peer's screen. Reads of
/// it within this window are the camera looking again, not news: they
/// neither queue the payload twice nor change the monitor's tint.
pub const SCREEN_HOLD: Duration = Duration::from_millis(1500);

/// Payload fingerprints read recently, and when each was last read.
#[derive(Default)]
struct ScreenMemory {
    seen: HashMap<u32, Instant>,
}

impl ScreenMemory {
    /// Whether `bytes` is news: not read at any time in the last
    /// [`SCREEN_HOLD`]. Every read refreshes the entry, so a code that stays
    /// on screen stays old however long it is held, while one that left and
    /// came back (a resend) is news again.
    fn is_fresh(&mut self, bytes: &[u8], now: Instant) -> bool {
        self.seen
            .retain(|_, at| now.saturating_duration_since(*at) < SCREEN_HOLD);
        self.seen.insert(crc32fast::hash(bytes), now).is_none()
    }
}

/// The monitor's view of the peer: rectified camera pixels around the
/// peer's code, never a regenerated code.
#[derive(Clone)]
pub struct PeerCrop {
    pub image: Arc<GrayImage>,
    /// Distinct payloads decoded when the crop was taken. Changes only when
    /// the peer shows something new.
    pub epoch: u64,
    /// When the frame it came from was decoded.
    pub at: Instant,
}

/// What one camera frame produced.
#[derive(Default)]
struct FrameOutcome {
    /// Payloads that are news, in decode order.
    fresh: Vec<Payload>,
    /// Successful decodes of any kind, repeats included.
    decodes: u64,
    /// Version byte of a frame from a peer speaking another protocol.
    foreign: Option<u8>,
    /// Rectified camera pixels around what decoded, if anything did.
    crop: Option<GrayImage>,
}

fn process_frame(
    gray: &GrayImage,
    tracker: &mut Tracker,
    multi: bool,
    memory: &mut ScreenMemory,
    now: Instant,
) -> FrameOutcome {
    let decoded: Vec<Vec<u8>> = if multi {
        tracker.decode_all(gray)
    } else {
        tracker.decode(gray).into_iter().collect()
    };
    let mut outcome = FrameOutcome::default();
    for bytes in &decoded {
        match frame::decode(bytes) {
            Ok(payload) => {
                outcome.decodes += 1;
                if memory.is_fresh(bytes, now) {
                    outcome.fresh.push(payload);
                }
            }
            Err(_) => {
                if let Some(version) = frame::foreign_version(bytes) {
                    outcome.foreign = Some(version);
                }
            }
        }
    }
    if outcome.decodes > 0 {
        outcome.crop = tracker
            .region()
            .map(|quad| detect::rectify(gray, &quad, MONITOR_CROP));
    }
    outcome
}

fn enqueue(queue: &mut VecDeque<Payload>, payload: Payload) {
    if queue.len() >= DECODE_QUEUE {
        queue.pop_front();
    }
    queue.push_back(payload);
}

#[derive(Default)]
struct Shared {
    /// Payloads that were news, in decode order.
    decoded: Mutex<VecDeque<Payload>>,
    decodes: AtomicU64,
    frames: AtomicU64,
    /// Exponential moving average of the gap between frames that decoded
    /// anything, in milliseconds. Zero until two such frames have landed.
    decode_gap_ms: AtomicU64,
    /// Distinct payloads decoded so far.
    epoch: AtomicU64,
    preview: Mutex<Option<Arc<GrayImage>>>,
    crop: Mutex<Option<PeerCrop>>,
    failure: Mutex<Option<String>>,
    /// Version byte last seen on a frame this build cannot decode; 0 = none.
    foreign: AtomicU8,
    /// Whether the peer's screen is currently being tracked in the frame.
    locked: AtomicBool,
    /// Decode every QR in the frame: the peer is tiling.
    multi: AtomicBool,
    /// The link is up: the preview is no longer drawn.
    linked: AtomicBool,
    stop: AtomicBool,
}

/// A webcam owned by one dedicated thread for its whole life, publishing a
/// preview, a crop of the peer's code, and decoded payloads.
pub struct CameraFeed {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
    pub index: u32,
}

impl CameraFeed {
    /// Opens camera `index` on its own thread and blocks until the first
    /// frame arrives, so a permission or device error surfaces before the
    /// TUI takes over the terminal.
    pub fn open(index: u32) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let handle = thread::spawn(move || {
            if let Err(err) = capture_loop(index, &worker) {
                if let Ok(mut slot) = worker.failure.lock() {
                    *slot = Some(err.to_string());
                }
            }
        });
        let mut feed = Self {
            shared,
            handle: Some(handle),
            index,
        };
        feed.wait_for_first_frame()?;
        Ok(feed)
    }

    fn wait_for_first_frame(&mut self) -> Result<()> {
        for _ in 0..600 {
            if let Some(err) = self.failure() {
                return Err(Error::Camera(err));
            }
            if self.shared.frames.load(Ordering::Relaxed) > 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err(Error::Camera(format!(
            "camera {} produced no frames within 12s",
            self.index
        )))
    }

    /// The next payload that was news when the camera read it.
    pub fn take_next(&self) -> Option<Payload> {
        self.shared.decoded.lock().ok()?.pop_front()
    }

    pub fn preview(&self) -> Option<Arc<GrayImage>> {
        self.shared.preview.lock().ok()?.clone()
    }

    /// The last crop of the peer's code, however old.
    pub fn peer_crop(&self) -> Option<PeerCrop> {
        self.shared.crop.lock().ok()?.clone()
    }

    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::Relaxed)
    }

    /// The protocol version of a peer this build cannot talk to, if one has
    /// been seen.
    pub fn foreign_version(&self) -> Option<u8> {
        match self.shared.foreign.load(Ordering::Relaxed) {
            0 => None,
            version => Some(version),
        }
    }

    pub fn failure(&self) -> Option<String> {
        self.shared.failure.lock().ok()?.clone()
    }

    /// `(frames captured, QR codes decoded)` since the feed opened.
    pub fn counters(&self) -> (u64, u64) {
        (
            self.shared.frames.load(Ordering::Relaxed),
            self.shared.decodes.load(Ordering::Relaxed),
        )
    }

    pub fn locked(&self) -> bool {
        self.shared.locked.load(Ordering::Relaxed)
    }

    /// Switch between single-code tracking and decoding every QR in the
    /// frame (a tiling peer).
    pub fn set_multi(&self, multi: bool) {
        self.shared.multi.store(multi, Ordering::Relaxed);
    }

    /// Once linked the preview is not drawn, so stop paying to downscale it.
    pub fn set_linked(&self, linked: bool) {
        self.shared.linked.store(linked, Ordering::Relaxed);
    }

    /// Typical milliseconds between frames that decoded something.
    pub fn decode_gap_ms(&self) -> Option<u64> {
        match self.shared.decode_gap_ms.load(Ordering::Relaxed) {
            0 => None,
            gap => Some(gap),
        }
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn capture_loop(index: u32, shared: &Shared) -> Result<()> {
    let format = RequestedFormat::new::<LumaFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = Camera::new(CameraIndex::Index(index), format).map_err(camera_error)?;
    camera.open_stream().map_err(camera_error)?;

    // The lids sit at different angles, so the peer's screen is a small
    // trapezoid somewhere in a wide frame. The tracker finds it once and then
    // decodes rectified crops of that region.
    let mut tracker = Tracker::new();
    let mut memory = ScreenMemory::default();
    let mut last_decode: Option<Instant> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        let buffer = camera.frame().map_err(camera_error)?;
        let Ok(gray) = buffer.decode_image::<LumaFormat>() else {
            continue;
        };
        shared.frames.fetch_add(1, Ordering::Relaxed);

        if !shared.linked.load(Ordering::Relaxed) {
            if let Ok(mut slot) = shared.preview.lock() {
                *slot = Some(Arc::new(downscale(&gray)));
            }
        }

        let now = Instant::now();
        let multi = shared.multi.load(Ordering::Relaxed);
        let outcome = process_frame(&gray, &mut tracker, multi, &mut memory, now);
        shared.locked.store(tracker.locked(), Ordering::Relaxed);
        if let Some(version) = outcome.foreign {
            shared.foreign.store(version, Ordering::Relaxed);
        }
        if outcome.decodes > 0 {
            shared.decodes.fetch_add(outcome.decodes, Ordering::Relaxed);
            if let Some(previous) = last_decode.replace(now) {
                let gap = now.duration_since(previous).as_millis() as u64;
                let smoothed = match shared.decode_gap_ms.load(Ordering::Relaxed) {
                    0 => gap,
                    current => (current * 3 + gap) / 4,
                };
                shared.decode_gap_ms.store(smoothed.max(1), Ordering::Relaxed);
            }
        }
        if !outcome.fresh.is_empty() {
            shared
                .epoch
                .fetch_add(outcome.fresh.len() as u64, Ordering::Relaxed);
            if let Ok(mut queue) = shared.decoded.lock() {
                for payload in outcome.fresh {
                    enqueue(&mut queue, payload);
                }
            }
        }
        if let Some(crop) = outcome.crop {
            let peer = PeerCrop {
                image: Arc::new(crop),
                epoch: shared.epoch.load(Ordering::Relaxed),
                at: now,
            };
            if let Ok(mut slot) = shared.crop.lock() {
                *slot = Some(peer);
            }
        }
    }
    Ok(())
}

fn downscale(frame: &GrayImage) -> GrayImage {
    if frame.width() <= PREVIEW_WIDTH {
        return frame.clone();
    }
    let height = (frame.height() * PREVIEW_WIDTH / frame.width()).max(1);
    image::imageops::resize(frame, PREVIEW_WIDTH, height, FilterType::Nearest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr;
    use image::Luma;

    /// A camera frame of a code on a mid-grey desk. Every pixel carries the
    /// small offsets a real sensor has: nothing is pure black or white.
    fn camera_frame(bytes: &[u8], version: u8) -> GrayImage {
        let code = qr::encode_version(bytes, version).unwrap();
        let mut frame = GrayImage::from_pixel(900, 700, Luma([150]));
        for y in 0..code.height() {
            for x in 0..code.width() {
                let sensor = if code.get_pixel(x, y)[0] < 128 {
                    20 + (x % 5) as u8
                } else {
                    225 + (y % 5) as u8
                };
                frame.put_pixel(200 + x, 120 + y, Luma([sensor]));
            }
        }
        frame
    }

    #[test]
    fn downscale_preserves_aspect_ratio_and_caps_width() {
        let wide = GrayImage::new(1920, 1080);
        let small = downscale(&wide);
        assert_eq!(small.width(), PREVIEW_WIDTH);
        assert_eq!(small.height(), 1080 * PREVIEW_WIDTH / 1920);
        assert_eq!(downscale(&GrayImage::new(64, 48)).dimensions(), (64, 48));
    }

    #[test]
    fn repeated_reads_of_an_unchanged_code_are_not_news() {
        let mut memory = ScreenMemory::default();
        let t0 = Instant::now();
        assert!(memory.is_fresh(b"A", t0));
        assert!(!memory.is_fresh(b"A", t0 + Duration::from_millis(100)));
        assert!(memory.is_fresh(b"B", t0 + Duration::from_millis(100)), "a different code is news");
        assert!(
            !memory.is_fresh(b"A", t0 + Duration::from_millis(1400)),
            "still on screen: each read refreshes it"
        );
        assert!(
            memory.is_fresh(b"A", t0 + Duration::from_millis(1400) + SCREEN_HOLD),
            "gone for a whole hold and back again is a resend"
        );
    }

    #[test]
    fn a_frame_yields_its_payload_once_and_a_crop_of_real_camera_pixels() {
        let payload = Payload::Ok { transfer_id: 9 };
        let frame = camera_frame(&frame::encode(&payload).unwrap(), 10);
        let mut tracker = Tracker::new();
        let mut memory = ScreenMemory::default();
        let t0 = Instant::now();

        let first = process_frame(&frame, &mut tracker, false, &mut memory, t0);
        assert_eq!(first.fresh, vec![payload]);
        assert_eq!(first.decodes, 1);
        let crop = first.crop.expect("a decode leaves a crop");
        assert_eq!(crop.dimensions(), (MONITOR_CROP, MONITOR_CROP));
        assert!(
            crop.pixels().all(|p| p[0] != 0 && p[0] != 255),
            "a regenerated code would be pure black and white; this must be camera pixels"
        );
        assert!(crop.pixels().any(|p| p[0] < 80) && crop.pixels().any(|p| p[0] > 180));

        let again = process_frame(&frame, &mut tracker, false, &mut memory, t0 + Duration::from_millis(50));
        assert!(again.fresh.is_empty(), "the same code still on screen is not news");
        assert_eq!(again.decodes, 1);
        assert!(again.crop.is_some(), "the monitor still refreshes from the live frame");
    }

    #[test]
    fn a_peer_on_another_protocol_version_is_reported_not_decoded() {
        let mut bytes = frame::encode(&Payload::Ok { transfer_id: 9 }).unwrap();
        bytes[2] = 1;
        let frame = camera_frame(&bytes, 10);
        let outcome = process_frame(
            &frame,
            &mut Tracker::new(),
            false,
            &mut ScreenMemory::default(),
            Instant::now(),
        );
        assert!(outcome.fresh.is_empty());
        assert_eq!(outcome.foreign, Some(1));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_payload() {
        let mut queue = VecDeque::new();
        for first in 0..=DECODE_QUEUE as u32 {
            enqueue(
                &mut queue,
                Payload::Data {
                    transfer_id: 1,
                    first,
                    count: 1,
                    bytes: vec![first as u8],
                },
            );
        }
        assert_eq!(queue.len(), DECODE_QUEUE);
        assert!(matches!(queue.front(), Some(Payload::Data { first: 1, .. })));
    }
}
```

- [ ] **Step 4: `TuiOptical` uses the new feed (`src/tui/optical.rs`)**

1. Add `frame::PROTOCOL_VERSION` to the imports.
2. In `set_version`, replace `self.camera.set_multi(true);` with:

```rust
        self.camera.set_linked(true);
        // Only the receiver watches a peer that tiles DATA. The sender watches
        // one progress code, which tracked single-code decoding reads far
        // more cheaply than a full-frame multi-code scan every frame.
        self.camera.set_multi(self.tiles == Tiles::One);
```

3. In `poll`, directly after the `failure()` check, add:

```rust
            if let Some(peer) = self.camera.foreign_version() {
                return Err(Error::ProtocolMismatch {
                    peer,
                    ours: PROTOCOL_VERSION,
                });
            }
```

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test detect:: tui:: && cargo test 2>&1 | grep "test result"`
Expected: everything passes with no warnings.

- [ ] **Step 6: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/detect.rs src/tui/mod.rs
git status --short
```

---

### Task 9: The fuzzy ASCII peer monitor

**Files:**
- Create: `src/tui/monitor.rs`
- Modify: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `tui::camera::PeerCrop` (Task 8); `Screen.side` and `Phase::Linked` (Task 7).
- Produces:
  - `pub const RAMP: &[u8]`, `pub const STALE_AFTER: Duration` (1200 ms)
  - `Freshness { Live, Stale }`
  - `freshness(crop_at: Instant, now: Instant, locked: bool) -> Freshness`
  - `tint(epoch: u64) -> Color`
  - `ascii_rows(&GrayImage, cols: u16, rows: u16) -> Vec<String>`
  - `MonitorWidget<'a> { crop: Option<&'a PeerCrop>, locked: bool, now: Instant }`

- [ ] **Step 1: Write the failing tests**

Create `src/tui/monitor.rs` containing only these tests, and add `pub mod monitor;` to `src/tui/mod.rs` after `pub mod layout;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr;
    use image::Luma;
    use std::sync::Arc;

    fn crop(value: u8, epoch: u64, at: Instant) -> PeerCrop {
        PeerCrop {
            image: Arc::new(GrayImage::from_pixel(96, 96, Luma([value]))),
            epoch,
            at,
        }
    }

    #[test]
    fn the_rendering_is_ascii_density_never_a_scannable_block_code() {
        let code = qr::encode_version(b"AX-monitor", 10).unwrap();
        let rows = ascii_rows(&code, 60, 30);
        assert_eq!(rows.len(), 24);
        assert!(rows.iter().all(|row| row.len() == 48));
        assert!(rows.iter().flat_map(|row| row.bytes()).all(|b| RAMP.contains(&b)));
        assert!(!rows.concat().chars().any(|c| matches!(c, '█' | '▀' | '▄')));
    }

    #[test]
    fn dark_pixels_draw_dense_and_light_pixels_draw_blank() {
        let dark = GrayImage::from_pixel(96, 96, Luma([0]));
        let light = GrayImage::from_pixel(96, 96, Luma([255]));
        assert!(ascii_rows(&dark, 20, 10).concat().chars().all(|c| c == '@'));
        assert!(ascii_rows(&light, 20, 10).concat().chars().all(|c| c == ' '));
        assert!(ascii_rows(&dark, 1, 10).is_empty(), "no room for a square");
    }

    #[test]
    fn the_tint_changes_only_when_the_epoch_does() {
        assert_eq!(tint(3), tint(3));
        assert_ne!(tint(3), tint(4));
    }

    #[test]
    fn the_view_goes_stale_when_tracking_is_lost_or_decodes_stop() {
        let t0 = Instant::now();
        assert_eq!(freshness(t0, t0 + Duration::from_millis(100), true), Freshness::Live);
        assert_eq!(freshness(t0, t0 + Duration::from_millis(100), false), Freshness::Stale);
        assert_eq!(
            freshness(t0, t0 + STALE_AFTER + Duration::from_millis(1), true),
            Freshness::Stale
        );
    }

    #[test]
    fn a_stale_view_keeps_the_last_crop_dimmed_and_says_so() {
        let t0 = Instant::now();
        let last = crop(0, 5, t0);
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        MonitorWidget {
            crop: Some(&last),
            locked: false,
            now: t0 + Duration::from_secs(3),
        }
        .render(area, &mut buf);
        let header: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(header.contains("STALE") && header.contains("reacquiring"), "{header}");
        let body = &buf[(20, 2)];
        assert_eq!(body.symbol(), "@", "the last crop is still drawn");
        assert_eq!(body.fg, Color::DarkGray);
        assert!(body.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_live_view_is_tinted_by_epoch() {
        let t0 = Instant::now();
        let current = crop(0, 5, t0);
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        MonitorWidget {
            crop: Some(&current),
            locked: true,
            now: t0,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(20, 2)].fg, tint(5));
        assert!(!buf[(20, 2)].modifier.contains(Modifier::DIM));
    }
}
```

Run: `cargo test tui::monitor::` → expected to fail to compile.

- [ ] **Step 2: Implement**

Put this above the tests in `src/tui/monitor.rs`:

```rust
//! The peer monitor: a fuzzy ASCII view of what this camera actually sees of
//! the other screen, once the link is up.
//!
//! It is drawn from the rectified camera crop, never from the decoded
//! payload, so glare, blur and misalignment show as they are. It is
//! deliberately *not* a QR code: blurred ASCII density on the terminal's own
//! background, so the peer's camera — pointed straight at this screen — can
//! never decode it as one of our codes.
//!
//! Its tint changes only when the camera decodes a payload it has not just
//! seen, so a code held on screen does not look like progress. When tracking
//! drops or decodes stop, the last crop stays up, dimmed and labelled STALE.

use std::time::{Duration, Instant};

use image::GrayImage;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::tui::camera::PeerCrop;

/// Density ramp, lightest first.
pub const RAMP: &[u8] = b" .:-=+*#%@";
/// A crop older than this, or any crop while tracking is lost, is stale.
pub const STALE_AFTER: Duration = Duration::from_millis(1200);
/// Blur applied before sampling, so modules smear into each other.
const BLUR_SIGMA: f32 = 1.2;
/// Tallest the view is drawn; beyond this it only gets coarser to look at.
const MAX_ROWS: u16 = 24;
const TINTS: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::LightBlue,
    Color::LightRed,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Live,
    Stale,
}

pub fn freshness(crop_at: Instant, now: Instant, locked: bool) -> Freshness {
    if locked && now.saturating_duration_since(crop_at) <= STALE_AFTER {
        Freshness::Live
    } else {
        Freshness::Stale
    }
}

/// The tint for a crop taken at `epoch`.
pub fn tint(epoch: u64) -> Color {
    TINTS[(epoch % TINTS.len() as u64) as usize]
}

/// Blurred, area-averaged ASCII rendering of `crop` in at most `cols` x
/// `rows` cells, kept square on screen (cells are about twice as tall as
/// they are wide).
pub fn ascii_rows(crop: &GrayImage, cols: u16, rows: u16) -> Vec<String> {
    let rows = rows.min(cols / 2).min(MAX_ROWS);
    let cols = rows * 2;
    if rows == 0 || crop.width() == 0 || crop.height() == 0 {
        return Vec::new();
    }
    let soft = image::imageops::blur(crop, BLUR_SIGMA);
    let (w, h) = soft.dimensions();
    let (rows, cols) = (u32::from(rows), u32::from(cols));
    (0..rows)
        .map(|row| {
            let y0 = row * h / rows;
            let y1 = ((row + 1) * h / rows).max(y0 + 1).min(h);
            (0..cols)
                .map(|col| {
                    let x0 = col * w / cols;
                    let x1 = ((col + 1) * w / cols).max(x0 + 1).min(w);
                    let mut sum = 0u32;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            sum += u32::from(soft.get_pixel(x, y)[0]);
                        }
                    }
                    let mean = sum / ((x1 - x0) * (y1 - y0));
                    let darkness = 255 - mean as usize;
                    RAMP[darkness * (RAMP.len() - 1) / 255] as char
                })
                .collect()
        })
        .collect()
}

fn clip(text: &str, width: u16) -> String {
    text.chars().take(usize::from(width)).collect()
}

pub struct MonitorWidget<'a> {
    pub crop: Option<&'a PeerCrop>,
    pub locked: bool,
    pub now: Instant,
}

impl Widget for MonitorWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(crop) = self.crop else {
            buf.set_string(
                area.x,
                area.y,
                clip("peer view · waiting for the first decode", area.width),
                Style::new().fg(Color::DarkGray),
            );
            return;
        };
        let (label, label_style, body_style) = match freshness(crop.at, self.now, self.locked) {
            Freshness::Live => (
                "● peer view · live".to_string(),
                Style::new().fg(tint(crop.epoch)).add_modifier(Modifier::BOLD),
                Style::new().fg(tint(crop.epoch)),
            ),
            Freshness::Stale => (
                format!(
                    "◌ STALE {}s · reacquiring",
                    self.now.saturating_duration_since(crop.at).as_secs()
                ),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            ),
        };
        buf.set_string(area.x, area.y, clip(&label, area.width), label_style);
        let rows = ascii_rows(&crop.image, area.width, area.height.saturating_sub(1));
        let width = rows.first().map_or(0, |row| row.len() as u16);
        let x = area.x + (area.width - width) / 2;
        for (i, line) in rows.iter().enumerate() {
            buf.set_string(x, area.y + 1 + i as u16, line, body_style);
        }
    }
}
```

Run: `cargo test tui::monitor::` → 6 tests pass.

- [ ] **Step 3: Put the monitor in the side pane (`src/tui/mod.rs`)**

1. Imports: add `camera::{CameraFeed, PeerCrop}` (replacing `camera::CameraFeed`), `monitor::{freshness, tint, Freshness, MonitorWidget}`, and `layout` to the `tui::{...}` list.
2. Add `crop: Option<PeerCrop>` and `now: Instant` to `FeedView`, and set them in `event_loop` with `crop: feed.peer_crop()` and `now: Instant::now()`.
3. In `draw`, change the call to `draw_side(frame, ui, view, &screen);`.
4. Replace `draw_side` with:

```rust
fn draw_side(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let Some(side) = screen.side else {
        return;
    };
    if ui.linked {
        // After the link the full preview gives way to a view of just the
        // peer's code, and it stays this way through any reconnect.
        frame.render_widget(
            MonitorWidget {
                crop: view.crop.as_ref(),
                locked: view.locked,
                now: view.now,
            },
            side,
        );
        return;
    }
    let inner = if screen.chrome.qr_border > 0 {
        let block = Block::default().borders(Borders::ALL).title(" camera ");
        let inner = block.inner(side);
        frame.render_widget(block, side);
        inner
    } else {
        side
    };
    frame.render_widget(
        PreviewWidget {
            frame: view.preview.as_deref(),
        },
        inner,
    );
}
```

5. In `draw_title`, change `let spans = vec![` to `let mut spans = vec![`, and add this before `frame.render_widget(...)`. It keeps the state visible even when a narrow terminal hides the side pane:

```rust
    if ui.linked {
        spans.push(match &view.crop {
            Some(crop) if freshness(crop.at, view.now, view.locked) == Freshness::Live => {
                Span::styled("  ·  peer view live", Style::new().fg(tint(crop.epoch)))
            }
            _ => Span::styled(
                "  ·  peer view STALE, reacquiring",
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        });
    }
```

6. In the tests:
   - Update `render` to build `FeedView { counters: (0, 0), locked: false, preview: None, crop: None, now: Instant::now() }`.
   - Add `fn render_with(cols, rows, ui, view: &FeedView)`, a variant taking an explicit view, and have `render` call it.
   - Append:

```rust
    #[test]
    fn once_linked_the_preview_becomes_a_monitor_that_goes_stale_visibly() {
        let image = Arc::new(qr::encode_version(b"AX-linked", 10).unwrap());
        let ui = ui_for(true, vec![image], Tiles::One);
        let t0 = Instant::now();
        let view = FeedView {
            counters: (0, 0),
            locked: false,
            preview: None,
            crop: Some(PeerCrop {
                image: Arc::new(GrayImage::from_pixel(96, 96, image::Luma([0]))),
                epoch: 2,
                at: t0,
            }),
            now: t0 + Duration::from_secs(2),
        };
        let buf = render_with(148, 38, &ui, &view);
        let text = text(&buf);
        assert!(!text.contains("waiting for camera"), "no full preview after the link");
        assert!(text.contains("STALE"), "{text}");
        assert!(text.contains("reacquiring"));
        assert!((65..148).all(|x| !code_at(&buf, x)), "the monitor is never a scannable code");
    }
```

- [ ] **Step 4: Build and test**

Run: `cargo build && cargo test tui:: && cargo test 2>&1 | grep "test result"`
Expected: everything passes with no warnings.

- [ ] **Step 5: Format and checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/tui/mod.rs
git status --short
```

---

### Task 10: Benchmark, operator notes, help text, and full verification

**Files:**
- Create: `examples/decoder_bench.rs`, `docs/operator-notes.md`
- Modify: `src/cli.rs` (help text plus one guard test)

**Interfaces:**
- Consumes: the public `airgap_xfer::{qr, detect, frame, chunks}` APIs.
- Produces: `cargo run --release --example decoder_bench`, and the operator notes.

- [ ] **Step 1: Write the guard test against new CLI options**

Append to `src/cli.rs` tests:

```rust
    #[test]
    fn no_command_line_options_were_added() {
        use clap::CommandFactory;
        let cli = Cli::command();
        let names = |sub: &str| -> Vec<String> {
            let mut names: Vec<String> = cli
                .find_subcommand(sub)
                .unwrap()
                .get_arguments()
                .map(|arg| arg.get_id().to_string())
                .filter(|id| id != "help" && id != "version")
                .collect();
            names.sort();
            names
        };
        assert_eq!(names("send"), ["camera", "keep_temp", "no_invert", "path"]);
        assert_eq!(names("recv"), ["camera", "force", "keep_temp", "no_invert", "outdir"]);
    }
```

Run: `cargo test cli::` → this passes immediately. It is a regression guard, not a red/green test.

- [ ] **Step 2: Update the help text**

Replace `SIZE_NOTE` in `src/cli.rs` with:

```rust
/// A version 10 QR code is 65 columns by 33 rows once drawn two modules per
/// terminal cell, and it cannot be shrunk further without becoming
/// undecodable. The TUI sheds its borders, footer, and transcript to fit, so
/// the hard floor is the code itself plus a title and a prompt line.
const SIZE_NOTE: &str = "Both terminals need at least 65x35 cells: the smallest usable QR code is \
65x33 on its own, and a clipped code cannot be read by the other camera. If \
the window is smaller, airgap-xfer waits and asks you to zoom out (Ctrl/- or \
Cmd/-) or stretch the window. Outgoing codes stay in the left column. While \
aiming, the right side shows the camera; once linked it becomes a fuzzy view \
of the other side's code as your camera sees it. It changes colour when a new \
code is read and is labelled STALE when tracking drops. On a narrow terminal \
that pane is hidden rather than squeezing the codes. Aim the sender until \
tracking locks, press Enter, then Enter once more to send; the receiver \
accepts on its own. If the link drops (camera covered, lid bumped, fallback \
to a smaller code) both sides reconnect by themselves and keep what was \
already received, as long as both stay open. Esc aborts, q quits.";
```

- [ ] **Step 3: Write the benchmark**

Create `examples/decoder_bench.rs`:

```rust
//! Synthetic decoder benchmark: how long the camera thread spends per frame
//! on each decode path, measured on generated 1080p frames instead of a
//! webcam, so the numbers are repeatable on any machine.
//!
//!     cargo run --release --example decoder_bench
//!
//! Set BENCH_ITERS to change the sample count (default 15).

use std::time::{Duration, Instant};

use airgap_xfer::{
    chunks::CHUNK_BYTES,
    detect::{self, Homography, Quad, Tracker},
    frame::{self, Payload},
    qr,
};
use image::{GrayImage, Luma};

fn main() {
    let iters = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15usize)
        .max(1);
    println!("{iters} iterations per row, 1920x1080 synthetic frames");
    println!("{:<40} {:>10} {:>10} {:>8}", "path", "median ms", "p90 ms", "ok");
    for version in [10u8, 25, 40] {
        let code = qr::encode_version(&full_data(version), version).unwrap();
        let quad = keystone(520.0, 0.25);
        let canvas = project(&code, &quad, 1920, 1080);
        report(&format!("v{version} full-frame scan"), iters, || {
            qr::decode_with_points(&canvas).is_ok()
        });
        let mut tracker = Tracker::new();
        let _ = tracker.decode(&canvas);
        report(&format!("v{version} tracked crop"), iters, || tracker.decode(&canvas).is_some());
        report(&format!("v{version} monitor crop ({} px)", 96), iters, || {
            detect::rectify(&canvas, &quad, 96).width() == 96
        });
    }
    let tile = qr::encode_version(&full_data(15), 15).unwrap();
    let grid = tiled(&tile, 2, 2, 1920, 1080);
    report("4 x v15 tiles, decode_all", iters, || qr::decode_all(&grid).len() == 4);
}

fn full_data(version: u8) -> Vec<u8> {
    let count = frame::chunks_per_frame(version);
    frame::encode(&Payload::Data {
        transfer_id: 0x0123_4567_89ab_cdef,
        first: 0,
        count: count as u8,
        bytes: (0..count as usize * CHUNK_BYTES).map(|i| (i * 37) as u8).collect(),
    })
    .unwrap()
}

fn report(name: &str, iters: usize, mut op: impl FnMut() -> bool) {
    let mut times: Vec<Duration> = Vec::with_capacity(iters);
    let mut ok = 0;
    for _ in 0..iters {
        let start = Instant::now();
        if op() {
            ok += 1;
        }
        times.push(start.elapsed());
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let median = times[times.len() / 2];
    let p90 = times[(times.len() * 9 / 10).min(times.len() - 1)];
    println!("{name:<40} {:>10.2} {:>10.2} {:>5}/{iters}", ms(median), ms(p90), ok);
}

/// A square screen of `size` pixels whose far edge is `tilt` shorter than
/// its near edge — the keystone two laptop lids produce.
fn keystone(size: f32, tilt: f32) -> Quad {
    let (cx, cy) = (960.0, 540.0);
    let far = size / 2.0 * (1.0 - tilt);
    Quad {
        corners: [
            (cx - far, cy - size / 2.0),
            (cx + far, cy - size / 2.0),
            (cx + size / 2.0, cy + size / 2.0),
            (cx - size / 2.0, cy + size / 2.0),
        ],
    }
}

fn project(code: &GrayImage, quad: &Quad, w: u32, h: u32) -> GrayImage {
    let inverse = Homography::square_to_quad(quad).invert().expect("non-degenerate quad");
    let mut canvas = GrayImage::from_pixel(w, h, Luma([190]));
    for y in 0..h {
        for x in 0..w {
            let Some((u, v)) = inverse.map(x as f32 + 0.5, y as f32 + 0.5) else {
                continue;
            };
            if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                let sx = ((u * code.width() as f32) as u32).min(code.width() - 1);
                let sy = ((v * code.height() as f32) as u32).min(code.height() - 1);
                canvas.put_pixel(x, y, *code.get_pixel(sx, sy));
            }
        }
    }
    canvas
}

fn tiled(tile: &GrayImage, across: u32, down: u32, w: u32, h: u32) -> GrayImage {
    let mut canvas = GrayImage::from_pixel(w, h, Luma([190]));
    let gap = 24;
    for row in 0..down {
        for col in 0..across {
            let (x0, y0) = (100 + col * (tile.width() + gap), 60 + row * (tile.height() + gap));
            for y in 0..tile.height() {
                for x in 0..tile.width() {
                    canvas.put_pixel(x0 + x, y0 + y, *tile.get_pixel(x, y));
                }
            }
        }
    }
    canvas
}
```

Run: `cargo build --release --example decoder_bench && cargo run --release --example decoder_bench`
Expected: a table of 10 rows. Save the full output; Step 6 copies it into the notes.

- [ ] **Step 4: Run the full verification and capture outputs**

Run each command below and keep its output for Step 6:

```bash
cargo build --release 2>&1 | tail -3
cargo test 2>&1 | grep -E "test result|FAILED|panicked"
cargo test -- --test-threads=1 2>&1 | grep -E "test result|FAILED"
for i in 1 2 3 4 5; do cargo test session:: transport:: 2>&1 | grep "test result"; done
rustfmt --edition 2021 --check src/chunks.rs src/resume.rs src/deliver.rs src/frame.rs src/error.rs src/qr.rs src/optical.rs src/link.rs src/transport.rs src/session.rs src/cli.rs src/live.rs src/detect.rs src/tui/mod.rs examples/decoder_bench.rs && echo "rustfmt: touched files clean"
cargo fmt --check 2>&1 | grep '^Diff in' | sed 's/:[0-9]*:$//' | sort -u
cargo clippy --all-targets 2>&1 | grep -A2 -E '^warning' | grep -E '^warning|-->'
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps 2>&1 | tail -2
git diff --quiet -- Cargo.toml Cargo.lock bin/ install.sh install.ps1 src/pack.rs src/main.rs && echo "untouched files unchanged"
git status --short
```

Expected:
- **Build:** succeeds.
- **Tests:** all pass, in both the parallel and single-threaded runs, and all 5 repeat runs of `session::` and `transport::` pass.
- **Touched-file rustfmt:** prints `rustfmt: touched files clean`.
- **`cargo fmt --check`:** lists only `src/pack.rs` and `src/lib.rs`. These are pre-existing drift in files not formatted by policy.
- **Clippy:** shows only the allowed pre-existing warnings.
- **Rustdoc:** finishes with no error.
- **Untouched files:** prints `untouched files unchanged`.

If a test fails, use @superpowers:systematic-debugging; do not paper over it.

- [ ] **Step 5: Run the link checks for the docs**

After writing the notes in Step 6, run:

```bash
grep -oE '\]\(([^)#]+)' docs/operator-notes.md | sed 's/^](//' | grep -v '^https\?:' | while read -r target; do
  [ -e "docs/$target" ] || echo "BROKEN: $target"
done; echo "markdown link check done"
```
Expected: only `markdown link check done`, with no `BROKEN:` lines.

- [ ] **Step 6: Write `docs/operator-notes.md`**

Create the file with the content below. Replace every `⟪…⟫` with the exact value from Steps 3–4: counts, warning lines, and the benchmark table pasted verbatim. Leave nothing in `⟪…⟫` form.

````markdown
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

Measured on ⟪machine / OS / CPU from `uname -a` and the date⟫:

```
⟪paste the full benchmark output from Task 10 Step 3⟫
```

Run it on both laptops and compare. If the monitor goes STALE during steady
transfer while the benchmark shows long decode times, the camera thread is
the bottleneck.

## Verification status

### Automated results (this repository, one machine)

| Check | Command | Result |
|---|---|---|
| Release build | `cargo build --release` | ⟪pass/fail⟫ |
| Test suite | `cargo test` | ⟪N passed, M failed⟫ |
| Single-threaded | `cargo test -- --test-threads=1` | ⟪N passed, M failed⟫ |
| Repeat run (protocol) | `cargo test session:: transport::` ×5 | ⟪5/5 green, or which failed⟫ |
| Formatting (changed files) | `rustfmt --edition 2021 --check …` | ⟪clean / diff⟫ |
| Formatting (whole crate) | `cargo fmt --check` | ⟪files listed — pre-existing drift⟫ |
| Lint | `cargo clippy --all-targets` | ⟪warning lines verbatim⟫ |
| Doc links | `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps` | ⟪pass/fail⟫ |
| Markdown links | relative-link check over this file | ⟪pass/fail⟫ |

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
````

Then run Step 5's link check.

- [ ] **Step 7: Final checkpoint (no commit)**

```bash
rustfmt --edition 2021 src/cli.rs examples/decoder_bench.rs
cargo test 2>&1 | grep "test result"
git status --short
git diff --stat
```

Expected:
- `git status --short` shows only the files in the File Map, as `M` or `??`.
- `bin/`, `Cargo.toml`, `Cargo.lock`, `src/pack.rs`, `src/main.rs`, `.superpowers/` and the older docs are absent from the list.
- Nothing is staged or committed.

In the hand-off message, report the automated results and the outstanding two-device checks as separate lists, copied from `docs/operator-notes.md`.

---

## Self-Review Notes

Each spec requirement maps to the task that implements it:

| Spec requirement | Task(s) |
|---|---|
| Codes on the left | 7 (`qr_frame.x == area.x` invariant) |
| Full preview while aiming | 7 |
| ASCII monitor after link, from real camera pixels | 8 (crop) + 9 (render) |
| Tint changes per new payload; no animation on repeats | 8 (`ScreenMemory`, epoch) + 9 (`tint`) |
| Stale and reacquiring state, dimmed | 9 |
| High-contrast codes | unchanged `QrWidget`; 9 asserts the monitor is never scannable |
| Shared layout for rendering and tile allocation | 7 (`plan`, `data_tiles`, `ScreenSize`) |
| Hide the preview rather than clip | 7 |
| Confirmations: sender locks, receiver auto-accepts, guards | 6, plus 4 (no re-prompt) |
| Resume after loss and fallback, in memory | 2, 3, 4 |
| Version-independent 96-byte chunks; adjacent packing; 32-chunk bitmaps | 1, 3 |
| Transfer identity scoping and full offer validation | 2, 3 |
| Duplicates never inflate progress or override bytes | 1, 3 |
| Preserved handshake starts | 3 (`Error::Restart`) + 4 (`Replay`) |
| SHA-256 retained | 3, 5 |
| OK only after return, sync and unpack | 3, 5 |
| No new transport, GUI, dependencies or options | Global Constraints, 10 (guard test, `git diff` checks) |
| Deterministic tests for every listed area | 1–9 |
| Formatting, tests and link checks | 10 |
| Operator notes and synthetic benchmark | 10 |
| Automated results separated from two-device checks | 10 |
| Uncommitted; nothing unrelated touched | header, Global Constraints, 10 |

Two design decisions were made without asking:
1. **The sender yields a tile column for the monitor only when it has ≥ 3 columns.** Throughput wins at 1–2 columns, and the title line still shows staleness.
2. **The transfer id is derived from the offer plus a per-process nonce.** A restarted sender therefore never resumes into a receiver's old cache, which matches "while both applications remain open".
