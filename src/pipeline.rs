//! Conversion pipeline: bounded-parallelism driver and per-file conversion.

use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use compio::io::AsyncWriteAtExt;

use crate::cli::RunOptions;
use crate::domain::{Decoded, DecodedImage};
use crate::encode::{EncodeError, EncodeSettings, NoiseStrength};
use crate::inputs::InputFile;
use crate::sniff::Format;
use crate::stats::{Bytes, Stats};

/// Reserved-output registry shared by all tasks (the runtime is
/// single-threaded). Page-derived names are claimed at write time, so both
/// base and page paths are collision-free.
type Claimed = Rc<RefCell<HashSet<PathBuf>>>;

/// Exclusive-create attempts for a tmp name before giving up.
const TMP_NAME_ATTEMPTS: usize = 8;

/// Comparison key for the claimed set, independent of path spelling:
/// `a.jxl`, `./a.jxl` and `/cwd/a.jxl` must collide. The path is made
/// absolute, its longest existing ancestor directory canonicalized and the
/// not-yet-existing rest (`-o` dirs are created later) normalized lexically.
/// The file name itself is never resolved. Case is not folded for
/// case-insensitive filesystems: such collisions surface as `AlreadyExists`
/// at the no-replace publish and move on to the next name.
fn claim_key(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut key = abs.clone();
    if let Some(parent) = abs.parent() {
        for ancestor in parent.ancestors() {
            if let Ok(canonical) = ancestor.canonicalize() {
                key = canonical;
                for c in abs.strip_prefix(ancestor).unwrap_or(&abs).components() {
                    match c {
                        Component::ParentDir => {
                            key.pop();
                        }
                        Component::CurDir => {}
                        c => key.push(c),
                    }
                }
                break;
            }
        }
    }
    key
}

/// Convert `files` with at most `opts.jobs` conversions in flight.
///
/// Returns `Err` only when the async driver itself cannot start; per-file
/// failures are counted in `stats`.
pub fn run(opts: RunOptions, files: Vec<InputFile>, stats: Arc<Stats>) -> Result<(), String> {
    let jobs = opts.jobs;
    let keep_going = opts.keep_going;
    let opts = Arc::new(opts);
    let total = files.len();
    // Reserve output paths up front (inputs are sorted ⇒ deterministic
    // naming) so same-stem inputs from different formats cannot overwrite
    // each other — lethal under --delete-originals.
    // Every input's own path is reserved too: an output name that would
    // land on a still-unconverted sibling input (e.g. `a.png`'s natural
    // `a.jxl` when `a.jxl` is itself an input) is a collision.
    let claimed: Claimed = Rc::new(RefCell::new(
        files.iter().map(|f| claim_key(&f.path)).collect(),
    ));
    let tasks: Vec<Task> = files
        .into_iter()
        .map(|f| {
            let name = OutputName::new(&f.path, opts.output.as_deref(), f.root.as_deref());
            let dst = claim(&mut claimed.borrow_mut(), |n| name.candidate(n));
            Task {
                name,
                dst,
                path: f.path,
                opts: Arc::clone(&opts),
                stats: Arc::clone(&stats),
                claimed: Rc::clone(&claimed),
                total,
            }
        })
        .collect();
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("runtime init: {e}"))?;
    runtime.block_on(async move {
        // `jobs` workers pull from one queue, so a slow file only occupies
        // its own slot.
        let queue = Rc::new(RefCell::new(tasks.into_iter()));
        let workers: Vec<_> = (0..jobs.min(total))
            .map(|_| {
                let queue = Rc::clone(&queue);
                let stats = Arc::clone(&stats);
                compio::runtime::spawn(async move {
                    loop {
                        if !keep_going && stats.failed() > 0 {
                            break;
                        }
                        let Some(task) = queue.borrow_mut().next() else {
                            break;
                        };
                        if let Err(e) = compio::runtime::spawn(task.process()).await {
                            // Worker panicked (decoder bug): count it so the
                            // run fails loudly instead of exiting successfully.
                            eprintln!("error: worker panicked: {e}");
                            stats.record_fail();
                        }
                    }
                })
            })
            .collect();
        for w in workers {
            // Panics are caught per task above; the worker loop itself
            // cannot panic.
            let _ = w.await;
        }
        if !opts.verbose {
            eprintln!();
        }
    });
    Ok(())
}

/// One file's conversion job: source, reserved output base path, and the
/// shared run context.
struct Task {
    path: PathBuf,
    name: OutputName,
    dst: PathBuf,
    opts: Arc<RunOptions>,
    stats: Arc<Stats>,
    claimed: Claimed,
    total: usize,
}

impl Task {
    async fn process(self) {
        let Self {
            path,
            name,
            dst,
            opts,
            stats,
            claimed,
            total,
        } = self;
        let verbose = opts.verbose;
        let result = convert_file(&path, &name, &dst, &claimed, &opts, verbose).await;
        match result {
            Ok((n_in, n_out, written)) => {
                // Last line of defence: never remove a source that is the
                // same file as an output, however it was spelled.
                if opts.delete_originals {
                    if !written.iter().all(|w| is_distinct_file(&path, w)) {
                        eprintln!(
                            "warning: {}: not deleting source: cannot confirm outputs are separate files",
                            path.display()
                        );
                    } else if let Err(e) = name.sync_dirs().await {
                        // Outputs are fine, just not provably durable.
                        eprintln!(
                            "warning: {}: not deleting source: sync output dir: {e}",
                            path.display()
                        );
                    } else if let Err(e) = compio::fs::remove_file(&path).await {
                        eprintln!("warning: {}: delete source: {e}", path.display());
                    }
                }
                stats.record_ok(n_in, n_out);
                if verbose {
                    eprintln!(
                        "converted {} ({} -> {})",
                        path.display(),
                        Bytes(n_in),
                        Bytes(n_out)
                    );
                }
            }
            Err(e) => {
                stats.record_fail();
                eprintln!("error: {}: {e}", path.display());
            }
        }
        if !opts.verbose {
            eprint!("\r{}/{}          ", stats.done(), total);
        }
    }
}

/// Unix: same device and inode. Elsewhere: same canonical path, so hard
/// links are not detected on Windows. Any stat failure counts as "not
/// distinct" so the caller keeps the source.
fn is_distinct_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => (x.dev(), x.ino()) != (y.dev(), y.ino()),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x != y,
            _ => false,
        }
    }
}

async fn convert_file(
    path: &Path,
    name: &OutputName,
    dst: &Path,
    claimed: &Claimed,
    opts: &RunOptions,
    verbose: bool,
) -> Result<(u64, u64, Vec<PathBuf>), String> {
    let force_lossy_jpeg = opts.force_lossy_jpeg;
    let settings = &opts.settings;
    let data = compio::fs::read(path)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let n_in = data.len() as u64;

    let format = crate::sniff::sniff(&data).ok_or_else(|| "unrecognized format".to_string())?;
    if verbose {
        eprintln!("converting {}: format {format:?}", path.display());
    }

    // Arc: decode needs the bytes on a blocking thread without a copy;
    // the JPEG passthrough shares this same buffer into the encoder.
    let data = Arc::new(data);
    let outputs = {
        let data = Arc::clone(&data);
        let path = path.to_path_buf();
        let settings = *settings;
        compio::runtime::spawn_blocking(move || {
            encode_all(&data, format, force_lossy_jpeg, &settings, &path)
        })
        .await
        .map_err(|e| format!("worker: {e}"))??
    };
    if outputs.is_empty() {
        return Err("no images decoded".into());
    }

    // The source is about to be deleted: make the output durable first.
    let durable = opts.delete_originals;
    let (n_out, written) = write_outputs(name, dst, outputs, claimed, durable).await?;
    Ok((n_in, n_out, written))
}

fn encode_all(
    data: &Arc<Vec<u8>>,
    format: Format,
    force_lossy_jpeg: bool,
    settings: &EncodeSettings,
    path: &Path,
) -> Result<Vec<Vec<u8>>, String> {
    let decoded = crate::decode::decode(data, format)?;
    let mut out = Vec::with_capacity(decoded.len());
    for d in decoded {
        let jxl = match d {
            Decoded::Jpeg(jpeg) => {
                if force_lossy_jpeg {
                    encode_pixel_from_jpeg(&jpeg, settings, path)?
                } else {
                    match crate::encode::encode_jpeg(&jpeg, settings) {
                        Ok(jxl) => jxl,
                        Err(e) => {
                            // jbrd cannot represent this JPEG (CMYK,
                            // arithmetic, ...): fall back to pixels.
                            eprintln!(
                                "note: {}: jbrd rejected ({e}); re-encoding pixels",
                                path.display()
                            );
                            encode_pixel_from_jpeg(&jpeg, settings, path)?
                        }
                    }
                }
            }
            Decoded::Image(img) => encode_decoded_image(*img, settings, path)?,
        };
        out.push(jxl);
    }
    Ok(out)
}

fn encode_decoded_image(
    img: DecodedImage,
    settings: &EncodeSettings,
    path: &Path,
) -> Result<Vec<u8>, String> {
    // Consumed by value: metadata boxes and pixels move through, no clone.
    let DecodedImage {
        frame,
        color,
        mut meta,
        orientation,
        hdr,
        gain_map,
    } = img;
    if let Some(gm) = gain_map.as_ref() {
        meta.jhgm = Some(gm.to_jhgm(settings).map_err(gain_map_err)?);
    }
    // Only lossy pixel frames get grain (see encode_image); cli notes once.
    let iso = settings
        .noise
        .filter(|_| !settings.distance.is_lossless())
        .and_then(|strength| {
            photon_noise_iso(meta.exif.as_deref().map(Vec::as_slice), strength, path)
        });
    crate::encode::encode_image(&frame, &color, &meta, orientation, &hdr, settings, iso)
        .map_err(|e| e.to_string())
}

/// libjxl photon-noise ISO for one file: full-frame-equivalent EXIF ISO
/// scaled by the user's strength. `None` (with a note) without an ISO.
fn photon_noise_iso(exif: Option<&[u8]>, strength: NoiseStrength, path: &Path) -> Option<f32> {
    let Some(estimate) = exif.and_then(crate::domain::iso_from_exif) else {
        eprintln!(
            "note: {}: no ISO in metadata, no grain added",
            path.display()
        );
        return None;
    };
    if estimate.crop.is_none() {
        eprintln!(
            "note: {}: no 35 mm-equivalent focal length, ISO {} used without crop correction",
            path.display(),
            estimate.iso
        );
    }
    Some((estimate.full_frame_iso() * f64::from(strength.get())) as f32)
}

fn gain_map_err(e: EncodeError) -> String {
    format!("gain map: {e}")
}

/// Forced-lossy JPEG: pixel decode (zune-jpeg) then VarDCT encode.
fn encode_pixel_from_jpeg(
    jpeg: &[u8],
    settings: &EncodeSettings,
    path: &Path,
) -> Result<Vec<u8>, String> {
    match crate::format::jpeg::decode(jpeg)? {
        Decoded::Image(img) => encode_decoded_image(*img, settings, path),
        Decoded::Jpeg(_) => Err("internal: jpeg pixel decode returned passthrough".into()),
    }
}

/// Output naming scheme for one source file: `{stem}.jxl`, then
/// `{stem}.{ext}.jxl`, then `{stem}.{ext}.N.jxl` (N ≥ 2). `-o` with a
/// scanned directory mirrors the input layout; explicit files land flat in
/// `-o`.
struct OutputName {
    dir: PathBuf,
    /// Topmost directory to fsync: the parent of `-o` (which may have been
    /// created by this run), or the source's own directory in place.
    sync_top: PathBuf,
    stem: String,
    ext: String,
}

impl OutputName {
    fn new(src: &Path, out_dir: Option<&Path>, root: Option<&Path>) -> Self {
        let dir = match (out_dir, root) {
            // Mirror the input layout relative to the scanned root: the
            // directory is the parent of the mirrored relative path.
            (Some(dir), Some(root)) => dir
                .join(src.strip_prefix(root).unwrap_or(src))
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dir.to_path_buf()),
            (Some(dir), None) => dir.to_path_buf(),
            _ => src.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bin".into());
        let sync_top = match out_dir {
            Some(out) => out.parent().unwrap_or(out).to_path_buf(),
            None => dir.clone(),
        };
        Self {
            dir,
            sync_top,
            stem,
            ext,
        }
    }

    fn candidate(&self, n: u32) -> PathBuf {
        let Self { dir, stem, ext, .. } = self;
        dir.join(match n {
            0 => format!("{stem}.jxl"),
            1 => format!("{stem}.{ext}.jxl"),
            n => format!("{stem}.{ext}.{n}.jxl"),
        })
    }

    /// Persist the published names and every directory `-o` may have
    /// created. Always the whole chain: another task may have created a
    /// shared directory without having synced its parent yet.
    async fn sync_dirs(&self) -> io::Result<()> {
        for dir in self.dir.ancestors() {
            sync_dir(dir).await?;
            if dir == self.sync_top {
                break;
            }
        }
        Ok(())
    }
}

/// Page `page` (0-based) of a multi-page document derives from the claimed
/// base name: `{stem}_pN.jxl`, then `{stem}_pN.M.jxl` (M ≥ 2).
fn page_candidate(base: &Path, page: usize, n: u32) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    let name = format!("{stem}_p{}", page + 1);
    base.with_file_name(match n {
        0 => format!("{name}.jxl"),
        n => format!("{name}.{}.jxl", n + 1),
    })
}

/// Claim the first candidate not yet taken by an input or another output
/// of this run. Existing unrelated files are handled at publish time.
fn claim(claimed: &mut HashSet<PathBuf>, candidate: impl Fn(u32) -> PathBuf) -> PathBuf {
    (0..)
        .map(candidate)
        .find(|p| claimed.insert(claim_key(p)))
        .expect("candidate names are unbounded")
}

/// Write all pages of one converted file; returns bytes written and the
/// paths actually used. Page names are claimed at write time (they are only
/// known once the page count is). On any failure the pages already written
/// are removed, so a document is either fully converted or not at all.
/// `durable`: fsync each file before publishing.
async fn write_outputs(
    name: &OutputName,
    dst_base: &Path,
    outputs: Vec<Vec<u8>>,
    claimed: &Claimed,
    durable: bool,
) -> Result<(u64, Vec<PathBuf>), String> {
    let mut total = 0u64;
    let pages = outputs.len();
    let mut written = Vec::with_capacity(pages);
    compio::fs::create_dir_all(&name.dir)
        .await
        .map_err(|e| format!("mkdir: {e}"))?;
    for (idx, bytes) in outputs.into_iter().enumerate() {
        total += bytes.len() as u64;
        let next = || {
            let mut set = claimed.borrow_mut();
            if pages == 1 {
                claim(&mut set, |n| name.candidate(n))
            } else {
                claim(&mut set, |n| page_candidate(dst_base, idx, n))
            }
        };
        // Single-page files use the already-claimed base first.
        let first = if pages == 1 {
            dst_base.to_path_buf()
        } else {
            next()
        };
        match write_new(first, bytes, durable, next).await {
            Ok(dst) => written.push(dst),
            Err(e) => {
                for p in &written {
                    let _ = compio::fs::remove_file(p).await;
                }
                return Err(e);
            }
        }
    }
    Ok((total, written))
}

/// Atomic no-clobber write: exclusive-create tmp in the destination
/// directory under an unguessable name (defeats symlink pre-planting), then
/// publish it under `dst` without replacing anything. If a file already has
/// that name, `next` supplies the next candidate. Returns the path used; the
/// tmp is removed on failure.
async fn write_new(
    mut dst: PathBuf,
    bytes: Vec<u8>,
    durable: bool,
    next: impl Fn() -> PathBuf,
) -> Result<PathBuf, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut opened = None;
    for _ in 0..TMP_NAME_ATTEMPTS {
        let name = tmp_name(&dst, SEQ.fetch_add(1, Ordering::Relaxed));
        let mut opts = compio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        match opts.open(&name).await {
            Ok(file) => {
                opened = Some((file, name));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create tmp: {e}")),
        }
    }
    let (mut file, tmp) =
        opened.ok_or_else(|| "cannot create tmp file: name collisions".to_string())?;
    let compio::BufResult(mut res, _) = file.write_all_at(bytes, 0).await;
    if res.is_ok() && durable {
        res = file.sync_all().await;
    }
    drop(file); // close before publishing: Windows cannot rename an open file
    if let Err(e) = res {
        let _ = compio::fs::remove_file(&tmp).await;
        return Err(format!("write tmp: {e}"));
    }
    loop {
        match publish_noreplace(&tmp, &dst).await {
            Ok(()) => return Ok(dst),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => dst = next(),
            Err(e) => {
                let _ = compio::fs::remove_file(&tmp).await;
                return Err(format!("publish: {e}"));
            }
        }
    }
}

/// Move `tmp` to `dst`, failing with `AlreadyExists` instead of replacing an
/// existing file. Linux: `renameat2(RENAME_NOREPLACE)`; filesystems without
/// it, and other platforms, use the link scheme.
async fn publish_noreplace(tmp: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    match rename_noreplace(tmp, dst) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) => {}
        r => return r,
    }
    link_noreplace(tmp, dst).await
}

/// Hard-link then unlink (`link` never replaces). Filesystems without hard
/// links (FAT, exFAT, some network shares) reserve `dst` with an exclusive
/// create instead, then rename over that placeholder.
async fn link_noreplace(tmp: &Path, dst: &Path) -> io::Result<()> {
    match compio::fs::hard_link(tmp, dst).await {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => return placeholder_noreplace(tmp, dst).await,
    }
    if let Err(e) = compio::fs::remove_file(tmp).await {
        eprintln!("warning: {}: remove tmp: {e}", tmp.display());
    }
    Ok(())
}

async fn placeholder_noreplace(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut opts = compio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    drop(opts.open(dst).await?);
    let res = compio::fs::rename(tmp, dst).await;
    if res.is_err() {
        let _ = compio::fs::remove_file(dst).await;
    }
    res
}

#[cfg(target_os = "linux")]
fn rename_noreplace(tmp: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_path = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other);
    let (from, to) = (c_path(tmp)?, c_path(dst)?);
    // Raw syscall: the musl bundled with Rust's static target has no
    // renameat2 wrapper.
    // SAFETY: both arguments are valid NUL-terminated strings that outlive
    // the call; AT_FDCWD resolves them like rename(2).
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Persist directory entries (the published names). Windows has no
/// directory fsync; NTFS journals metadata itself.
async fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        compio::fs::File::open(dir).await?.sync_all().await
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Random tmp name. `RandomState` is seeded from the OS, so the hash is
/// unguessable to a local attacker; combined with `create_new`, pre-planted
/// symlinks or files simply collide and the next name is tried.
fn tmp_name(dst: &Path, seq: u64) -> PathBuf {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(u64::from(std::process::id()));
    h.write_u64(seq);
    h.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    );
    dst.with_extension(format!("jxl.tmp.{:016x}", h.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh scratch directory per test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jxlall-pipeline-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        compio::runtime::Runtime::new().unwrap().block_on(f)
    }

    fn claimed_with_inputs(inputs: &[&Path]) -> HashSet<PathBuf> {
        inputs.iter().map(|p| claim_key(p)).collect()
    }

    fn claim_for(src: &Path, out_dir: Option<&Path>, set: &mut HashSet<PathBuf>) -> PathBuf {
        let name = OutputName::new(src, out_dir, None);
        claim(set, |n| name.candidate(n))
    }

    #[test]
    fn claim_key_ignores_spelling() {
        let cwd = std::env::current_dir().unwrap();
        let key = claim_key(Path::new("not-there.jxl"));
        assert_eq!(key, claim_key(Path::new("./not-there.jxl")));
        assert_eq!(key, claim_key(&cwd.join("not-there.jxl")));
        // Not-yet-created `-o` directories normalize lexically.
        let dir = scratch("key");
        assert_eq!(
            claim_key(&dir.join("new/../x/a.jxl")),
            claim_key(&dir.join("x/a.jxl"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn same_stem_inputs_spelled_differently_get_distinct_outputs() {
        let (jpg, png) = (Path::new("./a.jpg"), Path::new("a.png"));
        let mut set = claimed_with_inputs(&[jpg, png]);
        let first = claim_for(jpg, None, &mut set);
        let second = claim_for(png, None, &mut set);
        assert_eq!(first, Path::new("./a.jxl"));
        assert_eq!(second, Path::new("a.png.jxl"));
    }

    #[test]
    fn output_dir_spelled_absolute_never_claims_relative_source() {
        // `-o $PWD a.jxl`: the natural output is the source itself.
        let cwd = std::env::current_dir().unwrap();
        let src = Path::new("a.jxl");
        let mut set = claimed_with_inputs(&[src]);
        assert_eq!(claim_for(src, Some(&cwd), &mut set), cwd.join("a.jxl.jxl"));
    }

    #[test]
    fn page_candidates_follow_suffix_scheme() {
        let base = Path::new("d/a.jxl");
        assert_eq!(page_candidate(base, 0, 0), Path::new("d/a_p1.jxl"));
        assert_eq!(page_candidate(base, 1, 1), Path::new("d/a_p2.2.jxl"));
    }

    #[test]
    fn publish_never_replaces_existing_file() {
        let dir = scratch("publish");
        let (tmp, dst) = (dir.join("t"), dir.join("a.jxl"));
        std::fs::write(&tmp, b"new").unwrap();
        std::fs::write(&dst, b"old").unwrap();
        let err = block_on(publish_noreplace(&tmp, &dst)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        std::fs::remove_file(&dst).unwrap();
        block_on(publish_noreplace(&tmp, &dst)).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert!(!tmp.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn link_fallbacks_never_replace_existing_file() {
        let dir = scratch("fallback");
        let (tmp, dst) = (dir.join("t"), dir.join("a.jxl"));
        std::fs::write(&dst, b"old").unwrap();
        let publish = |placeholder: bool, tmp: &Path, dst: &Path| {
            block_on(async {
                if placeholder {
                    placeholder_noreplace(tmp, dst).await
                } else {
                    link_noreplace(tmp, dst).await
                }
            })
        };
        for placeholder in [false, true] {
            std::fs::write(&tmp, b"new").unwrap();
            let err = publish(placeholder, &tmp, &dst).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(&dst).unwrap(), b"old");
        }
        std::fs::remove_file(&dst).unwrap();
        for placeholder in [false, true] {
            std::fs::write(&tmp, b"new").unwrap();
            publish(placeholder, &tmp, &dst).unwrap();
            assert_eq!(std::fs::read(&dst).unwrap(), b"new");
            assert!(!tmp.exists());
            std::fs::remove_file(&dst).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn existing_output_name_moves_to_next_candidate() {
        let dir = scratch("rerun");
        std::fs::write(dir.join("a.jxl"), b"unrelated").unwrap();
        std::fs::write(dir.join("a.png.jxl"), b"previous run").unwrap();
        let src = dir.join("a.png");
        let name = OutputName::new(&src, None, None);
        let mut set = claimed_with_inputs(&[&src]);
        let dst = claim(&mut set, |n| name.candidate(n));
        let claimed: Claimed = Rc::new(RefCell::new(set));
        let (_, written) = block_on(write_outputs(
            &name,
            &dst,
            vec![b"out".to_vec()],
            &claimed,
            true,
        ))
        .unwrap();
        assert_eq!(written, [dir.join("a.png.2.jxl")]);
        block_on(name.sync_dirs()).unwrap();
        assert_eq!(std::fs::read(dir.join("a.jxl")).unwrap(), b"unrelated");
        assert_eq!(
            std::fs::read(dir.join("a.png.jxl")).unwrap(),
            b"previous run"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_is_not_a_distinct_file() {
        let dir = scratch("same");
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&c, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert!(!is_distinct_file(&a, &b));
        assert!(!is_distinct_file(&a, &dir.join("missing")));
        assert!(is_distinct_file(&a, &c));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
