# airgap-xfer CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Mac ↔ Windows Rust CLI that packs a path into `.tar.zst`, moves it across an air gap via terminal QR codes and webcams, then restores the directory tree.

**Architecture:** One `airgap-xfer` binary. `send` packs, runs a probe handshake, then block-ACKs DATA frames. `recv` answers HELLO/LINK/ACK, writes the blob, verifies SHA-256, unpacks. All protocol tests use an in-memory `Optical` pair; the webcam is isolated behind `LiveOptical`.

**Tech Stack:** Rust 2021, clap, thiserror, crc32fast, sha2, tar, zstd, qrcode (encode), rxing (decode — see note), image, nokhwa (`input-native`), crossterm, ctrlc.

**Decode crate note:** The spec listed `rqrr`, with `rxing` as fallback. `rqrr` returns UTF-8 `String` and cannot round-trip the binary envelope. Use `rxing` for decode from the first QR task.

## Global Constraints

- Platforms: macOS and Windows only (no Linux in v1).
- No network sockets on the data path. No OpenCV. No Python.
- Protocol version `1`, magic `AX`, all multi-byte fields big-endian.
- QR ECC: Quartile. No extra recovery QRs mixed into DATA.
- Handshake probe table is fixed (versions 10/15/20/25/30/40, dwells 250/250/200/200/150/150 ms).
- Window size 32. Stalled after 20 ACK rounds with no new bits.
- Timeouts: HELLO/PROBE 30s, LINK 5s, GO/ready-ACK/FIN reply 15s, ACK read 2s, post-last-probe quiet 2s.
- Skip symlinks and unreadable files while packing; empty archive is an error.
- `recv` refuses if `outdir/<basename>` exists unless `--force`.
- Temp blob: `std::env::temp_dir()/airgap-xfer-{pid}-{nanos}.tar.zst`. Delete unless `--keep-temp`.
- User asked not to auto-commit the plan file. Implementation commit steps below are optional if the user has asked not to commit.

---

## File map

Create these files. Do not invent others unless a crate forces it (e.g. `Cargo.lock`).

| Path | Responsibility |
|---|---|
| `.gitignore` | `/target`, OS junk |
| `Cargo.toml` | package `airgap-xfer`, deps listed above |
| `src/lib.rs` | module tree + re-exports used by tests |
| `src/main.rs` | `airgap_xfer::cli::run()` |
| `src/error.rs` | `Error`, `Result` |
| `src/frame.rs` | envelope encode/decode, kinds, chunk size |
| `src/pack.rs` | tar+zstd pack/unpack, warnings, dest check |
| `src/qr.rs` | bytes → QR matrix → `GrayImage`; image → bytes via rxing; terminal string |
| `src/optical.rs` | `Optical` trait, `MockOptical`, `Pair`, `Lossy` |
| `src/link.rs` | handshake: `run_send_handshake`, `run_recv_handshake` |
| `src/transport.rs` | block-ACK send/recv of the blob |
| `src/live.rs` | `LiveOptical`: crossterm QR + nokhwa frames |
| `src/cli.rs` | clap, send/recv orchestration, Ctrl-C |
| `install.sh` | copy binary to `$PREFIX/bin` |
| `install.ps1` | copy binary to `%LOCALAPPDATA%\airgap-xfer` and user PATH |

---

### Task 1: Crate, errors, clap skeleton

**Files:**
- Create: `.gitignore`
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/error.rs`
- Create: `src/cli.rs`
- Test: `src/cli.rs` (module tests)

**Interfaces:**
- Consumes: nothing
- Produces: `airgap_xfer::Error`, `airgap_xfer::Result<T>`, `cli::Cli` / `cli::Cmd`, `cli::run() -> Result<()>` (body can be `todo!` except parse + help)

- [ ] **Step 1: Write the failing test**

Create `.gitignore`:

```
/target
.DS_Store
```

Create `Cargo.toml`:

```toml
[package]
name = "airgap-xfer"
version = "0.1.0"
edition = "2021"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
crc32fast = "1.4"
crossterm = "0.28"
ctrlc = "3.4"
image = "0.25"
nokhwa = { version = "0.10", features = ["input-native"] }
qrcode = "0.14"
rxing = "0.7"
sha2 = "0.10"
tar = "0.4"
thiserror = "2"
zstd = "0.13"
```

If `nokhwa 0.10` or `rxing 0.7` fail to resolve, use the latest compatible versions on crates.io that still expose native Mac/Windows capture and raw-byte QR decode. Do not add OpenCV.

Create `src/error.rs`:

```rust
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("point the other webcam at this terminal")]
    HandshakeTimeout,
    #[error("handshake failed")]
    HandshakeFailed,
    #[error("no usable QR version (lighting, distance, or font)")]
    NoUsableProbe,
    #[error("camera: {0}")]
    Camera(String),
    #[error("destination exists: {0} (use --force)")]
    DestExists(PathBuf),
    #[error("empty archive")]
    EmptyArchive,
    #[error("hash mismatch")]
    HashMismatch,
    #[error("stalled: missing seqs {0:?}")]
    Stalled(Vec<u32>),
    #[error("bad frame")]
    BadFrame,
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

Create `src/cli.rs` with clap types only plus this test (the parse will fail until `Cli` exists — write the test first against the intended API):

```rust
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "airgap-xfer", version)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    Send {
        path: PathBuf,
        #[arg(long, default_value_t = 0)]
        camera: u32,
        #[arg(long)]
        keep_temp: bool,
        #[arg(long)]
        no_invert: bool,
    },
    Recv {
        outdir: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        camera: u32,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        keep_temp: bool,
        #[arg(long)]
        no_invert: bool,
    },
}

pub fn run() -> crate::Result<()> {
    let _cli = Cli::parse();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_send_defaults() {
        let cli = Cli::try_parse_from(["airgap-xfer", "send", "./dir"]).unwrap();
        match cli.cmd {
            Cmd::Send {
                path,
                camera,
                keep_temp,
                no_invert,
            } => {
                assert_eq!(path, PathBuf::from("./dir"));
                assert_eq!(camera, 0);
                assert!(!keep_temp);
                assert!(!no_invert);
            }
            _ => panic!("expected send"),
        }
    }

    #[test]
    fn parses_recv_force_and_outdir() {
        let cli = Cli::try_parse_from(["airgap-xfer", "recv", "./in", "--force", "--camera", "1"])
            .unwrap();
        match cli.cmd {
            Cmd::Recv {
                outdir,
                camera,
                force,
                ..
            } => {
                assert_eq!(outdir.as_deref(), Some(std::path::Path::new("./in")));
                assert_eq!(camera, 1);
                assert!(force);
            }
            _ => panic!("expected recv"),
        }
    }
}
```

Create `src/lib.rs`:

```rust
pub mod cli;
pub mod error;

pub use error::{Error, Result};
```

Create `src/main.rs`:

```rust
fn main() {
    if let Err(err) = airgap_xfer::cli::run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

If you truly wrote tests before `Cli`, the first compile fails with `cannot find struct Cli`. After the files above exist, this step is “crate compiles and tests pass” — that is acceptable for scaffold. Run:

```bash
cargo test -q parses_send_defaults parses_recv_force_and_outdir
```

Expected: FAIL if clap types are missing; PASS once Step 3 is done.

- [ ] **Step 3: Write minimal implementation**

The files in Step 1 *are* the implementation. Fill `Cargo.toml` / modules if you stubbed them to fail first.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test -q parses_send_defaults parses_recv_force_and_outdir
```

Expected: PASS. Also:

```bash
cargo run -- send --help
```

Expected: clap help for `send` including `--camera`, `--keep-temp`, `--no-invert`. `recv --help` includes `--force`.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add .gitignore Cargo.toml Cargo.lock src/main.rs src/lib.rs src/error.rs src/cli.rs
git commit -m "Scaffold airgap-xfer crate with clap send/recv."
```

---

### Task 2: Frame envelope

**Files:**
- Create: `src/frame.rs`
- Modify: `src/lib.rs` (add `pub mod frame;`)
- Test: `src/frame.rs`

**Interfaces:**
- Consumes: `crate::Error`
- Produces:
  - `frame::PROTOCOL_VERSION: u8 = 1`
  - `frame::KIND_*` constants and `frame::Kind`
  - `frame::Payload` enum
  - `frame::encode(payload: &Payload) -> Result<Vec<u8>>`
  - `frame::decode(bytes: &[u8]) -> Result<Payload>` (`Error::BadFrame` on drop-worthy input)
  - `frame::qr_byte_capacity(version: u8) -> usize`
  - `frame::data_chunk_size(version: u8) -> usize`  // capacity - 14
  - `frame::FAIL_HASH: u8 = 1`, `FAIL_DISK = 2`, `FAIL_PROTOCOL = 3`, `FAIL_ABORTED = 4`
  - `frame::ROLE_RECV: u8 = 2`
  - `frame::ENVELOPE_OVERHEAD: usize = 10` // magic+ver+kind+len+crc

- [ ] **Step 1: Write the failing test**

Add `src/frame.rs` tests (leave `encode`/`decode` unimplemented so they fail):

```rust
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
        assert_eq!(qr_byte_capacity(10), 174);
        assert_eq!(qr_byte_capacity(15), 292);
        assert_eq!(qr_byte_capacity(20), 415);
        assert_eq!(qr_byte_capacity(25), 583);
        assert_eq!(qr_byte_capacity(30), 709);
        assert_eq!(qr_byte_capacity(40), 1273);
        assert_eq!(data_chunk_size(10), 174 - 14);
        assert_eq!(data_chunk_size(40), 1273 - 14);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- frame::
```

Expected: FAIL compile (`cannot find function encode`) or FAIL asserts.

- [ ] **Step 3: Write minimal implementation**

Implement `src/frame.rs`:

```rust
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

pub fn qr_byte_capacity(version: u8) -> usize {
    match version {
        10 => 174,
        15 => 292,
        20 => 415,
        25 => 583,
        30 => 709,
        40 => 1273,
        _ => 0,
    }
}

pub fn data_chunk_size(version: u8) -> usize {
    qr_byte_capacity(version).saturating_sub(ENVELOPE_OVERHEAD + 4)
}

fn kind_of(p: &Payload) -> u8 {
    match p {
        Payload::Hello { .. } => KIND_HELLO,
        Payload::Probe { .. } => KIND_PROBE,
        Payload::Link { .. } => KIND_LINK,
        Payload::Go { .. } => KIND_GO,
        Payload::Data { .. } => KIND_DATA,
        Payload::Ack { .. } => KIND_ACK,
        Payload::Fin { .. } => KIND_FIN,
        Payload::Ok => KIND_OK,
        Payload::Fail { .. } => KIND_FAIL,
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
```

Add `pub mod frame;` to `src/lib.rs`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- frame::
```

Expected: PASS.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/frame.rs src/lib.rs
git commit -m "Add AX frame envelope encode and decode."
```

---

### Task 3: Pack and unpack `.tar.zst`

**Files:**
- Create: `src/pack.rs`
- Modify: `src/lib.rs`
- Test: `src/pack.rs`

**Interfaces:**
- Consumes: `Error`
- Produces:
  - `pack::Packed { temp_path: PathBuf, basename: String, uncompressed_hint: u64, compressed_size: u64, sha256: [u8; 32], warnings: Vec<String> }`
  - `pack::pack(path: &Path) -> Result<Packed>`
  - `pack::unpack(zst: &Path, outdir: &Path, force: bool) -> Result<PathBuf>` // final `outdir/basename`
  - `pack::remove_temp(path: &Path)`
  - `pack::dest_exists(outdir: &Path, basename: &str) -> bool`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write(p: &Path, bytes: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, bytes).unwrap();
    }

    #[test]
    fn roundtrip_file_tree_empty_dir_and_nuls() {
        let root = std::env::temp_dir().join(format!("ag-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("dir/nested/a.txt"), b"hello");
        write(&root.join("dir/.hidden"), b"h");
        write(&root.join("dir/bin.dat"), &[0, 1, 0, 255]);
        fs::create_dir_all(root.join("dir/empty")).unwrap();

        let packed = pack(&root.join("dir")).unwrap();
        assert_eq!(packed.basename, "dir");
        assert_eq!(packed.compressed_size, fs::metadata(&packed.temp_path).unwrap().len());
        assert!(!packed.sha256.iter().all(|b| *b == 0));

        let out = root.join("out");
        let dest = unpack(&packed.temp_path, &out, false).unwrap();
        assert_eq!(dest, out.join("dir"));
        assert_eq!(fs::read(dest.join("nested/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dest.join(".hidden")).unwrap(), b"h");
        assert_eq!(fs::read(dest.join("bin.dat")).unwrap(), vec![0, 1, 0, 255]);
        assert!(dest.join("empty").is_dir());
        assert!(!packed.temp_path.exists()); // unpack does not delete send temp; send side deletes
        // unpack must not delete the zst (recv deletes after). Recreate: pack::unpack leaves zst.
        assert!(Path::new(&packed.temp_path).exists());
        remove_temp(&packed.temp_path);
        assert!(!packed.temp_path.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_symlink_and_errors_on_empty() {
        let root = std::env::temp_dir().join(format!("ag-pack-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/nope", root.join("link")).unwrap();
            let err = pack(&root).unwrap_err();
            assert!(matches!(err, crate::Error::EmptyArchive));
        }
        #[cfg(not(unix))]
        {
            // Windows: a directory with only a skipped/unreadable story is hard; pack an empty dir
            // still has a directory tar entry, so it must succeed and round-trip.
            let packed = pack(&root).unwrap();
            assert_eq!(packed.basename, root.file_name().unwrap().to_string_lossy());
            remove_temp(&packed.temp_path);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn refuse_existing_basename_without_force() {
        let root = std::env::temp_dir().join(format!("ag-pack-force-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("dir/f"), b"x");
        let packed = pack(&root.join("dir")).unwrap();
        let out = root.join("out");
        unpack(&packed.temp_path, &out, false).unwrap();
        let err = unpack(&packed.temp_path, &out, false).unwrap_err();
        assert!(matches!(err, crate::Error::DestExists(_)));
        unpack(&packed.temp_path, &out, true).unwrap();
        remove_temp(&packed.temp_path);
        let _ = fs::remove_dir_all(&root);
    }
}
```

Fix the first test comment: `unpack` does **not** delete the temp zst. `remove_temp` does. Assert the zst still exists after unpack, then `remove_temp`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- pack::
```

Expected: FAIL (`pack` not found).

- [ ] **Step 3: Write minimal implementation**

`src/pack.rs`:

- `pack`: basename = `path.file_name()`. Walk with `std::fs::read_dir` recursively. Skip symlinks (`symlink_metadata` + `is_symlink()`). Skip unreadable files, push warning strings. Tar headers use paths relative to the parent of `path`, so the root entry is the basename. Append directory entries for dirs (including empty). If zero entries written → `Error::EmptyArchive`. Pipe tar into `zstd::Encoder` at level 3 into the temp file. Then SHA-256 the file. `uncompressed_hint` = sum of file sizes (dirs count 0).
- `unpack`: `zstd::Decoder` + `tar::Archive::unpack` into `outdir/.airgap-xfer-partial/`. Locate `partial/basename`. If `outdir/basename` exists and `!force` → `DestExists` (after deleting partial). If force, `fs::remove_dir_all` or `remove_file` the existing basename. `fs::create_dir_all(outdir)`. Rename `partial/basename` → `outdir/basename`. Remove `.airgap-xfer-partial`.
- `remove_temp`: `fs::remove_file` ignoring NotFound.
- `dest_exists`: `outdir.join(basename).exists()`.

Wire `pub mod pack;` in `lib.rs`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- pack::
```

Expected: PASS.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/pack.rs src/lib.rs
git commit -m "Pack paths to tar.zst and restore directory trees."
```

---

### Task 4: QR encode, rasterize, decode, terminal cells

**Files:**
- Create: `src/qr.rs`
- Modify: `src/lib.rs`
- Test: `src/qr.rs`

**Interfaces:**
- Consumes: `frame::qr_byte_capacity`, `Error`
- Produces:
  - `qr::encode_version(data: &[u8], version: u8) -> Result<image::GrayImage>`  // Quartile, quiet zone ≥ 4 modules, module scale 4px
  - `qr::decode_image(img: &image::GrayImage) -> Result<Vec<u8>>`  // rxing; `BadFrame`/`Message` if none
  - `qr::render_terminal(img: &GrayImage, invert: bool) -> String`  // Unicode `█ ▀ ▄` half-blocks, light quiet zone when `invert == false`

`invert == false` (default): dark modules, light background. `--no-invert` sets `invert == true`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame;

    #[test]
    fn qr_roundtrip_binary_envelope() {
        let payload = frame::Payload::Data {
            seq: 1,
            chunk: (0u8..=80).collect(),
        };
        let bytes = frame::encode(&payload).unwrap();
        let img = encode_version(&bytes, 15).unwrap();
        let got = decode_image(&img).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn terminal_render_contains_blocks_and_newline() {
        let img = encode_version(b"AX-test", 10).unwrap();
        let s = render_terminal(&img, false);
        assert!(s.contains('█') || s.contains('▀') || s.contains('▄'));
        assert!(s.contains('\n'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- qr::
```

Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

Use `qrcode::{EcLevel, QrCode, Version, Color}`. `QrCode::with_version(data, Version::Normal(version as i16), EcLevel::Q)`. Rasterize: quiet zone 4, scale 4, dark = 0, light = 255.

Decode: `rxing::helpers::detect_in_luma(width, height, luma)` (or the current rxing equivalent). Prefer **raw bytes** of the result, not UTF-8 text. If the helper only exposes text, use `getRawBytes()` on the `RXingResult`.

`render_terminal`: walk pixels two rows at a time. For each pair (upper, lower): both dark → `█`, upper dark → `▀`, lower dark → `▄`, else space. Prefix/suffix a row of spaces as quiet zone. If `invert`, swap dark/light meaning.

If `encode_version` data exceeds capacity, return `Error::Message("payload exceeds QR version")`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- qr::
```

Expected: PASS. If rxing cannot decode the synthetic image, increase scale to 8px/module and re-test before changing crates.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/qr.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "Encode and decode binary QR payloads for the terminal."
```

---

### Task 5: Optical trait and in-memory pair

**Files:**
- Create: `src/optical.rs`
- Modify: `src/lib.rs`
- Test: `src/optical.rs`

**Interfaces:**
- Consumes: `frame::{encode, decode, Payload}`
- Produces:
  - `trait Optical { fn show(&mut self, payload: &Payload) -> Result<()>; fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>>; }`
  - `optical::pair() -> (PairEnd, PairEnd)`  // show on A is next poll on B
  - `optical::Lossy<T: Optical>` with `drop_data_seq: HashSet<u32>` — wrap the **receiver** end so `poll` drops listed DATA seqs once each (simulates a missed camera frame)

Prefer dropping on `poll` of the receiving end:

```rust
pub struct Lossy<T> {
    pub inner: T,
    pub drop_data_seq: HashSet<u32>,
}
```

On `poll`, if decoded `Payload::Data { seq, .. }` and set contains seq, drop it once (remove seq from the set) and return `Ok(None)`. Retransmissions of that seq then succeed. For tests, `PairEnd` is instant (timeout ignored except `Duration::ZERO` still returns available).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Payload;
    use std::time::Duration;

    #[test]
    fn pair_delivers_hello() {
        let (mut a, mut b) = pair();
        a.show(&Payload::Hello {
            protocol_ver: 1,
            role: 2,
        })
        .unwrap();
        match b.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Hello { role, .. }) => assert_eq!(role, 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lossy_drops_configured_data_seq() {
        let (mut send, recv) = pair();
        let mut recv = Lossy {
            inner: recv,
            drop_data_seq: [3].into_iter().collect(),
        };
        send.show(&Payload::Data {
            seq: 3,
            chunk: vec![1],
        })
        .unwrap();
        assert!(recv.poll(Duration::from_millis(1)).unwrap().is_none());
        send.show(&Payload::Data {
            seq: 4,
            chunk: vec![2],
        })
        .unwrap();
        match recv.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Data { seq, .. }) => assert_eq!(seq, 4),
            other => panic!("{other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- optical::
```

Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

`PairEnd` holds `std::sync::mpsc::Sender<Vec<u8>>` and `Receiver<Vec<u8>>`. `pair()` creates two channels. `show` encodes via `frame::encode` and sends. `poll` uses `recv_timeout`. `Lossy::poll`: if DATA seq is in `drop_data_seq`, remove it from the set and return `Ok(None)` (one drop per seq).

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- optical::
```

Expected: PASS.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/optical.rs src/lib.rs
git commit -m "Add in-memory optical pair for protocol tests."
```

---

### Task 6: Handshake

**Files:**
- Create: `src/link.rs`
- Modify: `src/lib.rs`
- Test: `src/link.rs`

**Interfaces:**
- Consumes: `Optical`, `frame::{Payload, data_chunk_size}`, `pack::dest_exists`
- Produces:
  - `link::PROBES: [(u8, u8, u16); 6]` // id, version, dwell_ms — exact spec table
  - `#[derive(Clone)] link::Session { qr_version: u8, dwell_ms: u16, basename: String, uncompressed_hint: u64, compressed_size: u64, chunk_count: u32, sha256: [u8; 32] }`
  - `link::LinkConfig { fast: bool }` — `fast: true` uses HELLO 100ms, LINK 50ms, GO 50ms, quiet 20ms, no dwell sleeps; `fast: false` uses spec timeouts
  - `link::run_send_handshake(opt: &mut impl Optical, basename: String, uncompressed_hint: u64, blob: &[u8], sha256: [u8; 32], cfg: LinkConfig) -> Result<Session>`
  - `link::run_recv_handshake(opt: &mut impl Optical, outdir: &Path, force: bool, cfg: LinkConfig) -> Result<Session>`

Send algorithm:

1. Poll 30s (fast: 100ms) until `Hello`. Else `HandshakeTimeout`.
2. For each probe in `PROBES`, `show(Probe { last: id == 5 })`. Sleep `dwell_ms` unless `cfg.fast`.
3. After last probe, poll 5s (fast: 50ms) for `Link`. If none: `HandshakeFailed`. Lock version+dwell from LINK.
4. `chunk_count = ceil(blob.len() / data_chunk_size(qr_version))` (minimum 1 if blob is empty — but pack already rejects empty archives). `show(Go { basename, uncompressed_hint, compressed_size: blob.len() as u64, chunk_count, sha256 })`.
5. Poll 15s (fast: 50ms) for `Ack { window_base: 0, .. }`. Else `HandshakeFailed`.
6. Return `Session`.

Recv algorithm:

1. Loop: `show(Hello { protocol_ver: 1, role: ROLE_RECV })`, poll 50ms, until `Probe` or 30s → `HandshakeTimeout`.
2. Record successful probe versions. Continue until `last==1` or 2s quiet after a probe (in fast mode, 2s → 20ms).
3. If none: `show` nothing; return `NoUsableProbe`.
4. `probe_loss = 100 * (6 - successful_count) / 6`. `show(Link { highest version, that probe's dwell, probe_loss })`.
5. Poll 15s for `Go`. If `dest_exists(outdir, basename) && !force`: `show(Fail { FAIL_ABORTED })`, return `DestExists`.
6. `show(Ack { window_base: 0, bitmap: 0 })`.
7. Return `Session` from GO fields.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::pair;
    use std::thread;

    #[test]
    fn handshake_picks_highest_passing_probe() {
        let (mut send_opt, mut recv_opt) = pair();
        let cfg = LinkConfig { fast: true };
        let blob = vec![1u8, 2, 3, 4, 5];
        let sha256 = [1u8; 32];
        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, "dir".into(), 10, &blob, sha256, cfg).unwrap()
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            run_recv_handshake(&mut recv_opt, &out, false, cfg).unwrap()
        });
        let s = send_t.join().unwrap();
        let r = recv_t.join().unwrap();
        assert_eq!(s.qr_version, 40);
        assert_eq!(r.qr_version, 40);
        assert_eq!(s.basename, "dir");
        assert_eq!(r.chunk_count, 1);
    }

    #[test]
    fn handshake_all_probes_fail() {
        // Recv that never sees probes: only send side missing. Drive recv with an optical
        // that never delivers Probe and times out fast.
        let (mut send_end, mut recv_end) = pair();
        // Drop the send end without showing probes; recv will HELLO then timeout.
        drop(send_end);
        let cfg = LinkConfig { fast: true };
        let out = std::env::temp_dir();
        let err = run_recv_handshake(&mut recv_end, &out, false, cfg).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::HandshakeTimeout | crate::Error::NoUsableProbe
        ));
    }
}
```

The second test: if send is dropped, recv's HELLO never gets a probe → `HandshakeTimeout` (30s). In `fast` mode, use 100ms instead of 30s for HELLO timeout so the test is quick.

Document: `LinkConfig` timeouts when `fast`: HELLO 100ms, LINK 50ms, GO 50ms, quiet 20ms, no dwell sleeps.

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- link::
```

Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

Implement `src/link.rs` exactly as the algorithms above, with `LinkConfig { fast }` scaling timeouts. `PROBES` constant:

```rust
pub const PROBES: [(u8, u8, u16); 6] = [
    (0, 10, 250),
    (1, 15, 250),
    (2, 20, 200),
    (3, 25, 200),
    (4, 30, 150),
    (5, 40, 150),
];
```

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- link::
```

Expected: PASS.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/link.rs src/lib.rs
git commit -m "Implement optical link handshake with probe selection."
```

---

### Task 7: Block-ACK transport

**Files:**
- Create: `src/transport.rs`
- Modify: `src/lib.rs`
- Test: `src/transport.rs`

**Interfaces:**
- Consumes: `Optical`, `Session`, `frame::Payload`, `frame::data_chunk_size`
- Produces:
  - `transport::WINDOW: u32 = 32`
  - `transport::send_blob(opt: &mut impl Optical, session: &Session, blob: &[u8], cfg: TransportConfig) -> Result<()>`
  - `transport::recv_blob(opt: &mut impl Optical, session: &Session, cfg: TransportConfig) -> Result<Vec<u8>>`
  - `TransportConfig { fast: bool }` (ACK wait 2s vs 50ms; stall rounds still 20)

Chunk the blob with `data_chunk_size(session.qr_version)`. Last chunk may be short. `chunk_count` must match `session.chunk_count` (compute on send; recv allocates `compressed_size` and writes by seq).

Send window `base`:

1. Show each `Data { seq, chunk }` for seq in range (in fast mode, no dwell; live mode sleeps `session.dwell_ms` in Task 8 wrapping, **or** `send_blob` sleeps dwell unless `cfg.fast`).
2. Poll ACK up to ACK timeout. If `Ack { window_base }` matches `base`, compute holes: for seq in `base..end`, bit `(seq-base)` unset → hole. Retransmit holes. If ACK adds no new bits vs last, increment stall counter; else reset. At 20, `Error::Stalled(missing)`.
3. Advance base by 32 until done.
4. `show(Fin { sha256: session.sha256 })`.
5. Poll 15s (fast: 50ms) for `Ok` or `Fail`. `Fail` reason hash → `Error::HashMismatch`. Missing → `HandshakeFailed`.

Recv:

1. `got: HashSet<u32>` and `parts: Vec<Option<Vec<u8>>>` length `chunk_count`.
2. Loop until all chunks: poll DATA (timeout 2s / fast 50ms). Ignore duplicate / out of window (`seq < base` or `seq >= base+32` except during same window fill). Continuously `show(Ack { window_base: base, bitmap })` after each accepted DATA. When window complete, advance `base`.
3. On `Fin`, concat parts. If sha256 of bytes != session.sha256: `show(Fail { FAIL_HASH })`, `Error::HashMismatch`. Else `show(Ok)`, return bytes.

Do **not** unpack inside `recv_blob`. CLI unpacks.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::data_chunk_size;
    use crate::link::Session;
    use crate::optical::{pair, Lossy};
    use sha2::{Digest, Sha256};
    use std::thread;

    fn session_for(blob: &[u8], version: u8) -> Session {
        let cs = data_chunk_size(version);
        let chunk_count = ((blob.len() + cs - 1) / cs) as u32;
        let mut hasher = Sha256::new();
        hasher.update(blob);
        let sha256: [u8; 32] = hasher.finalize().into();
        Session {
            qr_version: version,
            dwell_ms: 1,
            basename: "dir".into(),
            uncompressed_hint: blob.len() as u64,
            compressed_size: blob.len() as u64,
            chunk_count,
            sha256,
        }
    }

    #[test]
    fn transfer_zero_loss() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_drops_seq_zero_then_recovers() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, r) = pair();
        let mut r = Lossy {
            inner: r,
            drop_data_seq: [0].into_iter().collect(),
        };
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn hash_mismatch_does_not_return_blob() {
        let blob = vec![1, 2, 3, 4];
        let mut sess = session_for(&blob, 10);
        sess.sha256 = [0u8; 32];
        let (mut s, mut r) = pair();
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg)
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg));
        let recv_res = rt.join().unwrap();
        assert!(matches!(recv_res, Err(crate::Error::HashMismatch)));
        let _ = st.join().unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- transport::
```

Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

Implement `send_blob` / `recv_blob` as specified. Derive `Clone` on `Session`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test --lib -- transport::
```

Expected: PASS. Also add a burst-loss test: `drop_data_seq` = `{2,3,4,5,6,7,8,9}` once each; blob still round-trips.

```rust
#[test]
fn transfer_burst_of_eight_drops() {
    let blob: Vec<u8> = (0..4000u32).map(|i| i as u8).collect();
    let sess = session_for(&blob, 10);
    let (mut s, r) = pair();
    let mut r = Lossy {
        inner: r,
        drop_data_seq: (2..10).collect(),
    };
    let cfg = TransportConfig { fast: true };
    let st = thread::spawn({
        let blob = blob.clone();
        let sess = sess.clone();
        move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
    });
    let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
    st.join().unwrap();
    assert_eq!(rt.join().unwrap(), blob);
}
```

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/transport.rs src/optical.rs src/link.rs src/lib.rs
git commit -m "Transfer blobs with block ACK and retransmission."
```

---

### Task 8: Live optical + CLI orchestration

**Files:**
- Create: `src/live.rs`
- Modify: `src/cli.rs`, `src/main.rs`, `src/lib.rs`
- Test: `src/cli.rs`, `src/pack.rs` (dest check already covered)

**Interfaces:**
- Consumes: all modules
- Produces:
  - `live::LiveOptical::open(camera: u32, no_invert: bool) -> Result<Self>`
  - `impl Optical for LiveOptical` (`show` encodes QR at current `version` stored on self; `poll` grabs a frame, `qr::decode_image`)
  - `live::LiveOptical::set_version(&mut self, v: u8)`
  - `cli::run()` full send/recv
  - Ctrl-C: restore terminal, `remove_temp` unless `keep_temp`, exit 130

Send `run` path:

1. `pack(path)` → read the temp `.tar.zst` into memory (or mmap) for handshake + send.
2. `LiveOptical::open`. `run_send_handshake` (Task 6) computes `chunk_count` after LINK from blob length + locked version, then sends `GO`.
3. `LiveOptical::show` uses `encode_version_for`: `Probe` uses that probe’s QR version; every other kind uses `self.version` (default 10 until `set_version` after LINK).
4. `send_blob` with `TransportConfig { fast: false }` (sleep `session.dwell_ms` between DATA frames).
5. On success `remove_temp` unless `keep_temp`. Print pack warnings to stderr **after** leaving alt screen.

Recv:

1. Open live optical.
2. `run_recv_handshake`
3. `recv_blob` → write bytes to temp path (same naming as pack)
4. Hash already verified in recv_blob; `unpack(temp, outdir, force)`
5. `remove_temp` unless `keep_temp`

Alt screen: `LiveOptical::open` enters alternate screen, hides cursor. `Drop` leaves alt screen, shows cursor. `show` clears, prints `render_terminal`, one status line (`set_status(&str)`).

Camera errors: map to `Error::Camera` with macOS text `"allow Camera for this terminal in System Settings > Privacy & Security"` and Windows text `"enable camera access in Settings > Privacy > Camera"`. Use `cfg(target_os)`.

Ctrl-C: `ctrlc::set_handler` sets `AtomicBool`. `poll`/`show` check it and return `Error::Message("interrupted")`. `cli::run` maps that to restore + exit 130. Store temp path in `Mutex<Option<PathBuf>>` for the handler to delete.

- [ ] **Step 1: Write the failing test**

CLI dest-exists is pack-level. Add handshake dest test if missing. For CLI:

```rust
#[test]
fn send_requires_path() {
    let err = Cli::try_parse_from(["airgap-xfer", "send"]).unwrap_err();
    assert!(err.to_string().contains("required") || err.to_string().contains("PATH"));
}
```

Add `src/live.rs` unit test for version switching on probe vs data:

```rust
pub fn encode_version_for(payload: &frame::Payload, locked: u8) -> u8 {
    match payload {
        frame::Payload::Probe { qr_version, .. } => *qr_version,
        _ => locked,
    }
}

#[test]
fn probe_uses_probe_version() {
    assert_eq!(
        encode_version_for(
            &frame::Payload::Probe {
                id: 2,
                qr_version: 20,
                dwell_ms: 200,
                last: false
            },
            10
        ),
        20
    );
    assert_eq!(encode_version_for(&frame::Payload::Ok, 25), 25);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib -- live:: encode_version_for
```

Expected: FAIL until helper exists.

- [ ] **Step 3: Write minimal implementation**

Implement `src/live.rs` with nokhwa `Camera::new(CameraIndex::Index(camera), ...)` luma frames → `GrayImage` → `qr::decode_image`. Ignore decode misses (`poll` returns `Ok(None)`).

Wire `cli::run`:

```rust
pub fn run() -> crate::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Send { path, camera, keep_temp, no_invert } => send(path, camera, keep_temp, no_invert),
        Cmd::Recv { outdir, camera, force, keep_temp, no_invert } => {
            recv(outdir.unwrap_or_else(|| PathBuf::from(".")), camera, force, keep_temp, no_invert)
        }
    }
}
```

Do not change the Task 6 handshake signature. Wire `send`/`recv` functions in `cli.rs` using `LiveOptical`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo test -q
```

Expected: all lib tests PASS. `cargo run -- send --help` and `recv --help` still work.

Manual (not CI): Mac ↔ Windows live checklist from the spec.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add src/live.rs src/cli.rs src/link.rs src/lib.rs src/main.rs
git commit -m "Wire send/recv through webcam QR and terminal display."
```

---

### Task 9: Install scripts

**Files:**
- Create: `install.sh`
- Create: `install.ps1`
- Test: run `install.sh` with `PREFIX`

**Interfaces:**
- Consumes: `target/release/airgap-xfer` or `$1` binary path
- Produces: installed executable on PATH

- [ ] **Step 1: Write the failing test**

There is no script yet. Create a temp prefix test after writing the script. First run:

```bash
test -f install.sh
```

Expected: FAIL (missing).

- [ ] **Step 2: Confirm it fails**

Expected: `test` exit 1.

- [ ] **Step 3: Write minimal implementation**

`install.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-/usr/local}"
SRC="${1:-$ROOT/target/release/airgap-xfer}"
if [[ ! -f "$SRC" ]]; then
  echo "missing binary: $SRC (build with: cargo build --release)" >&2
  exit 1
fi
mkdir -p "$PREFIX/bin"
install -m 755 "$SRC" "$PREFIX/bin/airgap-xfer"
echo "installed $PREFIX/bin/airgap-xfer"
```

`install.ps1`:

```powershell
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Src = if ($args[0]) { $args[0] } else { Join-Path $Root "target\release\airgap-xfer.exe" }
if (-not (Test-Path $Src)) { Write-Error "missing binary: $Src (cargo build --release)" }
$DestDir = Join-Path $env:LOCALAPPDATA "airgap-xfer"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
Copy-Item -Force $Src (Join-Path $DestDir "airgap-xfer.exe")
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($UserPath.Split(';') -contains $DestDir)) {
  [Environment]::SetEnvironmentVariable("Path", "$DestDir;$UserPath", "User")
}
Write-Host "installed $DestDir\airgap-xfer.exe"
```

`chmod +x install.sh`.

- [ ] **Step 4: Run tests and make sure they pass**

```bash
cargo build -q
PREFIX=/tmp/airgap-xfer-prefix ./install.sh ./target/debug/airgap-xfer
/tmp/airgap-xfer-prefix/bin/airgap-xfer send --help
```

Expected: help text, exit 0.

- [ ] **Step 5: Commit (skip if user asked not to commit)**

```bash
git add install.sh install.ps1
git commit -m "Add shell and PowerShell installers for the binary."
```

---

## Manual live checklist (after Task 8)

On Mac and Windows, facing webcams:

1. Camera permission; sender sees HELLO; handshake reaches GO (no DATA during probes).
2. ~1 MB folder round-trip.
3. ~20 MB folder; cover camera briefly; holes fill via ACK.
4. Ctrl-C mid-send: no leftover tree; terminal usable.

---

## Self-review notes

- Spec coverage: envelope, pack/unpack, handshake table/timeouts, block-ACK 32, hash FAIL no unpack, dest `--force`, install scripts, Ctrl-C, no OpenCV, rxing instead of rqrr (binary payload).
- `chunk_count` is computed after LINK (required; unknown QR version before handshake).
- `Lossy` drops each seq once so retransmission can succeed.
- `LinkConfig.fast` / `TransportConfig.fast` keep CI off real 30s timeouts.
