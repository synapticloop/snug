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
    fn detail_of_drops_empty_parts_and_keeps_groups_separate() {
        assert_eq!(detail_of(&["a", "", "  ", "b"]), "a\n\nb");
        assert_eq!(detail_of(&[]), "");
        assert_eq!(detail_of(&["only"]), "only");
    }
}
