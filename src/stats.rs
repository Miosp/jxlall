//! Run tallies, human-readable sizes and durations, and the final report.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Byte count formatted as B/KiB/MiB/GiB.
pub struct Bytes(pub u64);

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        let (div, unit) = if n >= 1 << 30 {
            (1u64 << 30, "GiB")
        } else if n >= 1 << 20 {
            (1u64 << 20, "MiB")
        } else if n >= 1 << 10 {
            (1u64 << 10, "KiB")
        } else {
            return write!(f, "{n} B");
        };
        write!(f, "{:.2} {unit}", n as f64 / div as f64)
    }
}

/// Duration for reading: `0.04 s`, `17.6 s`, `2 min 05 s`, `1 h 02 min`.
pub struct Elapsed(pub Duration);

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.as_secs();
        if secs >= 3600 {
            write!(f, "{} h {:02} min", secs / 3600, secs / 60 % 60)
        } else if secs >= 60 {
            write!(f, "{} min {:02} s", secs / 60, secs % 60)
        } else if secs >= 10 {
            write!(f, "{:.1} s", self.0.as_secs_f64())
        } else {
            write!(f, "{:.2} s", self.0.as_secs_f64())
        }
    }
}

pub fn plural(n: u64) -> &'static str {
    if n == 1 { "file" } else { "files" }
}

/// Output size as a share of the input size.
pub fn percent_of(part: u64, whole: u64) -> f64 {
    if whole > 0 {
        part as f64 / whole as f64 * 100.0
    } else {
        0.0
    }
}

/// One source file converted.
pub struct Converted {
    pub path: PathBuf,
    pub format: &'static str,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub outputs: usize,
    pub elapsed: Duration,
    pub source_deleted: bool,
    /// `--delete-originals` kept this source, with a warning saying why.
    pub source_kept: bool,
}

#[derive(Default, Clone, Copy)]
struct FormatTally {
    files: u64,
    bytes_in: u64,
    bytes_out: u64,
}

struct Failure {
    path: PathBuf,
    message: String,
}

#[derive(Default)]
struct Tally {
    ok: u64,
    bytes_in: u64,
    bytes_out: u64,
    outputs: u64,
    larger_than_source: u64,
    sources_deleted: u64,
    sources_kept: u64,
    by_format: BTreeMap<&'static str, FormatTally>,
    slowest: Option<(PathBuf, Duration)>,
    failures: Vec<Failure>,
}

/// Aggregated per-run results, shared across worker tasks.
#[derive(Default)]
pub struct Stats {
    tally: Mutex<Tally>,
}

impl Stats {
    pub fn record_ok(&self, file: Converted) {
        let mut t = self.lock();
        t.ok += 1;
        t.bytes_in += file.bytes_in;
        t.bytes_out += file.bytes_out;
        t.outputs += file.outputs as u64;
        t.larger_than_source += u64::from(file.bytes_out > file.bytes_in);
        t.sources_deleted += u64::from(file.source_deleted);
        t.sources_kept += u64::from(file.source_kept);
        let format = t.by_format.entry(file.format).or_default();
        format.files += 1;
        format.bytes_in += file.bytes_in;
        format.bytes_out += file.bytes_out;
        if t.slowest.as_ref().is_none_or(|(_, d)| file.elapsed > *d) {
            t.slowest = Some((file.path, file.elapsed));
        }
    }

    pub fn record_fail(&self, path: &Path, message: String) {
        self.lock().failures.push(Failure {
            path: path.to_path_buf(),
            message,
        });
    }

    /// Failures so far.
    pub fn failed(&self) -> u64 {
        self.lock().failures.len() as u64
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tally> {
        self.tally.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Failures listed in full; a whole library failing should not bury the report.
const MAX_LISTED_FAILURES: usize = 20;

/// The end-of-run summary.
pub struct Report<'a> {
    pub stats: &'a Stats,
    pub total_files: u64,
    pub elapsed: Duration,
    pub settings: String,
    pub notes: u64,
    /// Inputs that could not be read or scanned, reported as they were found.
    pub input_errors: u64,
}

impl Report<'_> {
    pub fn succeeded(&self) -> bool {
        self.input_errors == 0 && self.stats.failed() == 0
    }
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.stats.lock();
        writeln!(
            f,
            "Converted {} of {} {} in {}",
            t.ok,
            self.total_files,
            plural(self.total_files),
            Elapsed(self.elapsed)
        )?;
        if t.ok > 0 {
            let change = if t.bytes_out <= t.bytes_in {
                format!("saved {}", Bytes(t.bytes_in - t.bytes_out))
            } else {
                format!("grew {}", Bytes(t.bytes_out - t.bytes_in))
            };
            writeln!(
                f,
                "  Size:     {} -> {} ({:.1}% of input, {change})",
                Bytes(t.bytes_in),
                Bytes(t.bytes_out),
                percent_of(t.bytes_out, t.bytes_in)
            )?;
            let secs = self.elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
            writeln!(
                f,
                "  Speed:    {:.2} files/s, {}/s of input",
                t.ok as f64 / secs,
                Bytes((t.bytes_in as f64 / secs) as u64)
            )?;
        }
        if t.by_format.len() > 1 {
            for (format, n) in &t.by_format {
                writeln!(
                    f,
                    "  {format}: {} {}, {} -> {} ({:.1}%)",
                    n.files,
                    plural(n.files),
                    Bytes(n.bytes_in),
                    Bytes(n.bytes_out),
                    percent_of(n.bytes_out, n.bytes_in)
                )?;
            }
        }
        if let Some((path, took)) = t.slowest.as_ref().filter(|_| t.ok > 1) {
            writeln!(f, "  Slowest:  {} ({})", path.display(), Elapsed(*took))?;
        }
        if t.outputs != t.ok {
            writeln!(f, "  Outputs:  {} {}", t.outputs, plural(t.outputs))?;
        }
        if t.larger_than_source > 0 {
            writeln!(
                f,
                "  Larger than source: {} {}",
                t.larger_than_source,
                plural(t.larger_than_source)
            )?;
        }
        writeln!(f, "  Settings: {}", self.settings)?;
        if self.notes > 0 {
            writeln!(f, "  Notes:    {}, printed above", self.notes)?;
        }
        if t.sources_deleted > 0 {
            writeln!(f, "  Originals deleted: {}", t.sources_deleted)?;
        }
        if t.sources_kept > 0 {
            writeln!(
                f,
                "  Originals kept: {}, reasons printed above",
                t.sources_kept
            )?;
        }
        let failed = t.failures.len() as u64;
        let skipped = self.total_files.saturating_sub(t.ok + failed);
        if skipped > 0 {
            writeln!(
                f,
                "  Skipped:  {skipped}, stopped after a failure (--keep-going continues)"
            )?;
        }
        if self.input_errors > 0 {
            writeln!(
                f,
                "  Unreadable inputs: {}, printed above",
                self.input_errors
            )?;
        }
        if failed > 0 {
            writeln!(f, "  Failed:   {failed}")?;
            for failure in t.failures.iter().take(MAX_LISTED_FAILURES) {
                writeln!(f, "    {}: {}", failure.path.display(), failure.message)?;
            }
            if t.failures.len() > MAX_LISTED_FAILURES {
                writeln!(
                    f,
                    "    ... and {} more",
                    t.failures.len() - MAX_LISTED_FAILURES
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn converted(name: &str, format: &'static str, mib_in: u64, mib_out: u64) -> Converted {
        Converted {
            path: name.into(),
            format,
            bytes_in: mib_in * MIB,
            bytes_out: mib_out * MIB,
            outputs: 1,
            elapsed: Duration::from_secs(2),
            source_deleted: false,
            source_kept: false,
        }
    }

    fn report(stats: &Stats, total_files: u64) -> Report<'_> {
        Report {
            stats,
            total_files,
            elapsed: Duration::from_secs(4),
            settings: "lossless, effort 4; 16 threads".into(),
            notes: 0,
            input_errors: 0,
        }
    }

    #[test]
    fn elapsed_picks_a_readable_unit() {
        let show = |d| Elapsed(d).to_string();
        assert_eq!(show(Duration::from_millis(42)), "0.04 s");
        assert_eq!(show(Duration::from_millis(17_600)), "17.6 s");
        assert_eq!(show(Duration::from_secs(125)), "2 min 05 s");
        assert_eq!(show(Duration::from_secs(3720)), "1 h 02 min");
    }

    #[test]
    fn a_single_file_report_skips_what_would_repeat_it() {
        let stats = Stats::default();
        stats.record_ok(converted("a.heic", "HEIF/AVIF", 8, 4));
        assert_eq!(
            report(&stats, 1).to_string(),
            "Converted 1 of 1 file in 4.00 s\n\
             \x20 Size:     8.00 MiB -> 4.00 MiB (50.0% of input, saved 4.00 MiB)\n\
             \x20 Speed:    0.25 files/s, 2.00 MiB/s of input\n\
             \x20 Settings: lossless, effort 4; 16 threads\n"
        );
    }

    #[test]
    fn a_batch_report_breaks_down_formats_and_names_the_slowest_file() {
        let stats = Stats::default();
        stats.record_ok(converted("a.jpg", "JPEG", 4, 3));
        let mut slow = converted("b.heic", "HEIF/AVIF", 2, 3);
        slow.elapsed = Duration::from_secs(9);
        stats.record_ok(slow);
        let text = report(&stats, 2).to_string();
        assert!(
            text.contains("  HEIF/AVIF: 1 file, 2.00 MiB -> 3.00 MiB (150.0%)\n"),
            "{text}"
        );
        assert!(
            text.contains("  JPEG: 1 file, 4.00 MiB -> 3.00 MiB (75.0%)\n"),
            "{text}"
        );
        assert!(text.contains("  Slowest:  b.heic (9.00 s)\n"), "{text}");
        assert!(text.contains("  Larger than source: 1 file\n"), "{text}");
    }

    #[test]
    fn growth_is_reported_as_growth() {
        let stats = Stats::default();
        stats.record_ok(converted("a.jpg", "JPEG", 2, 3));
        let text = report(&stats, 1).to_string();
        assert!(text.contains("(150.0% of input, grew 1.00 MiB)"), "{text}");
    }

    #[test]
    fn extra_outputs_deletions_and_notes_appear_only_when_they_happened() {
        let stats = Stats::default();
        let mut multipage = converted("a.tif", "TIFF", 4, 2);
        multipage.outputs = 3;
        multipage.source_deleted = true;
        stats.record_ok(multipage);
        let mut r = report(&stats, 1);
        r.notes = 2;
        let text = r.to_string();
        assert!(text.contains("  Outputs:  3 files\n"), "{text}");
        assert!(text.contains("  Originals deleted: 1\n"), "{text}");
        assert!(text.contains("  Notes:    2, printed above\n"), "{text}");
    }

    #[test]
    fn a_refused_deletion_gets_its_own_line() {
        let stats = Stats::default();
        let mut kept = converted("a.jpg", "JPEG", 2, 1);
        kept.source_kept = true;
        stats.record_ok(kept);
        let text = report(&stats, 1).to_string();
        assert!(
            text.contains("  Originals kept: 1, reasons printed above\n"),
            "{text}"
        );
        assert!(!text.contains("Originals deleted"), "{text}");
    }

    #[test]
    fn failures_are_listed_with_their_reasons() {
        let stats = Stats::default();
        stats.record_fail(Path::new("bad.jpg"), "unrecognized format".into());
        let r = report(&stats, 3);
        let text = r.to_string();
        assert!(
            text.starts_with("Converted 0 of 3 files in 4.00 s\n"),
            "{text}"
        );
        assert!(
            text.contains("  Skipped:  2, stopped after a failure (--keep-going continues)\n"),
            "{text}"
        );
        assert!(
            text.contains("  Failed:   1\n    bad.jpg: unrecognized format\n"),
            "{text}"
        );
        assert!(!text.contains("Speed"), "{text}");
        assert!(!r.succeeded());
    }

    #[test]
    fn a_long_failure_list_is_cut_short() {
        let stats = Stats::default();
        for i in 0..MAX_LISTED_FAILURES + 3 {
            stats.record_fail(Path::new(&format!("{i}.jpg")), "bad".into());
        }
        let text = report(&stats, (MAX_LISTED_FAILURES + 3) as u64).to_string();
        assert!(text.contains("    ... and 3 more\n"), "{text}");
        assert!(
            !text.contains(&format!("{}.jpg", MAX_LISTED_FAILURES)),
            "{text}"
        );
    }

    #[test]
    fn unreadable_inputs_fail_the_run() {
        let stats = Stats::default();
        stats.record_ok(converted("a.jpg", "JPEG", 2, 1));
        let mut r = report(&stats, 1);
        r.input_errors = 2;
        assert!(
            r.to_string()
                .contains("  Unreadable inputs: 2, printed above\n")
        );
        assert!(!r.succeeded());
    }

    #[test]
    fn a_clean_run_succeeds() {
        let stats = Stats::default();
        stats.record_ok(converted("a.jpg", "JPEG", 2, 1));
        assert!(report(&stats, 1).succeeded());
    }
}
