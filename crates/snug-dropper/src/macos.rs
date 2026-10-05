//! The macOS half of `Build with Snug`.
//!
//! Everything here is AppKit. The *decisions* live in [`crate::decide`]
//! and the child-process plumbing in [`crate::build`], both of which are
//! platform-neutral and tested without a Mac — this module only receives
//! the dropped paths and turns the outcome into windows.
//!
//! # Why this is an `.app` and not a bare executable
//!
//! On Windows, dropping a file on an EXE makes Explorer launch it with
//! the dropped paths appended to `argv` — that *is* the whole
//! drag-and-drop API, which is why `decide` is a pure function over a
//! slice of paths.
//!
//! macOS has no such thing. A bare Mach-O cannot be a drop target at
//! all: Finder only hands a bundle what its `CFBundleDocumentTypes`
//! claims, and delivers it through `application:openFile:` on an
//! `NSApplicationDelegate`. So the dropper ships as
//! `Build with Snug.app`, and this module implements the delegate.
//!
//! That is also why the artefact is a bundle rather than a bare binary
//! with an `__icns` section: a bundle is the only form that can *receive*
//! a drop, and receiving the drop is the entire product.
//!
//! # The `application:openFile:` ordering
//!
//! The first version drained its latch in `applicationDidFinishLaunching:`
//! and terminated. That never worked, and the reason is worth recording
//! because it is not in the delegate protocol's docs: Cocoa defines
//! `applicationDidFinishLaunching:` as firing once
//! `-[NSApplication finishLaunching]` has completed **"but no event
//! dispatching has begun"**. The drop is an `odoc` Apple Event, and
//! Apple Events are dispatched *by the run loop* — so every drop arrives
//! strictly *after* that method returns. Draining there always found an
//! empty list, opened a Terminal as if the user had double-clicked, and
//! terminated the app before the drop was ever delivered.
//!
//! So the two cases are separated in time instead. Launch schedules a
//! short settle callback; a drop re-arms it. Whichever fires first
//! drains the latch, and the empty case is the double-click:
//!
//! ```text
//! launch ──> schedule settle ──┐
//!                              │ (nothing dropped)
//! openFile: ──> latch, re-arm ─┴──> settle ──> drain ──> act ──> terminate
//! ```
//!
//! Re-arming rather than acting on the first drop is what keeps a
//! multi-file drop intact: Finder sends one Apple Event per file, and
//! `decide` has to see the *whole* set to refuse it with "only one jar".
//!
//! A `performSelector:withObject:afterDelay:` is used because
//! `objc2-app-kit` binds no `NSTimer`, and because rescheduling the same
//! selector cancels the pending request — which is exactly the coalescing
//! behaviour wanted here, with no timer object to keep track of.
//!
//! # Dialogs
//!
//! Plain `NSAlert`s, which is a deliberate difference from the launcher's
//! AppKit code. The launcher abandoned `NSAlert` because it gated a
//! worker thread on a human pressing "Install", and a modal that only a
//! click can dismiss is a deadlock waiting for a user who never arrives.
//! Nothing here does that: every alert is a *report* of a build that has
//! already finished, or a confirm that is immediately answered. There is
//! no worker waiting on a button, so the modal loop is exactly right.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{LazyLock, Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, extern_methods};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate,
};
use objc2_foundation::{
    NSObject, NSObjectNSDelayedPerforming, NSObjectProtocol, NSRunLoop, NSString,
};

use crate::build::{self, LOG_FILE_NAME, SNUG_PROGRAM};
use crate::decide::{Mode, Reject, decide};

/// Dropped paths collected before the run loop was live.
///
/// `OnceLock` because it is written from the delegate, which AppKit only
/// ever calls on the main thread, and read from
/// `applicationDidFinishLaunching:`, which is also main-thread — so no
/// lock is actually needed. It is `Mutex` anyway purely to satisfy
/// `Sync` on the static; the mutex is never contended.
static PENDING: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

fn pending() -> &'static Mutex<Vec<PathBuf>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record a drop without acting on it. See the module docs.
fn record(path: &str) {
    pending().lock().expect("pending-drop list").push(PathBuf::from(path));
}

/// Take everything recorded so far.
fn take_pending() -> Vec<PathBuf> {
    std::mem::take(&mut *pending().lock().expect("pending-drop list"))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct DropDelegate;

    unsafe impl NSObjectProtocol for DropDelegate {}

    // The selector names are Apple's, not ours: `application:openFile:` is
    // the name AppKit looks up in the class's method table, so renaming
    // it to snake case would stop the drop arriving at all.
    #[allow(non_snake_case)]
    unsafe impl NSApplicationDelegate for DropDelegate {
        /// One dropped path. Latched, never acted on — see module docs.
        #[unsafe(method(application:openFile:))]
        fn application_openFile(&self, _sender: &NSApplication, filename: &NSString) -> bool {
            record(&filename.to_string());
            // Re-arm the settle callback: a multi-file drop is several
            // Apple Events in a row, and rescheduling the same selector
            // cancels the pending one, so the callback only fires once the
            // last one has landed.
            self.arm_settle(SETTLE_AFTER_DROP);
            true
        }

        /// No event dispatching has begun yet, so no drop can have arrived.
        /// Arm the callback that will conclude "this was a double-click"
        /// if nothing else turns up.
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn applicationDidFinishLaunching(&self, _notification: &AnyObject) {
            self.arm_settle(SETTLE_AFTER_LAUNCH);
        }

        /// The settle timer fired: either the quiet after launch (a
        /// double-click) or the tail of a multi-file drop. Draining here
        /// rather than at either end is what makes the multi-file case
        /// arrive intact.
        #[unsafe(method(settle:))]
        fn settle(&self, _sender: &AnyObject) {
            let items = take_pending();
            finish(&items);
            // Nothing here leaves a window open and the work is done, so
            // the process is finished. Terminating rather than letting
            // `run()` return keeps the app out of the Dock with no window
            // to click — the same reason the Windows shim just exits.
            let mtm = MainThreadMarker::new().expect("AppKit called us on the main thread");
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    }
);

// `+new` is Objective-C's `alloc` + `init` in one call, which is why it is
// spelled as a class method here rather than done by hand: a
// `MainThreadOnly` class has no safe `init`, so there is nothing to call
// unless the macro declares this for us.
impl DropDelegate {
    extern_methods!(
        #[unsafe(method(new))]
        fn new(mtm: MainThreadMarker) -> Retained<Self>;
    );

    /// Schedule (or re-schedule) the settle callback.
    ///
    /// `performSelector:withObject:afterDelay:` on the main run loop is
    /// used rather than an `NSTimer` for two reasons: the crate binds no
    /// timer, and — the useful one — asking the run loop for the *same*
    /// selector again cancels the pending request. That is the whole
    /// coalescing mechanism, with no timer object to invalidate by hand
    /// and no way to leave a stale one firing after a build.
    fn arm_settle(&self, delay: f64) {
        // SAFETY: `SEL_SETTLE` is implemented above on this class, and
        // `AnyObject` is only ever an opaque argument the selector
        // ignores.
        unsafe {
            NSRunLoop::mainRunLoop().performSelector_withObject_afterDelay(
                *SEL_SETTLE,
                None,
                delay,
            );
        }
    }
}

/// Selector for the settle callback, registered by `define_class!`.
///
/// `LazyLock` rather than a `static`, because registering a selector is a
/// runtime call into the Objective-C runtime rather than a constant.
static SEL_SETTLE: LazyLock<Sel> = LazyLock::new(|| Sel::register(c"settle:"));

/// Grace period after launch with no drop, before concluding the user
/// double-clicked.
///
/// Long enough that a drop launched straight away is never misread as a
/// double-click, short enough that a bare double-click does not feel like
/// the app hung. The drop path re-arms a *shorter* delay, because at that
/// point the app is already up and only the remaining files of a
/// multi-file drop are outstanding.
const SETTLE_AFTER_LAUNCH: f64 = 0.35;

/// Tail delay after the most recent drop, before acting.
///
/// Only has to cover the gap between consecutive Apple Events of one drop.
const SETTLE_AFTER_DROP: f64 = 0.25;

/// Act on the collected items, then exit with the appropriate code.
///
/// Split out from the delegate so the decision table is reached through
/// exactly one path on macOS, as on Windows.
pub fn finish(items: &[PathBuf]) -> ExitCode {
    // `Contents/Resources` is where the bundler puts everything that is
    // not the executable. The dropper is a bundle, so "beside the
    // executable" means its own `Contents/MacOS/..`, and `snug` is
    // installed *beside the bundle* in the release folder.
    let home = match app_bundle_root() {
        Some(dir) => dir,
        None => {
            error("snug could not be found.\n\nKeep Build with Snug.app in the same folder as snug.");
            return ExitCode::FAILURE;
        }
    };

    match decide(items) {
        Mode::OpenTerminal => {
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Reject(Reject::TooManyItems) => {
            info("Only one jar is allowed.\n\nA Terminal window has been opened so you can build it yourself.");
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Reject(Reject::UnsupportedType) => {
            info("Only .jar files and folders can be dropped on Build with Snug.\n\nA Terminal window has been opened so you can build it yourself.");
            open_terminal(&home);
            ExitCode::SUCCESS
        }
        Mode::Build { input } => build_it(&home, input),
    }
}

/// macOS entry point.
pub fn run() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        // AppKit requires the main thread and there is no safe way to
        // build one of these windows anywhere else. `execve`'d from a
        // terminal this is still the main thread, so this is a formality
        // in practice; saying so beats constructing UI from a worker.
        eprintln!("Build with Snug: no main thread; cannot show a window.");
        return ExitCode::FAILURE;
    };

    // `NSApp::run()` installs its own autorelease pool and manages its
    // lifetime from here, so the only thing that needed wrapping was the
    // setup above — and `objc2` has no pool to borrow for it. Creating
    // the shared application is what AppKit uses as its cue that a GUI
    // session exists at all.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate: Retained<DropDelegate> = DropDelegate::new(mtm);
    // A defined class has no blanket `&AnyObject -> &ProtocolObject<_>`
    // conversion, so the protocol view is built explicitly. Same shape as
    // the launcher's window delegates.
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    // Unreachable in practice: `finish` terminates the app. Kept so the
    // function has a value if that ever changes.
    ExitCode::SUCCESS
}

/// The folder the bundle sits in — where `snug` is looked for and where
/// the log is written.
///
/// Walked out of `Contents/MacOS/<name>` rather than read from a
/// constant, so the bundle can be renamed in packaging without the path
/// going stale. Two levels up from the executable is the `.app`, and one
/// more is the folder containing it.
fn app_bundle_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // <release>/Build with Snug.app/Contents/MacOS/Build with Snug
    let bundle = exe.parent()?.parent()?.parent()?;
    Some(bundle.to_path_buf())
}

fn build_it(home: &Path, input: PathBuf) -> ExitCode {
    let snug = home.join(SNUG_PROGRAM);
    if !snug.is_file() {
        error("snug could not be found.\n\nKeep Build with Snug.app in the same folder as snug.");
        return ExitCode::FAILURE;
    }

    let invocation = build::invocation(&snug, &input);
    let log = home.join(LOG_FILE_NAME);

    if invocation.output.exists() && !confirm_overwrite(&invocation.output) {
        return ExitCode::SUCCESS;
    }

    // Deliberately *not* a worker thread. The build is a child process
    // and the alert is a report, not a gate, so the only thing a
    // background thread would buy is an app that looks hung for the
    // duration of a large packaging run. The launcher needed the other
    // trade because its dialog gated a *download* on a click.
    match build::run(&invocation, &log) {
        build::Outcome::Built { output } => {
            info(&format!(
                "Your application was created successfully.\n\n{}",
                output.display()
            ));
            ExitCode::SUCCESS
        }
        build::Outcome::Failed { reason } => {
            // No terminal fallback here by design: this dialog and the
            // log are the only recourse, so both have to carry the whole
            // story.
            error(&format!(
                "The build failed.\n\n{reason}\n\nDetails: {}",
                log.display()
            ));
            ExitCode::FAILURE
        }
    }
}

/// Ask before replacing an application from an earlier build.
///
/// Same reasoning as the Windows version: every build writes the same
/// fixed name, so a rebuild silently replaces the previous one, and for a
/// beginner "nothing happened" is indistinguishable from a successful
/// rebuild.
fn confirm_overwrite(output: &Path) -> bool {
    let mtm = MainThreadMarker::new().expect("dialogs are main-thread only");
    let file = output
        .file_name()
        .map_or_else(|| output.display().to_string(), |n| n.to_string_lossy().into());
    let parent = output.parent().unwrap_or(Path::new("."));
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("{file} already exists")));
    alert.setInformativeText(&NSString::from_str(&format!(
        "It is in {}.\n\nReplace it?",
        parent.display()
    )));
    alert.addButtonWithTitle(&NSString::from_str("Replace"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}

fn info(body: &str) {
    alert("Your application is ready", body);
}

fn error(body: &str) {
    alert("Something went wrong", body);
}

/// One alert with an OK button.
fn alert(title: &str, body: &str) {
    let mtm = MainThreadMarker::new().expect("dialogs are main-thread only");
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    // `NSAlert` numbers its buttons from 1000, so the return value of a
    // single-button alert carries no information worth reading. What
    // matters is that the modal loop runs until the click, which is what
    // keeps the process alive long enough to show it.
    let _ = alert.runModal();
}

/// Open a Terminal window in `dir` as the hand-off for every case this
/// starter cannot do itself.
///
/// `open -a Terminal <dir>` rather than spawning a shell: a bare `sh`
/// window would show a prompt in an unknown directory with no hint that
/// `snug` is the thing to type. A failure here is not worth a second
/// dialog on top of whatever the user already saw — they are being handed
/// the terminal as a convenience, and if the OS will not give them one
/// there is nothing useful to say about it.
fn open_terminal(dir: &Path) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-a")
        .arg("Terminal")
        .arg(dir)
        .spawn();
}
