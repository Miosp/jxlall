//! Downloads in the per-user cache: fetched once, verified by SHA-256 on every
//! load.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::artifacts::{Archive, ArchiveFile, ArchiveFormat, Remote};

const READ_CHUNK: usize = 1 << 20;
const BYTES_PER_MB: u64 = 1_000_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A stalled transfer fails instead of hanging the run.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Leftovers older than this belong to a run that was killed; a younger one
/// may still be in progress in another process.
const STALE_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const PART_MARKER: &str = ".part.";

fn cache_root() -> Result<PathBuf, String> {
    let dirs = directories::ProjectDirs::from("", "", "jxlall")
        .ok_or("no home directory for the download cache")?;
    Ok(dirs.cache_dir().to_path_buf())
}

/// Path of the verified file, downloading it first when missing or corrupt.
pub fn ensure(spec: &Remote) -> Result<PathBuf, String> {
    let dir = cache_root()?.join("models");
    let path = dir.join(spec.file_name);
    match verify_file(&path, spec.size, spec.sha256) {
        Ok(()) => return Ok(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "note: cached {} unusable ({e}); downloading again",
            path.display()
        ),
    }
    create_dir(&dir)?;
    remove_stale_parts(&dir, spec.file_name);
    announce(spec.file_name, spec.size, &path);
    let tmp = part_path(&dir, spec.file_name);
    let result = download(spec.url, spec.size, spec.sha256, &tmp).and_then(|()| {
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            // Windows: another run finished first and has the file open.
            Err(_) if verify_file(&path, spec.size, spec.sha256).is_ok() => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| format!("download {}: {e}", spec.file_name))?;
    Ok(path)
}

/// Directory holding the verified files of `spec`, installing them first when
/// missing or corrupt. Installation is atomic: extraction happens in a
/// temporary directory that is renamed into place.
pub fn ensure_archive(spec: &Archive) -> Result<PathBuf, String> {
    let root = cache_root()?.join("runtime");
    let dir = root.join(spec.id);
    if verify_installed(&dir, spec.files).is_ok() {
        return Ok(dir);
    }
    create_dir(&root)?;
    let archive_name = format!("{}-archive", spec.id);
    remove_stale_parts(&root, spec.id);
    remove_stale_parts(&root, &archive_name);
    announce(spec.id, spec.size, &dir);
    let download_path = part_path(&root, &archive_name);
    let extract_dir = part_path(&root, spec.id);
    let result = install(spec, &download_path, &extract_dir, &dir);
    let _ = std::fs::remove_file(&download_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    result.map_err(|e| format!("install {}: {e}", spec.id))?;
    Ok(dir)
}

fn install(
    spec: &Archive,
    download_path: &Path,
    extract_dir: &Path,
    dir: &Path,
) -> Result<(), String> {
    download(spec.url, spec.size, spec.sha256, download_path)?;
    std::fs::create_dir_all(extract_dir).map_err(|e| e.to_string())?;
    extract(download_path, spec.format, spec.files, extract_dir)?;
    publish(extract_dir, dir, spec.files)
}

/// Move a finished extraction into place. A valid install is left alone: a
/// concurrent run may have just finished it and be loading from it.
fn publish(extract_dir: &Path, dir: &Path, files: &[ArchiveFile]) -> Result<(), String> {
    if verify_installed(dir, files).is_ok() {
        let _ = std::fs::remove_dir_all(extract_dir);
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(dir);
    match std::fs::rename(extract_dir, dir) {
        Ok(()) => Ok(()),
        Err(_) if verify_installed(dir, files).is_ok() => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn create_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download cache {}: {e}", dir.display()))
}

fn announce(what: &str, size: u64, target: &Path) {
    eprintln!(
        "downloading {what} ({} MB) to {}",
        size.div_ceil(BYTES_PER_MB),
        target.display()
    );
}

fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{PART_MARKER}{}", std::process::id()))
}

fn remove_stale_parts(dir: &Path, name: &str) {
    let prefix = format!("{name}{PART_MARKER}");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > STALE_PART_AGE));
        if stale && entry.file_name().to_string_lossy().starts_with(&prefix) {
            let path = entry.path();
            let _ = std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path));
        }
    }
}

fn download(url: &str, size: u64, sha256: &str, dest: &Path) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .build()
        .into();
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    // One byte past the expected size is enough to detect an oversize body.
    let body = response.into_body().into_reader().take(size + 1);
    let mut file = File::create(dest).map_err(|e| e.to_string())?;
    let hash = copy_hashing(body, &mut file).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    check(hash, size, sha256).map_err(|e| e.to_string())
}

/// Extract the listed entries of an archive into `out` under their fixed names,
/// verifying each one. Paths inside the archive are never used to write.
fn extract(
    archive: &Path,
    format: ArchiveFormat,
    files: &[ArchiveFile],
    out: &Path,
) -> Result<(), String> {
    let source = File::open(archive).map_err(|e| e.to_string())?;
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(source).map_err(|e| e.to_string())?;
            for file in files {
                let entry = zip
                    .by_name(file.entry)
                    .map_err(|e| format!("{}: {e}", file.entry))?;
                write_entry(entry, file, out)?;
            }
            Ok(())
        }
        ArchiveFormat::TarGz => {
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(source));
            let mut remaining: Vec<&ArchiveFile> = files.iter().collect();
            for entry in tar.entries().map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?.into_owned();
                let found = remaining.iter().position(|f| path == Path::new(f.entry));
                if let Some(index) = found {
                    write_entry(entry, remaining.swap_remove(index), out)?;
                }
            }
            match remaining.first() {
                Some(missing) => Err(format!("{}: not in archive", missing.entry)),
                None => Ok(()),
            }
        }
    }
}

fn write_entry(entry: impl Read, file: &ArchiveFile, out: &Path) -> Result<(), String> {
    let mut dest = File::create(out.join(file.name)).map_err(|e| e.to_string())?;
    let hash = copy_hashing(entry.take(file.size + 1), &mut dest).map_err(|e| e.to_string())?;
    check(hash, file.size, file.sha256).map_err(|e| format!("{}: {e}", file.entry))
}

fn verify_installed(dir: &Path, files: &[ArchiveFile]) -> io::Result<()> {
    files
        .iter()
        .try_for_each(|f| verify_file(&dir.join(f.name), f.size, f.sha256))
}

fn verify_file(path: &Path, size: u64, sha256: &str) -> io::Result<()> {
    let file = File::open(path)?;
    if file.metadata()?.len() != size {
        return Err(io::Error::other("size mismatch"));
    }
    check(copy_hashing(file, &mut io::sink())?, size, sha256)
}

/// Copy `src` into `dst`; returns the byte count and SHA-256 digest.
fn copy_hashing(mut src: impl Read, dst: &mut impl Write) -> io::Result<(u64, Vec<u8>)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok((total, hasher.finalize().to_vec()));
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
}

fn check((len, digest): (u64, Vec<u8>), size: u64, sha256: &str) -> io::Result<()> {
    if len != size {
        return Err(io::Error::other(format!(
            "got {len} bytes, expected {size}"
        )));
    }
    if hex(&digest) != sha256 {
        return Err(io::Error::other("SHA-256 mismatch"));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of "abc" (FIPS 180-2 example).
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jxlall-fetch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn abc_file(entry: &'static str, name: &'static str) -> ArchiveFile {
        ArchiveFile {
            entry,
            name,
            sha256: ABC_SHA256,
            size: 3,
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_tgz(path: &Path, entries: &[(&str, &[u8])]) {
        let gz =
            flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn verifies_size_and_digest() {
        let dir = scratch("verify");
        let path = dir.join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert!(verify_file(&path, 3, ABC_SHA256).is_ok());
        assert!(verify_file(&path, 4, ABC_SHA256).is_err());
        assert!(verify_file(&path, 3, &ABC_SHA256.replace('b', "c")).is_err());
        std::fs::remove_file(&path).unwrap();
        let missing = verify_file(&path, 3, ABC_SHA256).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn extracts_only_listed_files_under_fixed_names() {
        let dir = scratch("extract");
        let files = [abc_file("top/lib/real.so.1.2", "real.so.1")];
        let entries: &[(&str, &[u8])] = &[("top/lib/real.so.1.2", b"abc"), ("top/other", b"xyz")];
        for (format, path) in [
            (ArchiveFormat::Zip, dir.join("a.zip")),
            (ArchiveFormat::TarGz, dir.join("a.tgz")),
        ] {
            match format {
                ArchiveFormat::Zip => write_zip(&path, entries),
                ArchiveFormat::TarGz => write_tgz(&path, entries),
            }
            let out = dir.join(format!("{format:?}"));
            std::fs::create_dir_all(&out).unwrap();
            extract(&path, format, &files, &out).unwrap();
            assert_eq!(std::fs::read(out.join("real.so.1")).unwrap(), b"abc");
            assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn extraction_rejects_wrong_content_and_missing_entries() {
        let dir = scratch("reject");
        let path = dir.join("a.zip");
        write_zip(&path, &[("wrong", b"abd")]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(extract(&path, ArchiveFormat::Zip, &[abc_file("wrong", "lib")], &out).is_err());
        assert!(
            extract(
                &path,
                ArchiveFormat::Zip,
                &[abc_file("absent", "lib")],
                &out
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tar_extraction_reports_a_missing_entry() {
        let dir = scratch("tar-missing");
        let path = dir.join("a.tgz");
        write_tgz(&path, &[("other", b"abc")]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(
            extract(
                &path,
                ArchiveFormat::TarGz,
                &[abc_file("absent", "lib")],
                &out
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publish_keeps_a_valid_install_and_replaces_a_corrupt_one() {
        let root = scratch("publish");
        let (dir, staged) = (root.join("dir"), root.join("staged"));
        let files = [abc_file("e", "lib")];
        let stage = || {
            std::fs::create_dir_all(&staged).unwrap();
            std::fs::write(staged.join("lib"), b"abc").unwrap();
        };
        stage();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib"), b"abd").unwrap();
        publish(&staged, &dir, &files).unwrap();
        assert_eq!(std::fs::read(dir.join("lib")).unwrap(), b"abc");
        assert!(!staged.exists());

        // A concurrent run finished first and may be loading from its copy.
        stage();
        std::fs::write(dir.join("marker"), b"").unwrap();
        publish(&staged, &dir, &files).unwrap();
        assert!(dir.join("marker").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tampered_installed_file_fails_verification() {
        let dir = scratch("tamper");
        let files = [abc_file("e", "lib")];
        assert!(verify_installed(&dir, &files).is_err());
        std::fs::write(dir.join("lib"), b"abc").unwrap();
        assert!(verify_installed(&dir, &files).is_ok());
        std::fs::write(dir.join("lib"), b"abd").unwrap();
        assert!(verify_installed(&dir, &files).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
