# airgap-xfer CLI design

Date: 2026-08-16

A Mac ↔ Windows CLI that moves a file or directory across an air gap by flashing QR codes in the terminal and reading them with the other machine’s webcam. Transfer uses no network sockets.

## Goal

- Send 2–50 MB payloads (after zstd) between a Mac and a Windows machine the operator controls.
- Pack any path into a temporary `.tar.zst`, optically transfer the blob, then restore the original directory tree (or single file).
- Minimize runtime dependencies: one static Rust binary per OS, plus `install.sh` / `install.ps1`.
- Measure the optical link once, then send data frames with block ACKs. Do not intersperse probe or extra ECC QRs during the payload.

## Non-goals (v1)

- Encryption
- Linux
- Resume after process kill
- GUI
- OpenCV or other heavy media stacks
- Network transport of any kind (the data path is camera + terminal only)

## Physical setup

Two laptops face each other. Each webcam looks at the other machine’s terminal window. Both machines run the same binary; one is `send`, the other is `recv`.

Require a Unicode / VT-capable terminal (Windows Terminal on Windows; Terminal.app, iTerm2, or similar on macOS). Classic `conhost.exe` is unsupported.

## Architecture

One binary: `airgap-xfer`.

```text
Sender                                      Receiver
------                                      --------
airgap-xfer send <path>                     airgap-xfer recv [outdir]
  Packer  → temp .tar.zst                     Camera → frames of sender screen
  Framer  → sequenced chunks                  QR decode
  QR encode → terminal                        Transport → fill gaps via ACK QR
  Camera  → frames of receiver ACK            QR encode ACK → terminal
  Transport → send / retransmit               Unpacker → directory tree
```

| Module | Responsibility | Depends on |
|---|---|---|
| `cli` | `send` / `recv`, flags, progress, exit codes | all below |
| `pack` | path → temp `.tar.zst`; reverse on recv | `zstd`, `tar` |
| `qr` | bytes ↔ QR matrix; matrix ↔ terminal cells | `qrcode`, `rqrr` |
| `camera` | webcam frames as grayscale | `nokhwa` |
| `link` | calibration handshake only | `qr`, `camera` |
| `transport` | block-ACK window over locked QR params | `qr`, `camera` |

Crate choices (keep this set small): `clap`, `crossterm`, `nokhwa`, `qrcode`, `rqrr`, `zstd`, `tar`, `sha2`, `crc32fast`, `thiserror`. Switch decode to `rxing` only if `rqrr` cannot reliably read terminal-rendered QRs.

## CLI

```text
airgap-xfer send <path> [--camera N] [--keep-temp] [--no-invert]
airgap-xfer recv [outdir] [--camera N] [--force] [--keep-temp] [--no-invert]
```

Defaults:

- `recv` outdir: current directory
- `--camera`: `0`
- QR is drawn with a light quiet zone and dark modules so decoders see a standard QR. `--no-invert` is reserved if a specific terminal needs the opposite polarity.

`send` and `recv` use the alternate screen, hide the cursor, and restore the terminal on any exit including Ctrl-C.

Progress is one status line below the QR (window seq / total, holes remaining, estimated throughput). Do not log to the QR region.

## Packing

Every `send` path is archived the same way:

1. Create `std::env::temp_dir()/airgap-xfer-{pid}-{rand}.tar.zst`.
2. Write a tar stream whose root entry is the basename of `<path>` (file or directory).
3. Compress that stream with zstd (default level 3).
4. SHA-256 the compressed file. That digest is the transfer checksum.

Unpack on success:

1. Decompress and untar into `outdir/.airgap-xfer-partial/`.
2. Rename-swap the extracted basename into `outdir`.
3. Delete the partial directory and the temp `.tar.zst`.

Rules:

- Hidden files are included.
- Symlinks are skipped (warning listed at end). Safer on Windows and avoids escaping `outdir`.
- Unreadable files are skipped (warning listed at end). If the archive would be empty, `send` errors.
- If `outdir/<basename>` already exists, refuse unless `--force` (replace that basename only).
- If `outdir` does not exist, create it.
- On Windows, tar mode bits are ignored; files get default ACLs.

## Frame envelope

Every QR payload is this binary envelope:

```text
magic    "AX"       2 bytes
ver      u8         1
kind     u8         see kinds below
len      u16 BE     payload length
payload             kind-specific
crc32    u32 BE     CRC-32 of everything before crc
```

Kinds:

| Kind | Value | Payload |
|---|---|---|
| `HELLO` | 1 | protocol_ver u8, role u8 (`recv`=2; only recv emits HELLO) |
| `PROBE` | 2 | probe id u8, qr version u8, dwell ms u16, last u8 (1 if final probe) |
| `LINK` | 3 | chosen qr version u8, dwell ms u16, probe loss 0–100 u8 |
| `GO` | 4 | basename_len u16 BE, basename UTF-8, uncompressed_hint u64 BE, compressed_size u64 BE, chunk_count u32 BE, sha256 32 bytes |
| `DATA` | 5 | seq u32 BE, then raw chunk bytes |
| `ACK` | 6 | window_base seq u32 BE, bitmap u32 BE (bit i = received `window_base+i`) |
| `FIN` | 7 | sha256 32 bytes |
| `OK` | 8 | empty |
| `FAIL` | 9 | reason u8 |

`FAIL` reasons: `1` hash mismatch, `2` disk full, `3` decode/protocol, `4` aborted.

Unknown kind, bad magic, truncated frame, or bad CRC: drop the frame (treat as loss).

Chunk size = QR binary capacity for the locked version and Quartile ECC, minus envelope overhead. Chunk size is fixed for the session after handshake. It does not change mid-transfer.

QR ECC level: Quartile. That is the only extra error correction on the data path. No recovery QRs mixed into DATA.

## Handshake (link setup)

Runs once per session. No DATA frames until both sides lock parameters.

1. `recv` displays `HELLO` until it sees `PROBE` or 30s timeout.
2. `send` waits for `HELLO` (30s timeout), then displays a fixed probe list, each held for its dwell:

   | probe id | QR version | dwell |
   |---|---|---|
   | 0 | 10 | 250 ms |
   | 1 | 15 | 250 ms |
   | 2 | 20 | 200 ms |
   | 3 | 25 | 200 ms |
   | 4 | 30 | 150 ms |
   | 5 | 40 | 150 ms |

   Last probe has `last=1`. After the last probe, sender keeps displaying it and polls the camera for `LINK` for up to 5s.
3. `recv` records which probes decoded. After a probe with `last=1` (or 2s with no further probes), it displays `LINK` with the highest version that decoded at least once and that probe’s dwell. `probe loss` = `100 * (6 - successful_count) / 6`. If none decoded, `recv` exits with lighting / distance / font guidance.
4. `send` reads `LINK`, locks version + dwell, displays `GO`.
5. `recv` reads `GO` (15s timeout). If `outdir/<basename>` exists and `--force` was not set, display `FAIL` (aborted) and exit. Otherwise display `ACK` with `window_base=0` and empty bitmap (ready).
6. `send` waits for that ready `ACK` (15s) before the first DATA window. Transfer starts.

If no probe succeeds, or webcam/permission fails, exit nonzero with the OS-specific permission hint (macOS Camera TCC, Windows camera privacy).

## Transfer (block-ACK)

Window size: 32 frames.

1. Sender emits `DATA` seq `base .. base+31` (or up to `chunk_count-1`), each held for the locked dwell.
2. Receiver continuously displays `ACK` for the current window as bits fill in.
3. After the window, sender reads ACK for up to 2s. Retransmit only holes. A window is complete when every seq in `base .. min(base+31, chunk_count-1)` has its bit set; bits past the last chunk are ignored. Repeat until complete or 20 consecutive ACK rounds show no new bits → abort, print missing seqs.
4. Advance `base` by 32. Repeat until all chunks are received.
5. Sender displays `FIN` + sha256. Receiver hashes the temp blob.
   - Match → unpack, display `OK`, exit 0.
   - Mismatch → display `FAIL` (hash), do not unpack, exit 1.
6. Sender waits for `OK` or `FAIL` (15s). Matching `FAIL` is printed and sender exits 1.

Duplicates and out-of-window `DATA` are ignored.

Ctrl-C: stop camera, delete temp `.tar.zst` unless `--keep-temp`, restore terminal, exit 130. Partial extract dir is deleted. Destination tree is left untouched unless rename-swap already completed.

## Install

No network is required to *run* a transfer. Install is copy-the-binary:

- `install.sh`: install `airgap-xfer` to `/usr/local/bin` (or `PREFIX`).
- `install.ps1`: install to `%LOCALAPPDATA%\airgap-xfer\` and prepend that directory to the user `PATH`.

First-pass validation on a locked-down machine is: does Gatekeeper / SmartScreen / AppLocker allow the binary to run. The transfer path itself cannot be blocked by a network firewall.

## Error handling (summary)

| Condition | Behavior |
|---|---|
| No HELLO in 30s (send) or no PROBE in 30s (recv) | Exit, tell operator to face webcams at the other terminal |
| No LINK in 5s after probes | Exit, handshake failed |
| No GO in 15s (recv) or no ready ACK in 15s (send) | Exit, handshake failed |
| No usable probe | Exit, suggest lighting, distance, larger font |
| Camera missing / permission | Exit with OS hint |
| Stalled window (20 idle ACK rounds) | Abort, list missing seqs, delete temp unless `--keep-temp` |
| Hash mismatch | `FAIL`, no unpack |
| Disk full / zstd error | Abort, clean temp, nonzero |
| `outdir/<basename>` exists | Refuse unless `--force` |
| Unreadable / symlink during pack | Skip + final warning list; empty archive is an error |

## Testing

### Automated (CI, no camera)

- Pack/unpack round-trip: single file, nested dirs, empty dir, binary with NULs. Names and bytes match. Temp `.tar.zst` is removed on success.
- Envelope: each kind encodes and decodes; truncated, bad magic, bad CRC, unknown kind are dropped.
- Transport on a mock `send_qr` / `recv_qr`: 0% loss, 10% random loss, burst of 8 drops, duplicate ACKs, hash mismatch → `FAIL` and no unpack.
- Handshake mock: highest passing probe is chosen; all probes fail → error.
- CLI: `send --help`, `recv --help`; refuse when `outdir/<basename>` exists without `--force`.

### Live (manual, Mac ↔ Windows)

- Camera permissions; sender sees `HELLO`, handshake reaches `GO`.
- Handshake selects a version; ~1 MB folder round-trips.
- ~20 MB folder; briefly cover the camera; holes fill via ACK without restarting.
- Ctrl-C mid-send: no leftover tree, terminal usable.

## Success criteria

- `airgap-xfer send ./dir` on Mac and `airgap-xfer recv ./in` on Windows (and the reverse) restores `./in/dir` with matching file bytes.
- Handshake completes without DATA on screen.
- A 20 MB compressed payload finishes without a full restart after a short camera occlusion.
- Runtime dependency set is the binary plus OS webcam permission. No OpenCV, no Python, no extra daemon.
