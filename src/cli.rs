use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

use crate::{
    frame,
    link,
    live,
    optical::Optical,
    pack,
    session,
    tui,
};

/// A version 10 QR code is 65 columns by 33 rows once drawn two modules per
/// terminal cell, and it cannot be shrunk further without becoming
/// undecodable. The TUI sheds its borders, footer, and transcript to fit, so
/// the hard floor is the code itself plus a title and a prompt line.
const SIZE_NOTE: &str = "Both terminals need at least 65x35 cells: the smallest usable QR code is \
65x33 on its own, and a clipped code cannot be read by the other camera. The \
layout drops its borders and transcript on short terminals to make room. \
Aim both cameras until tracking locks, then press Enter. After that the \
handshake runs on its own. One Enter on the sender starts the file; the \
receiver takes it without further keypresses. Esc aborts, q quits.";

#[derive(Parser, Debug)]
#[command(name = "airgap-xfer", version, after_help = SIZE_NOTE)]
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
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Send {
            path,
            camera,
            keep_temp,
            no_invert,
        } => send(path, camera, keep_temp, no_invert),
        Cmd::Recv {
            outdir,
            camera,
            force,
            keep_temp,
            no_invert,
        } => recv(
            outdir.unwrap_or_else(|| PathBuf::from(".")),
            camera,
            force,
            keep_temp,
            no_invert,
        ),
    };

    if is_interrupted(&result) {
        std::process::exit(130);
    }
    result
}

fn is_interrupted(result: &crate::Result<()>) -> bool {
    matches!(result, Err(crate::Error::Interrupted))
}

/// Announces the settings an attempt will use, so the operator can see the
/// ladder being walked down rather than just watching it hang.
fn send(path: PathBuf, camera: u32, keep_temp: bool, no_invert: bool) -> crate::Result<()> {
    let mut packed = pack::pack(&path)?;
    if !keep_temp {
        live::set_temp_path(Some(packed.temp_path.clone()));
    }

    let temp_path = packed.temp_path.clone();
    let warnings = std::mem::take(&mut packed.warnings);
    let title = format!("airgap-xfer · SEND {}", packed.basename);
    let result = tui::run(&title, camera, no_invert, move |mut opt, max_version| {
        let blob = fs::read(&packed.temp_path)?;
        let done = session::send_with_fallback(
            &mut opt,
            &packed.basename,
            packed.uncompressed_hint,
            &blob,
            packed.sha256,
            max_version,
        )?;
        Ok(format!(
            "sent {} ({} B in {} chunks at QR v{}{})",
            done.session.basename,
            done.session.compressed_size,
            done.session.chunk_count,
            done.session.qr_version,
            attempt_note(done.attempt)
        ))
    });

    live::set_temp_path(None);
    if !keep_temp {
        pack::remove_temp(&temp_path);
    }
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    result
}

/// Mentions which rung of the fallback ladder finally worked, and says nothing
/// when the first attempt did.
fn attempt_note(attempt: usize) -> String {
    if attempt == 0 {
        String::new()
    } else {
        format!(", attempt {}/{}", attempt + 1, link::MAX_ATTEMPTS)
    }
}

fn recv(
    outdir: PathBuf,
    camera: u32,
    force: bool,
    keep_temp: bool,
    no_invert: bool,
) -> crate::Result<()> {
    let title = format!("airgap-xfer · RECV into {}", outdir.display());
    tui::run(&title, camera, no_invert, move |mut opt, max_version| {
        let (done, blob) =
            session::recv_with_fallback(&mut opt, &outdir, force, max_version)?;

        // recv_blob only verifies the hash; it deliberately does not send OK.
        // The sender must not see OK until the blob is durably written (and
        // unpacked) here, so write+unpack happens before we show OK/FAIL.
        let archive_name = pack::archive_filename(&done.session.basename);
        let archive_path = outdir.join(&archive_name);
        if !keep_temp {
            live::set_temp_path(Some(archive_path.clone()));
        }

        opt.log(&format!("writing {archive_name}"));
        let write_and_unpack = (|| -> crate::Result<()> {
            if let Some(parent) = archive_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&archive_path, &blob)?;
            // The `.tar.zst` is the received file. Unpack is a convenience;
            // if it fails the archive stays so the operator can run
            // `tar --zstd -xf {archive_name}` themselves.
            pack::unpack(&archive_path, &outdir, force)?;
            Ok(())
        })();

        match &write_and_unpack {
            Ok(()) => {
                opt.show(&frame::Payload::Ok)?;
            }
            Err(crate::Error::Io(io_err)) if io_err.kind() == std::io::ErrorKind::StorageFull => {
                opt.show(&frame::Payload::Fail {
                    reason: frame::FAIL_DISK,
                })?;
            }
            Err(_) => {
                opt.show(&frame::Payload::Fail {
                    reason: frame::FAIL_PROTOCOL,
                })?;
            }
        }

        live::set_temp_path(None);
        write_and_unpack?;
        Ok(format!(
            "received {archive_name} into {} (unpacked {}){}",
            outdir.display(),
            done.session.basename,
            attempt_note(done.attempt)
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn send_requires_path() {
        let err = Cli::try_parse_from(["airgap-xfer", "send"]).unwrap_err();
        assert!(err.to_string().contains("required") || err.to_string().contains("PATH"));
    }

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
