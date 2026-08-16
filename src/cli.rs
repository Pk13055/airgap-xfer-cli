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
