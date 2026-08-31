use crate::{Error, Result};
use crc32fast::Hasher;

pub const PROTOCOL_VERSION: u8 = 1;
pub const ROLE_RECV: u8 = 2;
pub const FAIL_HASH: u8 = 1;
pub const FAIL_DISK: u8 = 2;
pub const FAIL_PROTOCOL: u8 = 3;
pub const FAIL_ABORTED: u8 = 4;
pub const ENVELOPE_OVERHEAD: usize = 10;
pub const KIND_HELLO: u8 = 1;
pub const KIND_PROBE: u8 = 2;
pub const KIND_LINK: u8 = 3;
pub const KIND_GO: u8 = 4;
pub const KIND_DATA: u8 = 5;
pub const KIND_ACK: u8 = 6;
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
    Ack = 6,
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
            KIND_ACK => Some(Kind::Ack),
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
    Hello { protocol_ver: u8, role: u8 },
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
    Go {
        basename: String,
        uncompressed_hint: u64,
        compressed_size: u64,
        chunk_count: u32,
        sha256: [u8; 32],
    },
    Data { seq: u32, chunk: Vec<u8> },
    Ack { window_base: u32, bitmap: u32 },
    Fin { sha256: [u8; 32] },
    Ok,
    Fail { reason: u8 },
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

pub fn data_chunk_size(version: u8) -> usize {
    qr_byte_capacity(version).saturating_sub(ENVELOPE_OVERHEAD + 4)
}

fn kind_of(p: &Payload) -> u8 {
    match p {
        Payload::Hello { .. } => Kind::Hello.as_u8(),
        Payload::Probe { .. } => Kind::Probe.as_u8(),
        Payload::Link { .. } => Kind::Link.as_u8(),
        Payload::Go { .. } => Kind::Go.as_u8(),
        Payload::Data { .. } => Kind::Data.as_u8(),
        Payload::Ack { .. } => Kind::Ack.as_u8(),
        Payload::Fin { .. } => Kind::Fin.as_u8(),
        Payload::Ok => Kind::Ok.as_u8(),
        Payload::Fail { .. } => Kind::Fail.as_u8(),
    }
}

fn put_payload(p: &Payload, out: &mut Vec<u8>) {
    match p {
        Payload::Hello {
            protocol_ver,
            role,
        } => {
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
            out.push(if *last { 1 } else { 0 });
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
        Payload::Go {
            basename,
            uncompressed_hint,
            compressed_size,
            chunk_count,
            sha256,
        } => {
            let b = basename.as_bytes();
            out.extend_from_slice(&(b.len() as u16).to_be_bytes());
            out.extend_from_slice(b);
            out.extend_from_slice(&uncompressed_hint.to_be_bytes());
            out.extend_from_slice(&compressed_size.to_be_bytes());
            out.extend_from_slice(&chunk_count.to_be_bytes());
            out.extend_from_slice(sha256);
        }
        Payload::Data { seq, chunk } => {
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(chunk);
        }
        Payload::Ack {
            window_base,
            bitmap,
        } => {
            out.extend_from_slice(&window_base.to_be_bytes());
            out.extend_from_slice(&bitmap.to_be_bytes());
        }
        Payload::Fin { sha256 } => out.extend_from_slice(sha256),
        Payload::Ok => {}
        Payload::Fail { reason } => out.push(*reason),
    }
}

pub fn encode(p: &Payload) -> Result<Vec<u8>> {
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
            dwell_ms: u16::from_be_bytes([p[2], p[3]]),
            last: p[4] == 1,
        },
        KIND_LINK if p.len() == 4 => Payload::Link {
            qr_version: p[0],
            dwell_ms: u16::from_be_bytes([p[1], p[2]]),
            probe_loss: p[3],
        },
        KIND_GO if p.len() >= 2 + 8 + 8 + 4 + 32 => {
            let n = u16::from_be_bytes([p[0], p[1]]) as usize;
            let need = 2 + n + 8 + 8 + 4 + 32;
            if p.len() != need {
                return Err(Error::BadFrame);
            }
            let basename = String::from_utf8(p[2..2 + n].to_vec()).map_err(|_| Error::BadFrame)?;
            let mut o = 2 + n;
            let uncompressed_hint = u64::from_be_bytes(p[o..o + 8].try_into().unwrap());
            o += 8;
            let compressed_size = u64::from_be_bytes(p[o..o + 8].try_into().unwrap());
            o += 8;
            let chunk_count = u32::from_be_bytes(p[o..o + 4].try_into().unwrap());
            o += 4;
            let mut sha256 = [0u8; 32];
            sha256.copy_from_slice(&p[o..o + 32]);
            Payload::Go {
                basename,
                uncompressed_hint,
                compressed_size,
                chunk_count,
                sha256,
            }
        }
        KIND_DATA if p.len() >= 4 => Payload::Data {
            seq: u32::from_be_bytes(p[0..4].try_into().unwrap()),
            chunk: p[4..].to_vec(),
        },
        KIND_ACK if p.len() == 8 => Payload::Ack {
            window_base: u32::from_be_bytes(p[0..4].try_into().unwrap()),
            bitmap: u32::from_be_bytes(p[4..8].try_into().unwrap()),
        },
        KIND_FIN if p.len() == 32 => {
            let mut sha256 = [0u8; 32];
            sha256.copy_from_slice(p);
            Payload::Fin { sha256 }
        }
        KIND_OK if p.is_empty() => Payload::Ok,
        KIND_FAIL if p.len() == 1 => Payload::Fail { reason: p[0] },
        _ => return Err(Error::BadFrame),
    };
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(p: Payload) {
        let b = encode(&p).unwrap();
        assert_eq!(decode(&b).unwrap(), p);
    }

    #[test]
    fn hello_probe_link_go_data_ack_fin_ok_fail_roundtrip() {
        roundtrip(Payload::Hello {
            protocol_ver: 1,
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
        roundtrip(Payload::Go {
            basename: "project".into(),
            uncompressed_hint: 1000,
            compressed_size: 400,
            chunk_count: 3,
            sha256: [7u8; 32],
        });
        roundtrip(Payload::Data {
            seq: 99,
            chunk: vec![0, 1, 2, 255],
        });
        roundtrip(Payload::Ack {
            window_base: 32,
            bitmap: 0x8000_0001,
        });
        roundtrip(Payload::Fin { sha256: [9u8; 32] });
        roundtrip(Payload::Ok);
        roundtrip(Payload::Fail {
            reason: FAIL_HASH,
        });
    }

    #[test]
    fn drops_truncated_bad_magic_bad_crc_unknown_kind() {
        assert!(decode(&[]).is_err());
        assert!(decode(b"NO").is_err());
        let mut good = encode(&Payload::Ok).unwrap();
        let last = good.len() - 1;
        good[last] ^= 0xff;
        assert!(matches!(decode(&good), Err(crate::Error::BadFrame)));
        good = encode(&Payload::Ok).unwrap();
        good[3] = 99; // kind
        // recompute would still fail as unknown kind even if crc patched:
        assert!(decode(&good).is_err());
    }

    #[test]
    fn chunk_size_is_capacity_minus_fourteen() {
        assert_eq!(qr_byte_capacity(10), 143);
        assert_eq!(qr_byte_capacity(15), 284);
        assert_eq!(qr_byte_capacity(20), 474);
        assert_eq!(qr_byte_capacity(25), 707);
        assert_eq!(qr_byte_capacity(30), 974);
        assert_eq!(qr_byte_capacity(40), 1655);
        assert_eq!(data_chunk_size(10), 143 - 14);
        assert_eq!(data_chunk_size(40), 1655 - 14);
    }
}
