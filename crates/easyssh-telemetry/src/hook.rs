//! The panic hook and the entry for frontend errors. Both only write to the
//! spool, and only while capture is on; nothing here touches the network.

use crate::report::{parse_backtrace, parse_js_stack, Report};
use crate::spool::Spool;
use std::backtrace::Backtrace;
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static CAPTURE: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);
/// A panic loop must not fill the disk with reports.
const MAX_PER_PROCESS: usize = 5;

/// Turns capture on or off; the hook checks it at panic time, so a change in
/// the settings applies without a restart.
pub fn set_capture(on: bool) {
    CAPTURE.store(on, Ordering::Relaxed);
}

pub fn capture_enabled() -> bool {
    CAPTURE.load(Ordering::Relaxed)
}

fn payload_text(info: &panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic with a non-text payload".to_owned())
}

/// Installs the hook. The previous hook still runs afterwards (stderr output).
/// Capture starts off; call [`set_capture`].
pub fn install(component: &'static str, version: &str, spool: Spool) {
    let version = version.to_owned();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if capture_enabled() && WRITTEN.fetch_add(1, Ordering::Relaxed) < MAX_PER_PROCESS {
            // Never let the reporter panic inside the panic hook.
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let trace = Backtrace::force_capture().to_string();
                let report = Report::new(
                    component,
                    "panic",
                    &version,
                    &payload_text(info),
                    parse_backtrace(&trace),
                );
                let _ = spool.write(&report);
            }));
        }
        previous(info);
    }));
}

/// A frontend error (`window.onerror`, `unhandledrejection`): scrubbed here,
/// whatever the page sent. Returns whether a report was stored.
pub fn capture_frontend(
    spool: &Spool,
    version: &str,
    kind: &str,
    message: &str,
    stack: &str,
) -> bool {
    if !capture_enabled() || WRITTEN.fetch_add(1, Ordering::Relaxed) >= MAX_PER_PROCESS * 4 {
        return false;
    }
    let kind = match kind {
        "unhandledrejection" => "unhandledrejection",
        _ => "error",
    };
    let report = Report::new("ui", kind, version, message, parse_js_stack(stack));
    spool.write(&report).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The hook and the flag are process-wide; tests that use them take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn panic_is_spooled_only_with_capture_on() {
        let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        WRITTEN.store(0, Ordering::Relaxed);
        let original = panic::take_hook();
        install("app", "9.9.9", spool.clone());

        set_capture(false);
        let _ = panic::catch_unwind(|| panic!("off: user@host.example.com"));
        assert!(spool.list().is_empty());

        set_capture(true);
        let _ = panic::catch_unwind(|| panic!("on: user@host.example.com at /home/alice/x"));
        set_capture(false);
        panic::set_hook(original);

        let list = spool.list();
        assert_eq!(list.len(), 1);
        let r = &list[0].report;
        assert_eq!(r.kind, "panic");
        assert_eq!(r.version, "9.9.9");
        assert_eq!(r.message, "on: <email> at <path>");
        assert!(!r.frames.is_empty());
        let all = serde_json::to_string(r).unwrap();
        assert!(!all.contains("alice") && !all.contains("/home/"));
    }

    #[test]
    fn frontend_errors_are_scrubbed_and_gated() {
        let _guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        WRITTEN.store(0, Ordering::Relaxed);
        set_capture(false);
        assert!(!capture_frontend(&spool, "1", "error", "x", ""));
        set_capture(true);
        assert!(capture_frontend(
            &spool,
            "1",
            "bogus kind",
            "failed for root@10.0.0.1",
            "Error\n    at connect (tauri://localhost/assets/a.js:1:2)"
        ));
        set_capture(false);
        let r = &spool.list()[0].report;
        assert_eq!(r.component, "ui");
        assert_eq!(r.kind, "error");
        assert_eq!(r.message, "failed for <email>");
        assert_eq!(r.frames, ["connect"]);
    }
}
