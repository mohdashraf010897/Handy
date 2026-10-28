//! Test-only capture of `log` records, so tests can assert what Flow logs.
//!
//! The process has one global logger; records are kept per test thread, so
//! tests running in parallel only see their own lines.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Capture;

type Lines = Mutex<HashMap<ThreadId, Vec<(Level, String)>>>;

fn lines() -> &'static Lines {
    static LINES: OnceLock<Lines> = OnceLock::new();
    LINES.get_or_init(Default::default)
}

impl Log for Capture {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        lines()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(thread::current().id())
            .or_default()
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

/// Runs `f` and returns every record it logged on this thread, at any level.
pub fn capture(f: impl FnOnce()) -> Vec<(Level, String)> {
    static INSTALL: OnceLock<bool> = OnceLock::new();
    static LOGGER: Capture = Capture;
    let installed = *INSTALL.get_or_init(|| {
        let ok = log::set_logger(&LOGGER).is_ok();
        if ok {
            log::set_max_level(LevelFilter::Trace);
        }
        ok
    });
    assert!(installed, "another logger was installed first");
    let id = thread::current().id();
    lines()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&id);
    f();
    lines()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&id)
        .unwrap_or_default()
}
