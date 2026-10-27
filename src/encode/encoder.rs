//! Encoder handle: RAII over encoder + parallel runner.

use jpegxl_sys::encoder::encode::*;
use jpegxl_sys::threads::resizable_parallel_runner::*;
use std::ffi::c_void;
use std::ptr::null;

#[derive(Debug)]
pub struct EncodeError(String);

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

pub fn err<T>(msg: impl Into<String>) -> Result<T, EncodeError> {
    Err(EncodeError(msg.into()))
}

pub fn check(status: JxlEncoderStatus, what: &str) -> Result<(), EncodeError> {
    match status {
        JxlEncoderStatus::Success => Ok(()),
        JxlEncoderStatus::Error => err(format!("libjxl error during {what}")),
        JxlEncoderStatus::NeedMoreOutput => err(format!("unexpected NeedMoreOutput during {what}")),
    }
}

/// RAII guard: encoder + resizable parallel runner.
pub struct Enc {
    pub(crate) enc: *mut JxlEncoder,
    runner: *mut c_void,
}

impl Enc {
    pub fn new() -> Result<Self, EncodeError> {
        unsafe {
            let runner = JxlResizableParallelRunnerCreate(null());
            if runner.is_null() {
                return err("JxlResizableParallelRunnerCreate failed");
            }
            let enc = JxlEncoderCreate(null());
            if enc.is_null() {
                JxlResizableParallelRunnerDestroy(runner);
                return err("JxlEncoderCreate failed");
            }
            // One internal thread; file-level parallelism drives the cores.
            JxlResizableParallelRunnerSetThreads(runner, 1);
            let s = JxlEncoderSetParallelRunner(enc, JxlResizableParallelRunner, runner);
            if s != JxlEncoderStatus::Success {
                JxlEncoderDestroy(enc);
                JxlResizableParallelRunnerDestroy(runner);
                return err("JxlEncoderSetParallelRunner failed");
            }
            Ok(Self { enc, runner })
        }
    }

    pub fn collect_output(self) -> Result<Vec<u8>, EncodeError> {
        unsafe {
            let mut out = Vec::new();
            let mut buf = [0u8; 1 << 16];
            loop {
                let mut next_out: *mut u8 = buf.as_mut_ptr();
                let mut avail_out: usize = buf.len();
                let status =
                    JxlEncoderProcessOutput(self.enc, &raw mut next_out, &raw mut avail_out);
                let used = buf.len() - avail_out;
                out.extend_from_slice(&buf[..used]);
                match status {
                    JxlEncoderStatus::Success => break,
                    JxlEncoderStatus::NeedMoreOutput => continue,
                    JxlEncoderStatus::Error => return err("JxlEncoderProcessOutput error"),
                }
            }
            drop(self);
            Ok(out)
        }
    }
}

impl Drop for Enc {
    fn drop(&mut self) {
        unsafe {
            JxlEncoderDestroy(self.enc);
            JxlResizableParallelRunnerDestroy(self.runner);
        }
    }
}
