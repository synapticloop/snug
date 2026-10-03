//! AppKit dialogs for the macOS launcher runtime.
//!
//! This is the macOS half of the `jdk_install::ui` seam, and it exists to
//! put the *same localised strings* on screen that Windows shows. Every
//! piece of copy comes from [`crate::dialogs`], assembled with
//! [`crate::dialogs::fill`] against the `[jdk_install.*]` keys — nothing
//! here formats a string of its own, so a `--localization <tag>` bundle
//! translates the macOS dialogs exactly as it translates the Windows
//! ones.
//!
//! The widgets are real AppKit (`NSAlert`) rather than a port of the
//! hand-painted Win32 windows. Two reasons, and they are the same
//! reason: AppKit gives correct HiDPI, correct accessibility, correct
//! keyboard focus and correct system integration for free, and a
//! CoreGraphics re-implementation of `modal_window.rs` would have to
//! re-earn every one of those by hand. The dialog will not look like the
//! Windows one. It will look like a macOS dialog, which is the correct
//! answer on this platform.
//!
//! # Threading
//!
//! AppKit wants UI on the main thread, and `NSAlert::new` takes a
//! [`MainThreadMarker`] to enforce that. The launcher's `run()` executes
//! on the process main thread and the *download* is on a worker thread,
//! so the dialogs naturally land in the right place. When the marker is
//! absent — something re-entered the flow off the main thread — every
//! entry point degrades to the log rather than constructing a window
//! from a thread that must not.
//!
//! # Testing
//!
//! A real `NSAlert` blocks until a human answers, which no test can do.
//! [`set_test_response`] makes the dialogs answer on demand and
//! [`last_shown`] captures what *would* have been displayed, so the part
//! that carries the risk — that the right localised text reaches the
//! right alert, and that the answer is honoured — is testable without a
//! window ever appearing.

#![cfg(target_os = "macos")]

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSString;

use crate::dialogs;

// ---------------------------------------------------------------------------
//  Test seam
// ---------------------------------------------------------------------------

/// What an alert would have displayed. Captured so a test can assert on
/// the copy without a window appearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    pub style: &'static str,
    pub heading: String,
    pub detail: String,
    pub buttons: Vec<String>,
}

/// Sentinel meaning "no override is installed".
const NO_OVERRIDE: i32 = -1;

/// When set, [`show_alert`] returns this button index instead of running a
/// modal session. Test-only, and process-wide because `NSAlert` is
/// process-global anyway.
static RESPONSE_OVERRIDE: AtomicI32 = AtomicI32::new(NO_OVERRIDE);

static LAST_SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

/// Make every subsequent alert answer with `button_index` without
/// showing. `None` restores the real behaviour.

// ---------------------------------------------------------------------------
//  Progress
// ---------------------------------------------------------------------------

/// Adoptium's own name for this platform, as it appears in the progress
/// line. The localisation baseline used to hardcode "Windows x64" in a
/// *shared* key, which printed the wrong architecture on macOS.
fn adoptium_arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "macOS arm64",
        _ => "macOS x86_64",
    }
}

/// The live status line under the progress bar.
///
/// Split out and pure so the wording is testable without a window. It
/// mirrors the Win32 progress window's fields: a percent, the phase, and
/// — once bytes are actually moving — a throughput line. The rate is only
/// shown when we have two samples, because the first would divide by an
/// elapsed time of nearly zero and print something absurd.
fn progress_status(
    phase: i32,
    pct: u32,
    done: u64,
    total: u64,
    mib_s: Option<f64>,
    arch: &str,
) -> String {
    let d = dialogs::dialogs();
    let p = &d.jdk_install.progress;

    let phase_text = dialogs::fill(p.phase_label.as_str(), &[("arch", arch)]);
    let pct_text = dialogs::fill(p.pct_label.as_str(), &[("pct", &pct.to_string())]);

    let mut parts = vec![phase_text, pct_text];
    if let Some(rate) = mib_s {
        let done_mb = format!("{:.0}", done as f64 / 1_048_576.0);
        let total_mb = format!("{:.0}", total as f64 / 1_048_576.0);
        let speed = format!("{rate:.1}");
        parts.push(dialogs::fill(
            p.detail_with_size.as_str(),
            &[
                ("done_mb", done_mb.as_str()),
                ("total_mb", total_mb.as_str()),
                ("speed_mb_s", speed.as_str()),
            ],
        ));
    } else if total == 0 {
        parts.push(p.detail_no_size.clone());
    }
    let _ = phase;
    parts.join("\n")
}

/// Show download progress until the worker finishes or the user cancels.
///
/// An `NSAlert` with an accessory view rather than a hand-built
/// `NSWindow`. A deliberate trade: an alert gets a real window, correct
/// focus and a working close box for free, and needs no
/// `NSWindowDelegate` and no target/action, neither of which is pleasant
/// to express from Rust. What it does not get is a resizable window or
/// the mascot panel, and neither is worth the plumbing here.
///
/// The run loop is pumped by hand rather than through `runModal`, because
/// the progress has to be sampled *between* event-loop turns and
/// `runModal` offers no such seam. `objc2-app-kit` 0.3.2 binds no
/// `NSTimer`, so a timer block was not an option either.
///
/// Returns `true` if the user let it run to completion. Cancellation is
/// recorded on `shared` exactly as the Windows window records it, so the
/// caller's post-conditions match on both platforms.
pub(crate) fn progress(
    main: &str,
    shared: std::sync::Arc<crate::jdk_install::ProgressShared>,
) -> bool {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use objc2_app_kit::{
        NSLineBreakMode, NSProgressIndicator, NSProgressIndicatorStyle, NSStackView, NSTextField,
        NSUserInterfaceLayoutOrientation,
    };
    use objc2_foundation::{NSDate, NSRunLoop};

    let d = dialogs::dialogs();
    let p = &d.jdk_install.progress;
    let arch = adoptium_arch_label();

    let Some(mtm) = activate_app() else {
        // No main thread to put a window on. We must not return early:
        // the caller joins the worker next, and the download is live.
        return log_only_progress(&shared);
    };

    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    let heading = NSString::from_str(&p.heading);
    alert.setMessageText(&heading);
    let subtitle = NSString::from_str(&p.subtitle);
    alert.setInformativeText(&subtitle);
    let _ = main;

    // One button, and it cancels. The baseline's
    // `cancel_button_during_download` reads "Install" because that label
    // belongs to the Windows *prompt* window, where pressing it means "go
    // ahead". Here the download is already running, so the only useful
    // action is abort, and "Install" would be a lie.
    let cancel_label = NSString::from_str(&d.jdk_install.prompt.button_cancel);
    alert.addButtonWithTitle(&cancel_label);

    let bar = NSProgressIndicator::new(mtm);
    bar.setStyle(NSProgressIndicatorStyle::Bar);
    bar.setIndeterminate(false);
    bar.setMinValue(0.0);
    bar.setMaxValue(100.0);
    bar.setDoubleValue(0.0);
    // SAFETY: a progress indicator only needs the main thread for its own
    // bookkeeping, and `mtm` is proof we hold it - that is exactly what
    // `MainThreadMarker` exists to assert.
    unsafe { bar.startAnimation(None) };

    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    status.setLineBreakMode(NSLineBreakMode::ByWordWrapping);

    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.addArrangedSubview(&bar);
    stack.addArrangedSubview(&status);
    alert.setAccessoryView(Some(&stack));

    let window = alert.window();
    window.makeKeyAndOrderFront(None);

    let mut cancelled = false;
    let mut last: Option<(u64, Instant)> = None;

    while shared.done.load(Ordering::SeqCst) == 0 {
        let (done, total, phase, pct) = (
            shared.bytes.load(Ordering::SeqCst),
            shared.total_bytes.load(Ordering::SeqCst),
            shared.phase.load(Ordering::SeqCst),
            shared.pct.load(Ordering::SeqCst).min(100),
        );

        // A rate needs two samples; the first would divide by ~0.
        let now = Instant::now();
        let mib_s = last.map(|(prev_bytes, prev_at)| {
            let secs = now.duration_since(prev_at).as_secs_f64().max(0.001);
            (done.saturating_sub(prev_bytes) as f64 / secs) / 1_048_576.0
        });
        last = Some((done, now));

        bar.setDoubleValue(pct as f64);
        let text = progress_status(phase, pct, done, total, mib_s, arch);
        let ns = NSString::from_str(&text);
        status.setStringValue(&ns);

        // Closing the alert - close box, Escape or Cancel - is the
        // abort. `isVisible` is the cheapest honest signal and needs no
        // delegate.
        if !window.isVisible() {
            cancelled = true;
            break;
        }

        // Pump briefly, then look again. Without this the window would
        // not repaint and the bar would sit frozen.
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        NSRunLoop::currentRunLoop().runUntilDate(&limit);
    }

    window.close();

    if cancelled {
        // Identical bookkeeping to the Windows progress window: the
        // worker is still alive and `cancel` is what stops it. Without
        // also setting `done`, the caller would read a half-finished
        // download as a success.
        shared.cancel.store(true, Ordering::SeqCst);
        if shared.done.load(Ordering::SeqCst) == 0 {
            shared.done.store(3, Ordering::SeqCst);
        }
        eprintln!("snug: download cancelled");
        return false;
    }

    true
}

/// Progress reporting when there is no main thread to put a window on.
fn log_only_progress(shared: &std::sync::Arc<crate::jdk_install::ProgressShared>) -> bool {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let arch = adoptium_arch_label();
    let mut last: Option<(u64, Instant)> = None;
    let mut last_pct = 0u32;
    let mut last_phase = -1i32;

    while shared.done.load(Ordering::SeqCst) == 0 {
        let (done, total, phase, pct) = (
            shared.bytes.load(Ordering::SeqCst),
            shared.total_bytes.load(Ordering::SeqCst),
            shared.phase.load(Ordering::SeqCst),
            shared.pct.load(Ordering::SeqCst).min(100),
        );
        let now = Instant::now();
        let mib_s = last.map(|(prev_bytes, prev_at)| {
            let secs = now.duration_since(prev_at).as_secs_f64().max(0.001);
            (done.saturating_sub(prev_bytes) as f64 / secs) / 1_048_576.0
        });
        last = Some((done, now));
        // The same decision the log-only poller used to make inline, and
        // the one `jdk_install::should_report` exists to pin.
        if crate::jdk_install::should_report(phase, last_phase, pct, last_pct) {
            last_phase = phase;
            last_pct = pct;
            eprintln!(
                "{}",
                progress_status(phase, pct, done, total, mib_s, arch)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    true
}

#[cfg(test)]
pub(crate) fn set_test_response(button_index: Option<usize>) {
    RESPONSE_OVERRIDE.store(
        button_index.map_or(NO_OVERRIDE, |i| i as i32),
        Ordering::SeqCst,
    );
}

/// The last alert this process would have shown. `#[cfg(test)]` because
/// it exists only so a test can read it back.
#[cfg(test)]
pub(crate) fn last_shown() -> Option<Shown> {
    LAST_SHOWN.lock().ok().and_then(|g| g.clone())
}

/// The IDYES-equivalent the flow compares against. Defined in
/// `jdk_install`'s non-Windows `id` module; mirrored here so this file
/// does not have to reach into the flow's internals.
const YES: i32 = 6;

// ---------------------------------------------------------------------------
//  AppKit plumbing
// ---------------------------------------------------------------------------

/// Make this process a foreground GUI app and return the main-thread
/// marker, or `None` if we are not on the main thread.
///
/// A `.app` launched from Finder is already a regular application, but
/// one started from a terminal is not, and without this the alert can
/// open behind whatever the user was doing and never take keyboard
/// focus — which reads as "the app hung".
fn activate_app() -> Option<MainThreadMarker> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // `sharedApplication` leaves launch half-finished; `finishLaunching`
    // is what lets the alert take focus. Calling it twice is harmless,
    // and this runs before every alert.
    app.finishLaunching();
    Some(mtm)
}

/// Show a modal alert and return the index of the button the user chose.
///
/// Falls back to logging (and returning `fallback`) when AppKit is
/// unavailable or we are off the main thread, so a dialog can never be
/// the reason a launch dies.
fn show_alert(
    style: NSAlertStyle,
    style_name: &'static str,
    heading: &str,
    detail: &str,
    buttons: &[&str],
    fallback: usize,
) -> usize {
    let record = Shown {
        style: style_name,
        heading: heading.to_string(),
        detail: detail.to_string(),
        buttons: buttons.iter().map(|b| (*b).to_string()).collect(),
    };
    if let Ok(mut slot) = LAST_SHOWN.lock() {
        *slot = Some(record);
    }

    let override_index = RESPONSE_OVERRIDE.load(Ordering::SeqCst);
    if override_index != NO_OVERRIDE {
        return override_index.max(0) as usize;
    }

    let Some(mtm) = activate_app() else {
        eprintln!("{heading}\n{detail}");
        return fallback;
    };

    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(style);

    let heading_ns = NSString::from_str(heading);
    alert.setMessageText(&heading_ns);
    if !detail.is_empty() {
        let detail_ns = NSString::from_str(detail);
        alert.setInformativeText(&detail_ns);
    }
    for label in buttons {
        let label_ns = NSString::from_str(label);
        alert.addButtonWithTitle(&label_ns);
    }

    let response = alert.runModal();
    // AppKit numbers the buttons from 1000 (first), 1001 (second), ...
    let index = response - 1000;
    if index < 0 {
        // A sheet dismissed without a button (window closed). Treat it
        // as the fallback rather than inventing an answer.
        return fallback;
    }
    index as usize
}

/// Join the informational lines the Windows dialogs lay out separately.
///
/// `NSAlert` has exactly one detail field, where the Win32 windows have a
/// subheading, a body and an optional info box. The line breaks keep the
/// same visual grouping so the strings still read as authored.
fn detail_of(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---------------------------------------------------------------------------
//  The dialogs
// ---------------------------------------------------------------------------

/// Adoptium could not be reached. Returns `true` when the user asked for
/// the release page to be opened.
pub(crate) fn metadata_failed(min_java: u16, error_detail: &str) -> i32 {
    let d = dialogs::dialogs();
    let md = &d.jdk_install.metadata_failed;
    let major = min_java.to_string();

    // The placeholder is `{major}`, not `{min_java}` — the localisation
    // baseline is the contract here, so read the key off the same
    // template Windows fills. Passing the wrong key leaves `{major}` in
    // the user's face.
    let subheading = dialogs::fill(
        md.subheading.as_str(),
        &[("major", &major), ("error", error_detail)],
    );
    let content = dialogs::fill(
        md.content.as_str(),
        &[("major", &major), ("error", error_detail)],
    );

    // Button 0 is "Open in Browser", which is what the flow checks for.
    // The fallback is button 1, so a dialog we cannot show does not open
    // a browser in the user's face unasked.
    let chosen = show_alert(
        NSAlertStyle::Warning,
        "warning",
        &md.heading,
        &detail_of(&[
            &subheading,
            &content,
            &md.info_heading,
            &md.info_subtext,
            &md.info_subtext_2,
        ]),
        &[&md.button_open_browser, &md.button_cancel],
        1,
    );
    if chosen == 0 { YES } else { 0 }
}

/// A download attempt failed. `true` means "retry".
pub(crate) fn retry(attempt: u32, max_attempts: u32, version: &str, error: &str) -> bool {
    let d = dialogs::dialogs();
    let r = &d.jdk_install.retry;
    let attempt_str = attempt.to_string();
    let max_str = max_attempts.to_string();

    let subheading = dialogs::fill(
        r.subheading.as_str(),
        &[
            ("version", version),
            ("attempt", attempt_str.as_str()),
            ("max_attempts", max_str.as_str()),
        ],
    );
    let content = dialogs::fill(
        r.content.as_str(),
        &[
            ("version", version),
            ("attempt", attempt_str.as_str()),
            ("max_attempts", max_str.as_str()),
            ("error", error),
        ],
    );

    // Button 0 is Retry. The fallback is Cancel: if we cannot ask, we
    // must not silently loop a few hundred megabytes of downloads.
    let chosen = show_alert(
        NSAlertStyle::Warning,
        "warning",
        &r.heading,
        &detail_of(&[&subheading, &content]),
        &[&r.button_retry, &r.button_cancel],
        1,
    );
    chosen == 0
}

/// Terminal failure, after the attempts are exhausted. No answer needed.
pub(crate) fn failure(title: &str, content: &str) {
    let d = dialogs::dialogs();
    let f = &d.jdk_install.failure;
    show_alert(
        NSAlertStyle::Critical,
        "critical",
        title,
        &detail_of(&[&f.heading, content]),
        &[&f.button_label],
        0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam is process-global — it has to be, because `NSAlert` is —
    /// so the tests in this module are not independent. Every one of them
    /// holds this for its duration, or two would overwrite each other's
    /// recorded alert and answer.
    static SERIALISED: Mutex<()> = Mutex::new(());

    /// Run `f` with dialogs auto-answering `response`, and return what
    /// would have been displayed.
    ///
    /// The call inside `f` is the point: installing an override does not
    /// record anything, only showing an alert does.
    fn shown_by(response: usize, f: impl FnOnce()) -> Shown {
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(Some(response));
        f();
        let shown = last_shown().expect("an alert should have been recorded");
        set_test_response(None);
        shown
    }

    #[test]
    fn retry_shows_the_localised_copy_and_honours_the_answer() {
        let yes = shown_by(0, || {
            assert!(retry(2, 3, "21.0.12", "connection reset"));
        });
        assert_eq!(yes.style, "warning");
        assert!(!yes.heading.is_empty());
        // The substituted values must be in the body, not the raw
        // placeholders: that is the whole point of `dialogs::fill`.
        assert!(
            yes.detail.contains("21.0.12"),
            "version missing from: {}",
            yes.detail
        );
        assert!(
            yes.detail.contains("Attempt 2 of 3"),
            "attempt/max missing from: {}",
            yes.detail
        );
        assert!(
            yes.detail.contains("connection reset"),
            "error missing from: {}",
            yes.detail
        );
        // Two buttons, and the first is the affirmative one.
        assert_eq!(yes.buttons.len(), 2);

        // The answer has to drive the return value in both directions: a
        // "retry" is Retry-first, and a cancel is not. Silently always
        // retrying would be the bug this test exists to prevent.
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(Some(0));
        assert!(retry(2, 3, "21.0.12", "connection reset"));
        set_test_response(Some(1));
        assert!(!retry(2, 3, "21.0.12", "connection reset"));
        set_test_response(None);
    }

    #[test]
    fn metadata_failed_offers_the_browser_and_defaults_to_not_opening_it() {
        let shown = shown_by(0, || {
            assert_eq!(metadata_failed(23, "DNS failure"), YES);
        });
        assert_eq!(shown.style, "warning");
        assert!(
            shown.detail.contains("Java 23"),
            "major version missing: {}",
            shown.detail
        );
        assert!(
            !shown.detail.contains("{major}"),
            "an unsubstituted placeholder reached the user: {}",
            shown.detail
        );
        assert!(
            shown.detail.contains("DNS failure"),
            "error detail missing: {}",
            shown.detail
        );
        assert_eq!(shown.buttons.len(), 2);

        // Button 0 (Open in Browser) is the only affirmative answer, and
        // anything else must read as "no" — the flow compares this
        // against IDYES, so a wrong mapping silently opens a browser.
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(Some(1));
        assert_eq!(metadata_failed(23, "DNS failure"), 0);
        set_test_response(None);
    }

    #[test]
    fn failure_is_critical_and_needs_no_answer() {
        let shown = shown_by(0, || {
            failure("Install failed", "disk full");
        });
        assert_eq!(shown.style, "critical");
        assert_eq!(shown.buttons.len(), 1);
    }

    #[test]
    fn progress_status_names_this_platform_not_windows() {
        // The baseline shipped `Downloading runtime (Windows x64)` as a
        // *shared* key, so a macOS download announced itself as a Windows
        // one. The key now takes `{arch}` and each platform fills it in.
        let text = progress_status(0, 42, 0, 0, None, adoptium_arch_label());
        assert!(
            text.contains(&adoptium_arch_label()),
            "arch missing from: {text}"
        );
        assert!(!text.contains("Windows"), "leaked the Windows arch: {text}");
        assert!(text.contains("42%"), "percent missing from: {text}");
    }

    #[test]
    fn progress_status_shows_a_rate_only_once_it_is_meaningful() {
        // First sample: no rate yet. Dividing by a near-zero elapsed time
        // would print something absurd, so the line is omitted instead.
        let no_rate = progress_status(0, 5, 5 << 20, 100 << 20, None, "macOS arm64");
        assert!(!no_rate.contains("MB/s"), "invented a rate: {no_rate}");

        let with_rate = progress_status(
            0,
            20,
            20 << 20,
            100 << 20,
            Some(3.7),
            "macOS arm64",
        );
        assert!(with_rate.contains("MB/s"), "rate missing: {with_rate}");
        // 20 MiB of 100 MiB at 3.7 MB/s, as the Win32 window shows it.
        assert!(with_rate.contains("20"), "done_mb missing: {with_rate}");
        assert!(with_rate.contains("100"), "total_mb missing: {with_rate}");
    }

    #[test]
    fn progress_status_never_leaves_a_raw_placeholder() {
        // Same class of bug as the `{major}` one: a wrong substitution key
        // puts `{arch}` in the user's face rather than failing.
        let text = progress_status(0, 7, 7 << 20, 100 << 20, Some(1.0), "macOS x86_64");
        assert!(!text.contains('{'), "unsubstituted placeholder in: {text}");
        assert!(!text.contains("%}"), "unsubstituted placeholder in: {text}");
    }

    #[test]
    fn detail_of_drops_empty_parts_and_keeps_groups_separate() {
        assert_eq!(detail_of(&["a", "", "  ", "b"]), "a\n\nb");
        assert_eq!(detail_of(&[]), "");
        assert_eq!(detail_of(&["only"]), "only");
    }
}
