//! Conversion pipeline: bounded-parallelism driver and per-file conversion.

use std::collections::hash_map::RandomState;
use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use compio::io::AsyncWriteAtExt;

use crate::cli::RunOptions;
use crate::domain::{Decoded, DecodedImage};
use crate::encode::{EncodeSettings, EncodeError};
use crate::inputs::InputFile;
use crate::sniff::Format;
use crate::stats::{Bytes, Stats};

/// Reserved-output registry shared by all tasks. Page-derived names are
/// claimed at write time, so both base and page paths are collision-free.
type Claimed = Arc<Mutex<HashSet<PathBuf>>>;

/// Comparison key for the claimed set: Windows matches file names
/// case-insensitively, so fold there to catch on-disk collisions.
fn claim_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Convert `files` with at most `opts.jobs` conversions in flight.
///
/// Returns `Err` only when the async driver itself cannot start; per-file
/// failures are counted in `stats`.
pub fn run(opts: RunOptions, files: Vec<InputFile>, stats: Arc<Stats>) -> Result<(), String> {
    let jobs = opts.jobs.max(1);
    let keep_going = opts.keep_going;
    let opts = Arc::new(opts);
    let total = files.len();
    // Reserve output paths up front (inputs are sorted ⇒ deterministic
    // naming) so same-stem inputs from different formats cannot overwrite
    // each other — lethal under --delete-originals.
    // Every input's own path is reserved too: an output name that would
    // rename over a still-unconverted sibling input (e.g. `a.png`'s
    // natural `a.jxl` when `a.jxl` is itself an input) is a collision.
    let mut claimed_set: HashSet<PathBuf> = files.iter().map(|f| claim_key(&f.path)).collect();
    let claimed: Claimed = Arc::new(Mutex::new(HashSet::new()));
    let tasks: Vec<Task> = files
        .into_iter()
        .map(|f| {
            let dst = claim_output(
                &f.path,
                opts.output.as_deref(),
                f.root.as_deref(),
                &mut claimed_set,
            );
            Task {
                dst,
                path: f.path,
                opts: Arc::clone(&opts),
                stats: Arc::clone(&stats),
                claimed: Arc::clone(&claimed),
                total,
            }
        })
        .collect();
    // The set stays structurally valid across a poisoning panic (the
    // critical section only touches the set), so recover it.
    claimed
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .extend(claimed_set);
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("runtime init: {e}"))?;
    runtime.block_on(async move {
            for chunk in tasks.chunks(jobs) {
                let mut handles = Vec::with_capacity(chunk.len());
                for task in chunk {
                    handles.push(compio::runtime::spawn(Task::process(task.clone())));
                }
                for h in handles {
                    if let Err(e) = h.await {
                        // Worker panicked (decoder bug): count it so the run
                        // fails loudly instead of exiting successfully.
                        eprintln!("error: worker panicked: {e}");
                        stats.record_fail();
                    }
                }
                if !keep_going && stats.failed() > 0 {
                    break;
                }
            }
            if !opts.verbose {
                eprintln!();
            }
    });
    Ok(())
}

/// One file's conversion job: source, reserved output base path, and the
/// shared run context.
#[derive(Clone)]
struct Task {
    path: PathBuf,
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
            dst,
            opts,
            stats,
            claimed,
            total,
        } = self;
        let verbose = opts.verbose;
        let result =
            convert_file(&path, &dst, &claimed, &opts.settings, opts.force_lossy_jpeg, verbose)
                .await;
        match result {
            Ok((n_in, n_out, written)) => {
                // Never remove a source that a fresh output landed on (can
                // only happen for page-derived names after suffix fallback).
                if opts.delete_originals
                    && !written.contains(&path)
                    && let Err(e) = compio::fs::remove_file(&path).await
                {
                    eprintln!("warning: {}: delete source: {e}", path.display());
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

async fn convert_file(
    path: &Path,
    dst: &Path,
    claimed: &Claimed,
    settings: &EncodeSettings,
    force_lossy_jpeg: bool,
    verbose: bool,
) -> Result<(u64, u64, Vec<PathBuf>), String> {
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

    let (n_out, written) = write_outputs(dst, outputs, claimed, path).await?;
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
                    encode_pixel_from_jpeg(&jpeg, settings)?
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
                            encode_pixel_from_jpeg(&jpeg, settings)?
                        }
                    }
                }
            }
            Decoded::Image(img) => encode_decoded_image(*img, settings)?,
        };
        out.push(jxl);
    }
    Ok(out)
}

fn encode_decoded_image(img: DecodedImage, settings: &EncodeSettings) -> Result<Vec<u8>, String> {
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
    crate::encode::encode_image(&frame, &color, &meta, orientation, &hdr, settings)
        .map_err(|e| e.to_string())
}

fn gain_map_err(e: EncodeError) -> String {
    format!("gain map: {e}")
}

/// Forced-lossy JPEG: pixel decode (zune-jpeg) then VarDCT encode.
fn encode_pixel_from_jpeg(jpeg: &[u8], settings: &EncodeSettings) -> Result<Vec<u8>, String> {
    match crate::format::jpeg::decode(jpeg)? {
        Decoded::Image(img) => encode_decoded_image(*img, settings),
        Decoded::Jpeg(_) => Err("internal: jpeg pixel decode returned passthrough".into()),
    }
}

/// Reserve the output path for one source file: `{stem}.jxl`, falling back
/// to `{stem}.{ext}.jxl` and then numeric suffixes on collision with an
/// output claimed earlier in this run. `-o` with a scanned directory
/// mirrors the input layout; explicit files land flat in `-o`.
fn claim_output(
    src: &Path,
    out_dir: Option<&Path>,
    root: Option<&Path>,
    claimed: &mut HashSet<PathBuf>,
) -> PathBuf {
    let base_dir = match (out_dir, root) {
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
    let taken = |p: &Path| claimed.contains(&claim_key(p));
    // Never overwrite the source itself: an input already named `{stem}.jxl`
    // would be destroyed by the output rename.
    let mut dst = base_dir.join(format!("{stem}.jxl"));
    if dst == src || taken(&dst) {
        dst = base_dir.join(format!("{stem}.{ext}.jxl"));
        let mut n = 2u32;
        while dst == src || taken(&dst) {
            dst = base_dir.join(format!("{stem}.{ext}.{n}.jxl"));
            n += 1;
        }
    }
    claimed.insert(claim_key(&dst));
    dst
}

/// Page N of a multi-page document derives from the claimed base name.
fn page_path(base: &Path, page: usize, pages: usize) -> PathBuf {
    if pages == 1 {
        return base.to_path_buf();
    }
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    base.with_file_name(format!("{stem}_p{}.jxl", page + 1))
}

/// Write all pages of one converted file; returns bytes written and the
/// paths actually used. Page names are claimed at write time (they are only
/// known once the page count is), falling back to numeric suffixes so a
/// `a_p1.jpg` sibling can never be silently overwritten by `a.tif`'s page.
async fn write_outputs(
    dst_base: &Path,
    outputs: Vec<Vec<u8>>,
    claimed: &Claimed,
    src: &Path,
) -> Result<(u64, Vec<PathBuf>), String> {
    let mut total = 0u64;
    let pages = outputs.len();
    let mut written = Vec::with_capacity(pages);
    for (idx, bytes) in outputs.into_iter().enumerate() {
        let dst = {
            let mut set = claimed.lock().unwrap_or_else(|p| p.into_inner());
            let mut dst = page_path(dst_base, idx, pages);
            // Single-page files use the already-claimed base as-is; only
            // page-derived names need a write-time reservation.
            if dst != dst_base {
                let mut n = 2u32;
                while dst == *src || !set.insert(claim_key(&dst)) {
                    dst = page_path(dst_base, idx, pages).with_extension(format!("{n}.jxl"));
                    n += 1;
                }
            }
            dst
        };
        if let Some(parent) = dst.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("mkdir: {e}"))?;
        }
        total += bytes.len() as u64;
        write_atomic(&dst, bytes).await?;
        written.push(dst);
    }
    Ok((total, written))
}

/// Atomic write: exclusive-create tmp in the destination directory under an
/// unguessable name (defeats symlink pre-planting), then rename over the
/// destination. The tmp is removed if write or rename fails.
async fn write_atomic(dst: &Path, bytes: Vec<u8>) -> Result<(), String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut opened = None;
    for _ in 0..8 {
        let name = tmp_name(dst, SEQ.fetch_add(1, Ordering::Relaxed));
        let mut opts = compio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        // Unix extra: refuse to even follow a pre-planted symlink.
        #[cfg(unix)]
        opts.custom_flags(libc::O_NOFOLLOW);
        match opts.open(&name).await {
            Ok(file) => {
                opened = Some((file, name));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create tmp: {e}")),
        }
    }
    let (mut file, tmp) =
        opened.ok_or_else(|| "cannot create tmp file: name collisions".to_string())?;
    let compio::BufResult(res, _) = file.write_all_at(bytes, 0).await;
    drop(file); // close before rename: Windows cannot rename an open file
    if let Err(e) = res {
        let _ = compio::fs::remove_file(&tmp).await;
        return Err(format!("write tmp: {e}"));
    }
    if let Err(e) = compio::fs::rename(&tmp, dst).await {
        let _ = compio::fs::remove_file(&tmp).await;
        return Err(format!("rename: {e}"));
    }
    Ok(())
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
