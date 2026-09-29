//! Engines left running, as the app and MCP servers run them.

use std::time::{Duration, Instant};

use turnscope_engine::Options;

/// How a test runs an engine: without asking models.dev for prices or
/// providers for limits, so nothing leaves the machine.
pub const OFFLINE: Options = Options {
    check_prices: false,
    read_limits: false,
};

/// Wait until `done` says so, and say how long that took; past a minute the
/// test fails, saying `what` it waited for.
///
/// A running engine reads a change as it comes, not on its look through
/// everything every ten minutes. Engines started together wait on one
/// another to start watching, a third of a second apiece in the debug
/// build tests run, and read nothing meanwhile: 32 started at once each
/// read their first change 10 to 26 s after it was written (2026-09-27). A
/// minute is ample for a test's few engines on a busy machine, and a tenth
/// of what a missed change waits for.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) -> Duration {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{what} within a minute"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    started.elapsed()
}
