use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Debug)]
pub struct Packed {
    pub temp_path: PathBuf,
    pub basename: String,
    pub uncompressed_hint: u64,
    pub compressed_size: u64,
    pub sha256: [u8; 32],
    pub warnings: Vec<String>,
}

pub fn pack(path: &Path) -> Result<Packed> {
    let basename = path
        .file_name()
        .ok_or_else(|| Error::Message(format!("path has no basename: {}", path.display())))?
        .to_string_lossy()
        .into_owned();
    let temp_path = std::env::temp_dir().join(format!(
        "airgap-xfer-{}-{}.tar.zst",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|err| Error::Message(format!("system clock before UNIX epoch: {err}")))?
            .as_nanos()
    ));

    let file = File::create(&temp_path)?;
    let encoder = zstd::Encoder::new(file, 3)?;
    let mut tar = tar::Builder::new(encoder);
    let mut warnings = Vec::new();
    let mut entry_count = 0;
    let mut uncompressed_hint = 0;

    append_path(
        &mut tar,
        path,
        Path::new(&basename),
        &mut warnings,
        &mut entry_count,
        &mut uncompressed_hint,
    );

    tar.finish()?;
    let encoder = tar.into_inner()?;
    encoder.finish()?;

    // A truly empty directory is a valid archive (its root directory entry
    // round-trips). A directory whose only contents were skipped is not.
    if entry_count == 0 || (entry_count == 1 && !warnings.is_empty()) {
        remove_temp(&temp_path);
        return Err(Error::EmptyArchive);
    }

    let compressed_size = fs::metadata(&temp_path)?.len();
    let sha256 = sha256_file(&temp_path)?;

    Ok(Packed {
        temp_path,
        basename,
        uncompressed_hint,
        compressed_size,
        sha256,
        warnings,
    })
}

fn append_path(
    tar: &mut tar::Builder<zstd::Encoder<'_, File>>,
    source: &Path,
    archive_path: &Path,
    warnings: &mut Vec<String>,
    entry_count: &mut u64,
    uncompressed_hint: &mut u64,
) {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(err) => {
            warnings.push(format!("skipped unreadable {}: {err}", source.display()));
            return;
        }
    };

    if metadata.file_type().is_symlink() {
        warnings.push(format!("skipped symlink {}", source.display()));
        return;
    }

    if metadata.is_file() {
        match tar.append_path_with_name(source, archive_path) {
            Ok(()) => {
                *entry_count += 1;
                *uncompressed_hint += metadata.len();
            }
            Err(err) => warnings.push(format!("skipped unreadable {}: {err}", source.display())),
        }
        return;
    }

    if !metadata.is_dir() {
        warnings.push(format!("skipped unsupported file type {}", source.display()));
        return;
    }

    match tar.append_dir(archive_path, source) {
        Ok(()) => *entry_count += 1,
        Err(err) => {
            warnings.push(format!("skipped unreadable {}: {err}", source.display()));
            return;
        }
    }

    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(err) => {
            warnings.push(format!("skipped unreadable {}: {err}", source.display()));
            return;
        }
    };

    for entry in entries {
        match entry {
            Ok(entry) => {
                append_path(
                    tar,
                    &entry.path(),
                    &archive_path.join(entry.file_name()),
                    warnings,
                    entry_count,
                    uncompressed_hint,
                );
            }
            Err(err) => warnings.push(format!(
                "skipped unreadable entry in {}: {err}",
                source.display()
            )),
        }
    }
}

pub fn unpack(zst: &Path, outdir: &Path, force: bool) -> Result<PathBuf> {
    let partial = outdir.join(".airgap-xfer-partial");
    remove_path_if_exists(&partial)?;
    fs::create_dir_all(&partial)?;

    let result = (|| {
        let file = File::open(zst)?;
        let decoder = zstd::Decoder::new(file)?;
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&partial)?;

        let basename = archive_root_basename(&partial)?;
        let source = partial.join(&basename);
        let destination = outdir.join(&basename);

        if destination.exists() {
            if !force {
                return Err(Error::DestExists(destination));
            }
            remove_path_if_exists(&destination)?;
        }

        fs::create_dir_all(outdir)?;
        fs::rename(&source, &destination)?;
        fs::remove_dir_all(&partial)?;
        Ok(destination)
    })();

    if result.is_err() {
        let _ = remove_path_if_exists(&partial);
    }
    result
}

fn archive_root_basename(partial: &Path) -> Result<String> {
    let mut roots = Vec::new();
    for entry in fs::read_dir(partial)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != "." && name != ".." {
            roots.push(name);
        }
    }

    if roots.len() != 1 {
        return Err(Error::Message(format!(
            "bad archive: expected exactly one top-level entry, found {}",
            roots.len()
        )));
    }

    Ok(roots.pop().unwrap().to_string_lossy().into_owned())
}

pub fn remove_temp(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!("warning: failed to remove temporary archive {}: {err}", path.display());
        }
    }
}

pub fn dest_exists(outdir: &Path, basename: &str) -> bool {
    outdir.join(basename).exists()
}

fn remove_path_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hasher.finalize().into())
}

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
        assert!(packed.temp_path.exists());
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

    #[test]
    fn rejects_archive_with_multiple_top_level_entries_without_torn_destination() {
        let root = std::env::temp_dir().join(format!("ag-pack-multiple-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let archive_path = root.join("multiple-roots.tar.zst");
        let file = File::create(&archive_path).unwrap();
        let encoder = zstd::Encoder::new(file, 3).unwrap();
        let mut archive = tar::Builder::new(encoder);
        for (path, contents) in [("alpha", b"new" as &[u8]), ("beta", b"other")] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, path, contents).unwrap();
        }
        archive.finish().unwrap();
        archive.into_inner().unwrap().finish().unwrap();

        let out = root.join("out");
        write(&out.join("alpha"), b"old");
        let err = unpack(&archive_path, &out, true).unwrap_err();

        assert!(matches!(err, Error::Message(message) if message.contains("bad archive")));
        assert_eq!(fs::read(out.join("alpha")).unwrap(), b"old");
        assert!(!out.join("beta").exists());
        assert!(!out.join(".airgap-xfer-partial").exists());

        let _ = fs::remove_dir_all(&root);
    }
}
