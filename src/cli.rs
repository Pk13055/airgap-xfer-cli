use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

use crate::{
    link::{self, LinkConfig},
    live, pack, transport,
    transport::TransportConfig,
};

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
    matches!(result, Err(crate::Error::Message(m)) if m == "interrupted")
}

fn send(path: PathBuf, camera: u32, keep_temp: bool, no_invert: bool) -> crate::Result<()> {
    let packed = pack::pack(&path)?;
    if !keep_temp {
        live::set_temp_path(Some(packed.temp_path.clone()));
    }

    let result = (|| -> crate::Result<()> {
        let blob = fs::read(&packed.temp_path)?;
        let mut opt = live::LiveOptical::open(camera, no_invert)?;
        let cfg = LinkConfig { fast: false };
        let session = link::run_send_handshake(
            &mut opt,
            packed.basename.clone(),
            packed.uncompressed_hint,
            &blob,
            packed.sha256,
            cfg,
        )?;
        opt.set_version(session.qr_version);
        transport::send_blob(&mut opt, &session, &blob, TransportConfig { fast: false })?;
        Ok(())
    })();

    live::set_temp_path(None);
    if !keep_temp {
        pack::remove_temp(&packed.temp_path);
    }
    for warning in &packed.warnings {
        eprintln!("warning: {warning}");
    }
    result
}

fn recv(
    outdir: PathBuf,
    camera: u32,
    force: bool,
    keep_temp: bool,
    no_invert: bool,
) -> crate::Result<()> {
    let blob = {
        let mut opt = live::LiveOptical::open(camera, no_invert)?;
        let cfg = LinkConfig { fast: false };
        let session = link::run_recv_handshake(&mut opt, &outdir, force, cfg)?;
        opt.set_version(session.qr_version);
        transport::recv_blob(&mut opt, &session, TransportConfig { fast: false })?
    };

    let temp_path = std::env::temp_dir().join(format!(
        "airgap-xfer-{}-{}.tar.zst",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| crate::Error::Message(format!("system clock before UNIX epoch: {err}")))?
            .as_nanos()
    ));
    fs::write(&temp_path, &blob)?;
    if !keep_temp {
        live::set_temp_path(Some(temp_path.clone()));
    }

    let result = pack::unpack(&temp_path, &outdir, force).map(|_| ());

    live::set_temp_path(None);
    if !keep_temp {
        pack::remove_temp(&temp_path);
    }
    result
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
