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
pub fn plan_spans(
    have: &ChunkSet,
    per_frame: u32,
    limit: u32,
    max_frames: usize,
) -> Vec<(u32, u32)> {
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
        let count = chunk_count(size)?;
        let len = usize::try_from(size).ok()?;
        let mut blob = Vec::new();
        blob.try_reserve_exact(len).ok()?;
        blob.resize(len, 0);
        Some(Self {
            size,
            blob,
            have: ChunkSet::new(count),
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
        assert_eq!(
            set.apply_report(4, &[u32::MAX]),
            Some(0),
            "repeats add nothing"
        );
    }

    #[test]
    fn spans_pack_adjacent_holes_lowest_first() {
        let mut have = ChunkSet::new(40);
        for index in [0, 1, 5, 6, 7] {
            have.insert(index);
        }
        assert_eq!(plan_spans(&have, 4, 40, 3), vec![(2, 3), (8, 4), (12, 4)]);
        assert_eq!(
            plan_spans(&have, 4, 10, 8),
            vec![(2, 3), (8, 2)],
            "never past the limit"
        );
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
            Ok(Accepted {
                new: 1,
                ..Accepted::default()
            })
        );
        assert_eq!(
            store.accept(0, 1, &first),
            Ok(Accepted {
                duplicate: 1,
                ..Accepted::default()
            })
        );
        assert_eq!(
            store.accept(0, 1, &[0xEE; 96]),
            Ok(Accepted {
                conflict: 1,
                ..Accepted::default()
            })
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
            Ok(Accepted {
                new: 2,
                duplicate: 1,
                conflict: 0
            })
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
    fn new_store_rejects_sizes_that_cannot_be_numbered_or_held() {
        assert!(ChunkStore::new(u64::from(u32::MAX) * 96 + 1).is_none());
        assert!(ChunkStore::new(u64::MAX).is_none());
    }

    #[test]
    fn every_report_gets_a_new_serial() {
        let mut store = ChunkStore::new(10).unwrap();
        let first = store.next_serial();
        assert_ne!(first, store.next_serial());
    }
}
