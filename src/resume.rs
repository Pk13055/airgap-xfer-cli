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
        assert!(
            cache.store(&a).is_none(),
            "the old transfer's chunks are gone"
        );
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
