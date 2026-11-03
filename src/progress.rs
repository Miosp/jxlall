//! Live progress bar and the log lines printed above it.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Bar length; its position is the completed fraction in these units.
const BAR_UNITS: u64 = 1000;
const TICK: Duration = Duration::from_millis(200);
const BAR_TEMPLATE: &str = "[{bar:20}] {percent:>3}%  {wide_msg:!}";
const BAR_CHARS: &str = "=> ";
/// Most of a file an in-flight estimate may claim: the encoder reports no
/// progress of its own, so only finishing proves a file is done.
const MAX_ACTIVE_CREDIT: f64 = 0.95;
/// Shown until the first file finishes: nothing before that says how long a
/// file takes.
const UNKNOWN_CLOCK: &str = "--:--";

static BAR: OnceLock<ProgressBar> = OnceLock::new();
static NOTES: AtomicU64 = AtomicU64::new(0);
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// `--verbose`, for code too far from the options to be handed it.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

#[cfg_attr(not(feature = "ai"), allow(dead_code))]
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Print one line to stderr above the progress bar. Never panics: this also runs
/// inside ONNX Runtime's log callback, where a panic aborts the process.
pub fn print_line(line: std::fmt::Arguments) {
    let print = || {
        let _ = writeln!(std::io::stderr(), "{line}");
    };
    match BAR.get() {
        Some(bar) => bar.suspend(print),
        None => print(),
    }
}

/// Count a note or warning for the final report.
pub fn count_note() {
    NOTES.fetch_add(1, Ordering::Relaxed);
}

/// Notes and warnings printed so far.
pub fn notes() -> u64 {
    NOTES.load(Ordering::Relaxed)
}

/// `eprintln!` that keeps the progress bar intact.
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::progress::print_line(format_args!($($arg)*))
    };
}

/// A `log_line!` the final report counts: something was dropped or ignored.
macro_rules! notice {
    ($($arg:tt)*) => {{
        $crate::progress::count_note();
        $crate::progress::print_line(format_args!($($arg)*))
    }};
}

/// What a file is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Reading,
    Decoding,
    Restoring,
    Rating,
    Encoding,
    Writing,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Decoding => "decoding",
            Self::Restoring => "restoring",
            Self::Rating => "rating",
            Self::Encoding => "encoding",
            Self::Writing => "writing",
        }
    }
}

struct Active {
    bytes: u64,
    started: Instant,
    stage: Stage,
    name: String,
}

#[derive(Debug, PartialEq)]
struct Estimate {
    fraction: f64,
    remaining: Option<Duration>,
}

/// Estimates progress from the time finished files took per input byte. Files
/// in flight are credited with the time they have run, so the estimate is
/// right from the first finished file however many run at once.
struct Estimator {
    total_files: usize,
    total_bytes: u64,
    finished_files: usize,
    finished_bytes: u64,
    /// Successes only: a failure says nothing about how long work takes.
    timed_bytes: u64,
    timed_secs: f64,
    active: HashMap<usize, Active>,
    best_fraction: f64,
}

impl Estimator {
    fn new(total_files: usize, total_bytes: u64) -> Self {
        Self {
            total_files,
            total_bytes,
            finished_files: 0,
            finished_bytes: 0,
            timed_bytes: 0,
            timed_secs: 0.0,
            active: HashMap::new(),
            best_fraction: 0.0,
        }
    }

    fn begin(&mut self, id: usize, bytes: u64, name: String, now: Instant) {
        let active = Active {
            bytes,
            started: now,
            stage: Stage::Reading,
            name,
        };
        self.active.insert(id, active);
    }

    fn set_stage(&mut self, id: usize, stage: Stage) {
        if let Some(active) = self.active.get_mut(&id) {
            active.stage = stage;
        }
    }

    fn finish(&mut self, id: usize, succeeded: bool, now: Instant) {
        let Some(active) = self.active.remove(&id) else {
            return;
        };
        self.finished_files += 1;
        self.finished_bytes += active.bytes;
        if succeeded {
            self.timed_bytes += active.bytes;
            self.timed_secs += now.duration_since(active.started).as_secs_f64();
        }
    }

    fn estimate(&mut self, now: Instant) -> Estimate {
        let secs_per_byte =
            (self.timed_bytes > 0).then(|| self.timed_secs / self.timed_bytes as f64);
        let in_flight = |active: &Active, secs_per_byte: f64| {
            let ran = now.duration_since(active.started).as_secs_f64();
            (ran / secs_per_byte).min(active.bytes as f64 * MAX_ACTIVE_CREDIT)
        };
        let credited = self.finished_bytes as f64
            + secs_per_byte.map_or(0.0, |spb| {
                self.active.values().map(|a| in_flight(a, spb)).sum()
            });
        if self.total_bytes > 0 {
            self.best_fraction = self.best_fraction.max(credited / self.total_bytes as f64);
        }
        let workers = self.active.len().max(1) as f64;
        let remaining = secs_per_byte.map(|spb| {
            let bytes_left = (self.total_bytes as f64 - credited).max(0.0);
            Duration::from_secs_f64(bytes_left * spb / workers)
        });
        Estimate {
            fraction: self.best_fraction,
            remaining,
        }
    }

    /// `3/8  0:12 elapsed  0:20 left  encoding a.heic`
    fn status(&self, elapsed: Duration, remaining: Option<Duration>) -> String {
        let left = remaining.map_or_else(|| UNKNOWN_CLOCK.to_string(), clock);
        let current = match self.active.len() {
            0 => String::new(),
            1 => self
                .active
                .values()
                .map(|a| format!("{} {}", a.stage.label(), a.name))
                .collect(),
            n => format!("{n} in progress"),
        };
        format!(
            "{}/{}  {} elapsed  {left} left  {current}",
            self.finished_files,
            self.total_files,
            clock(elapsed)
        )
    }
}

/// `m:ss`, or `h:mm:ss` from an hour up.
fn clock(d: Duration) -> String {
    let secs = d.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Live bar for one run; hidden when stderr is not a terminal.
pub struct Progress {
    estimator: Arc<Mutex<Estimator>>,
    /// Dropping the sender stops the ticker.
    ticker: Mutex<Option<(Sender<()>, JoinHandle<()>)>>,
}

impl Progress {
    pub fn start(total_files: usize, total_bytes: u64) -> Self {
        let bar = ProgressBar::with_draw_target(Some(BAR_UNITS), ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template(BAR_TEMPLATE)
                .expect("the bar template is valid")
                .progress_chars(BAR_CHARS),
        );
        let _ = BAR.set(bar.clone());
        let estimator = Arc::new(Mutex::new(Estimator::new(total_files, total_bytes)));
        let (stop, stopped) = mpsc::channel();
        let ticker = {
            let estimator = Arc::clone(&estimator);
            let started = Instant::now();
            std::thread::spawn(move || {
                loop {
                    let (estimate, status) = {
                        let mut estimator = lock(&estimator);
                        let estimate = estimator.estimate(Instant::now());
                        let status = estimator.status(started.elapsed(), estimate.remaining);
                        (estimate, status)
                    };
                    bar.set_position((estimate.fraction * BAR_UNITS as f64) as u64);
                    bar.set_message(status);
                    match stopped.recv_timeout(TICK) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
                bar.finish_and_clear();
            })
        };
        Self {
            estimator,
            ticker: Mutex::new(Some((stop, ticker))),
        }
    }

    pub fn begin(&self, id: usize, bytes: u64, name: String) {
        lock(&self.estimator).begin(id, bytes, name, Instant::now());
    }

    pub fn set_stage(&self, id: usize, stage: Stage) {
        lock(&self.estimator).set_stage(id, stage);
    }

    pub fn finish_file(&self, id: usize, succeeded: bool) {
        lock(&self.estimator).finish(id, succeeded, Instant::now());
    }

    /// Stop updating and clear the bar.
    pub fn finish(&self) {
        if let Some((stop, ticker)) = lock(&self.ticker).take() {
            drop(stop);
            let _ = ticker.join();
        }
    }
}

/// A panic elsewhere must not take the display down with it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// Run `count` equal files one after another, each taking `each` seconds.
    fn finish_files(e: &mut Estimator, t0: Instant, count: usize, each: u64) -> Instant {
        let mut now = t0;
        for id in 0..count {
            e.begin(id, MB, format!("{id}.jpg"), now);
            now += secs(each);
            e.finish(id, true, now);
        }
        now
    }

    #[test]
    fn nothing_finished_means_no_eta_and_an_empty_bar() {
        let t0 = Instant::now();
        let mut e = Estimator::new(1, MB);
        e.begin(0, MB, "a.heic".into(), t0);
        let estimate = e.estimate(t0 + secs(30));
        assert_eq!(estimate.fraction, 0.0);
        assert_eq!(estimate.remaining, None);
    }

    #[test]
    fn sequential_files_extrapolate_from_finished_ones() {
        let t0 = Instant::now();
        let mut e = Estimator::new(4, 4 * MB);
        let now = finish_files(&mut e, t0, 2, 10);
        let estimate = e.estimate(now);
        assert_eq!(estimate.fraction, 0.5);
        assert_eq!(estimate.remaining, Some(secs(20)));
    }

    #[test]
    fn files_in_flight_count_toward_the_eta() {
        // Eight 10 s files at once out of 16. When the first finishes the other
        // seven are nearly done too, so ten seconds remain, not 150.
        let t0 = Instant::now();
        let mut e = Estimator::new(16, 16 * MB);
        for id in 0..8 {
            e.begin(id, MB, format!("{id}.jpg"), t0);
        }
        let now = t0 + secs(10);
        e.finish(0, true, now);
        e.begin(8, MB, "8.jpg".into(), now);
        let remaining = e.estimate(now).remaining.unwrap();
        assert!(
            remaining >= secs(9) && remaining <= secs(11),
            "{remaining:?}"
        );
    }

    #[test]
    fn a_slow_file_never_claims_to_be_finished() {
        let t0 = Instant::now();
        let mut e = Estimator::new(2, 2 * MB);
        let now = finish_files(&mut e, t0, 1, 1);
        e.begin(1, MB, "slow.tif".into(), now);
        let estimate = e.estimate(now + secs(1000));
        assert!(estimate.fraction < 1.0);
        assert!(estimate.remaining.unwrap() > Duration::ZERO);
    }

    #[test]
    fn larger_files_weigh_more() {
        let t0 = Instant::now();
        let mut e = Estimator::new(2, 4 * MB);
        e.begin(0, MB, "small.jpg".into(), t0);
        let now = t0 + secs(5);
        e.finish(0, true, now);
        let estimate = e.estimate(now);
        assert_eq!(estimate.fraction, 0.25);
        assert_eq!(estimate.remaining, Some(secs(15)));
    }

    #[test]
    fn failures_advance_the_bar_but_not_the_timing() {
        let t0 = Instant::now();
        let mut e = Estimator::new(3, 3 * MB);
        e.begin(0, MB, "bad.jpg".into(), t0);
        e.finish(0, false, t0);
        let now = t0 + secs(10);
        e.begin(1, MB, "good.jpg".into(), t0);
        e.finish(1, true, now);
        let estimate = e.estimate(now);
        assert!((estimate.fraction - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(estimate.remaining, Some(secs(10)));
    }

    #[test]
    fn the_bar_never_moves_backwards() {
        let t0 = Instant::now();
        let mut e = Estimator::new(3, 3 * MB);
        let now = finish_files(&mut e, t0, 1, 1);
        e.begin(1, MB, "a.jpg".into(), now);
        let before = e.estimate(now + secs(1)).fraction;
        // A slow finish raises the time per byte, shrinking the credit of files
        // in flight.
        e.begin(2, MB, "b.jpg".into(), now);
        e.finish(1, true, now + secs(100));
        assert!(e.estimate(now + secs(100)).fraction >= before);
    }

    #[test]
    fn an_empty_run_stays_at_zero() {
        let mut e = Estimator::new(0, 0);
        assert_eq!(e.estimate(Instant::now()).fraction, 0.0);
    }

    #[test]
    fn clock_grows_hours_only_when_needed() {
        assert_eq!(clock(secs(0)), "0:00");
        assert_eq!(clock(secs(65)), "1:05");
        assert_eq!(clock(secs(3725)), "1:02:05");
    }

    #[test]
    fn status_names_a_lone_file_and_its_stage() {
        let t0 = Instant::now();
        let mut e = Estimator::new(1, MB);
        e.begin(0, MB, "a.heic".into(), t0);
        e.set_stage(0, Stage::Encoding);
        assert_eq!(
            e.status(secs(12), None),
            "0/1  0:12 elapsed  --:-- left  encoding a.heic"
        );
    }

    #[test]
    fn status_counts_concurrent_files_instead_of_naming_them() {
        let t0 = Instant::now();
        let mut e = Estimator::new(8, 8 * MB);
        e.begin(0, MB, "a.heic".into(), t0);
        e.begin(1, MB, "b.heic".into(), t0);
        assert_eq!(
            e.status(secs(12), Some(secs(20))),
            "0/8  0:12 elapsed  0:20 left  2 in progress"
        );
    }
}
