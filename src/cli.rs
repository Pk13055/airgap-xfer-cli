use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

use crate::{
    chunks, deliver, live,
    optical::Optical,
    pack,
    resume::{self, Offer},
    session, tui,
};

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
    let result = tui::run(
        &title,
        camera,
        no_invert,
        tui::Aim::EnterWhenLocked,
        move |mut opt, max_version| {
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
        },
    );

    live::set_temp_path(None);
    if !keep_temp {
        pack::remove_temp(&temp_path);
    }
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    result
}

/// Says how many reconnects the transfer needed, and nothing when the first
/// link carried it all the way.
fn attempt_note(attempt: usize) -> String {
    match attempt {
        0 => String::new(),
        1 => ", after 1 reconnect".into(),
        n => format!(", after {n} reconnects"),
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
    tui::run(
        &title,
        camera,
        no_invert,
        tui::Aim::Immediate,
        move |mut opt, max_version| {
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
        },
    )
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
        assert_eq!(
            names("recv"),
            ["camera", "force", "keep_temp", "no_invert", "outdir"]
        );
    }
}
