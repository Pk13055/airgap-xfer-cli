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
    if bytes.len() >= ENVELOPE_OVERHEAD && bytes.starts_with(b"AX") && bytes[2] != PROTOCOL_VERSION
    {
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
        assert_eq!(
            SUPPORTED_VERSIONS.map(chunks_per_frame),
            [1, 2, 4, 7, 9, 17]
        );
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
        assert_eq!(
            foreign_version(&encode(&Payload::Ok { transfer_id: 1 }).unwrap()),
            None
        );
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
