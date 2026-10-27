//! Model files in the per-user cache: downloaded once, verified by SHA-256
//! on every load.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::ModelSpec;

const READ_CHUNK: usize = 1 << 20;
const BYTES_PER_MB: u64 = 1_000_000;

/// Path of the verified model file, downloading it first when missing or
/// corrupt.
pub fn ensure(spec: &ModelSpec) -> Result<PathBuf, String> {
    let dirs = directories::ProjectDirs::from("", "", "jxlall")
        .ok_or("no home directory for the model cache")?;
    let dir = dirs.cache_dir().join("models");
    let path = dir.join(spec.file_name);
    match verify_file(&path, spec.size, spec.sha256) {
        Ok(()) => return Ok(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("note: cached model {} unusable ({e}); downloading again", path.display()),
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("model cache {}: {e}", dir.display()))?;
    eprintln!(
        "downloading IQA model ({} MB) to {}\nnote: model license: {}",
        spec.size / BYTES_PER_MB,
        path.display(),
        spec.license
    );
    download(spec, &path).map_err(|e| format!("model download: {e}"))?;
    Ok(path)
}

fn download(spec: &ModelSpec, path: &Path) -> Result<(), String> {
    let tmp = path.with_extension(format!("part.{}", std::process::id()));
    let result = (|| {
        let response = ureq::get(spec.url).call().map_err(|e| e.to_string())?;
        // One byte past the expected size is enough to detect an oversize body.
        let body = response.into_body().into_reader().take(spec.size + 1);
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        let hash = copy_hashing(body, &mut file).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        check(hash, spec.size, spec.sha256).map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
        return Err(io::Error::other(format!("got {len} bytes, expected {size}")));
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

    #[test]
    fn verifies_size_and_digest() {
        let path = std::env::temp_dir().join(format!("jxlall-fetch-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert!(verify_file(&path, 3, ABC_SHA256).is_ok());
        assert!(verify_file(&path, 4, ABC_SHA256).is_err());
        assert!(verify_file(&path, 3, &ABC_SHA256.replace('b', "c")).is_err());
        std::fs::remove_file(&path).unwrap();
        let missing = verify_file(&path, 3, ABC_SHA256).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}
