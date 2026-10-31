//! Turning a verified archive into files on disk, and only then telling the
//! sender.
//!
//! OK is the sender's only evidence the transfer worked, so it must never
//! run ahead of reality: the archive is returned by the transport, written,
//! flushed to stable storage, and unpacked — and only then is OK shown. Any
//! failure on the way shows FAIL instead.

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    frame::{Payload, FAIL_DISK, FAIL_HASH, FAIL_PROTOCOL},
    optical::Optical,
    resume::Offer,
    Error, Result,
};

/// Where a received archive goes.
pub trait ArchiveSink {
    /// Writes the archive and flushes it to stable storage.
    fn persist(&mut self, blob: &[u8]) -> Result<()>;
    /// Unpacks the persisted archive.
    fn unpack(&mut self) -> Result<()>;
}

/// Writes `<outdir>/<basename>.tar.zst`, fsyncs it, and unpacks it into
/// `outdir`. The archive is left in place either way: if unpacking fails the
/// operator can still run `tar --zstd -xf` on it.
pub struct DiskSink {
    pub archive_path: PathBuf,
    pub outdir: PathBuf,
    pub force: bool,
}

impl ArchiveSink for DiskSink {
    fn persist(&mut self, blob: &[u8]) -> Result<()> {
        let parent = self.archive_path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut file = File::create(&self.archive_path)?;
        file.write_all(blob)?;
        file.sync_all()?;
        sync_dir(parent)
    }

    fn unpack(&mut self) -> Result<()> {
        crate::pack::unpack(&self.archive_path, &self.outdir, self.force).map(|_| ())
    }
}

/// Makes the new directory entry itself durable. Only Unix can open a
/// directory for syncing; elsewhere the file sync is the best available.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

fn fail_reason(err: &Error) -> u8 {
    match err {
        Error::HashMismatch => FAIL_HASH,
        Error::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => FAIL_DISK,
        _ => FAIL_PROTOCOL,
    }
}

/// Re-checks `blob` against the offer, persists and unpacks it through
/// `sink`, and shows OK only once all of that succeeded. On failure shows
/// FAIL and returns the error.
pub fn deliver(
    opt: &mut impl Optical,
    offer: &Offer,
    blob: &[u8],
    sink: &mut impl ArchiveSink,
) -> Result<()> {
    let outcome = (|| {
        let digest: [u8; 32] = Sha256::digest(blob).into();
        if digest != offer.sha256 {
            return Err(Error::HashMismatch);
        }
        sink.persist(blob)?;
        sink.unpack()
    })();
    match outcome {
        Ok(()) => opt.show(&Payload::Ok {
            transfer_id: offer.transfer_id,
        }),
        Err(err) => {
            let _ = opt.show(&Payload::Fail {
                transfer_id: offer.transfer_id,
                reason: fail_reason(&err),
            });
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack;
    use sha2::{Digest, Sha256};
    use std::{cell::RefCell, fs, rc::Rc, time::Duration};

    type Events = Rc<RefCell<Vec<String>>>;

    struct Watch {
        events: Events,
    }

    impl Optical for Watch {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.events.borrow_mut().push(match payload {
                Payload::Ok { .. } => "OK".into(),
                Payload::Fail { reason, .. } => format!("FAIL {reason}"),
                other => format!("{other:?}"),
            });
            Ok(())
        }

        fn poll(&mut self, _: Duration) -> Result<Option<Payload>> {
            Ok(None)
        }
    }

    struct Scripted {
        events: Events,
        fail_persist: bool,
        fail_unpack: bool,
    }

    impl ArchiveSink for Scripted {
        fn persist(&mut self, _blob: &[u8]) -> Result<()> {
            self.events.borrow_mut().push("persist+sync".into());
            if self.fail_persist {
                return Err(Error::Io(std::io::ErrorKind::StorageFull.into()));
            }
            Ok(())
        }

        fn unpack(&mut self) -> Result<()> {
            self.events.borrow_mut().push("unpack".into());
            if self.fail_unpack {
                return Err(Error::Message("bad archive".into()));
            }
            Ok(())
        }
    }

    fn run(
        blob: &[u8],
        claimed: &[u8],
        fail_persist: bool,
        fail_unpack: bool,
    ) -> (Result<()>, Vec<String>) {
        let events: Events = Rc::default();
        let offer = Offer::new(
            "dir",
            1,
            claimed.len() as u64,
            Sha256::digest(claimed).into(),
            1,
        )
        .unwrap();
        let result = deliver(
            &mut Watch {
                events: Rc::clone(&events),
            },
            &offer,
            blob,
            &mut Scripted {
                events: Rc::clone(&events),
                fail_persist,
                fail_unpack,
            },
        );
        let log = events.borrow().clone();
        (result, log)
    }

    #[test]
    fn ok_is_shown_only_after_the_archive_is_synced_and_unpacked() {
        let (result, events) = run(b"blob", b"blob", false, false);
        result.unwrap();
        assert_eq!(events, ["persist+sync", "unpack", "OK"]);
    }

    #[test]
    fn a_failed_unpack_reports_fail_and_never_ok() {
        let (result, events) = run(b"blob", b"blob", false, true);
        assert!(result.is_err());
        assert_eq!(events, ["persist+sync", "unpack", "FAIL 3"]);
    }

    #[test]
    fn a_full_disk_reports_fail_disk_and_skips_unpacking() {
        let (result, events) = run(b"blob", b"blob", true, false);
        assert!(result.is_err());
        assert_eq!(events, ["persist+sync", "FAIL 2"]);
    }

    #[test]
    fn a_blob_that_is_not_the_offered_archive_is_never_written() {
        let (result, events) = run(b"blob", b"other", false, false);
        assert!(matches!(result, Err(Error::HashMismatch)));
        assert_eq!(events, ["FAIL 1"]);
    }

    #[test]
    fn the_disk_sink_leaves_the_archive_and_the_unpacked_tree() {
        let root = std::env::temp_dir().join(format!("ag-deliver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src/project");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"hello").unwrap();
        let packed = pack::pack(&src).unwrap();
        let blob = fs::read(&packed.temp_path).unwrap();
        pack::remove_temp(&packed.temp_path);

        let out = root.join("out");
        let mut sink = DiskSink {
            archive_path: out.join(pack::archive_filename("project")),
            outdir: out.clone(),
            force: false,
        };
        let offer = Offer::new(
            "project",
            packed.uncompressed_hint,
            blob.len() as u64,
            packed.sha256,
            1,
        )
        .unwrap();
        let events: Events = Rc::default();
        deliver(
            &mut Watch {
                events: Rc::clone(&events),
            },
            &offer,
            &blob,
            &mut sink,
        )
        .unwrap();
        assert_eq!(fs::read(out.join("project/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(&sink.archive_path).unwrap(), blob);
        assert_eq!(*events.borrow(), ["OK"]);
        let _ = fs::remove_dir_all(&root);
    }
}
