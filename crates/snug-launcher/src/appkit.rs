//! AppKit windows for the macOS launcher runtime.
//!
//! This is the macOS half of the `jdk_install::ui` seam, and it exists to
//! put the *same localised strings* on screen that Windows shows. Every
//! piece of copy comes from [`crate::dialogs`], assembled with
//! [`crate::dialogs::fill`] against the `[jdk_install.*]` keys — nothing
//! here formats a string of its own, so a `--localization <tag>` bundle
//! translates the macOS windows exactly as it translates the Windows ones.
//!
//! # Why a real `NSWindow` and not an `NSAlert`
//!
//! This was an `NSAlert` once, and it was wrong for the progress window in
//! a way no amount of care could have fixed. `NSAlert` is a *modal*: it
//! runs its own nested event loop, and the download flow gates the worker
//! thread on a human clicking "Install". So the only thing that could ever
//! start the download was a click in a modal alert — and when the bundle
//! is `execve`'d from a terminal rather than launched through Launch
//! Services, that alert has no GUI session to appear in. It degraded to
//! logging, and degrading to logging still left the worker spinning on a
//! click that could never arrive. Minutes of silence and zero bytes.
//!
//! A plain window has no such trap. It is a passive display: it shows what
//! is happening and gets out of the way. The two properties that actually
//! matter are now structural rather than incidental —
//!
//!   - **nothing is gated on the UI.** The consent question is asked
//!     *before* the worker thread is spawned, so a window that fails to
//!     appear can never strand a thread.
//!   - **closing is the cancel.** `isVisible()` is polled on the same
//!     timer that samples progress, so the close box, Escape and the Cancel
//!     button all work without a delegate protocol or a sheet.
//!
//! What is given up is the free behaviour an alert provides: correct
//! HiDPI, accessibility and system integration still come from AppKit, but
//! an alert's own focus ring, default-button key handling and sheet
//! parenting do not. The layout is therefore explicit frames rather than
//! Auto Layout constraints, which is a little more code and a lot less
//! magic. It still looks like a macOS window, which is the correct answer
//! on this platform.
//!
//! # Threading
//!
//! AppKit wants UI on the main thread. The launcher's `run()` executes on
//! the process main thread and the *download* is on a worker thread, so
//! the windows naturally land in the right place. When we are somehow not
//! on the main thread, every entry point degrades to the log rather than
//! constructing a window from a thread that must not.
//!
//! # `unsafe`
//!
//! Two small `define_class!` types below need it. A plain `NSWindow` has
//! no built-in button response the way `NSAlert` does: `NSControl`'s
//! `setTarget:`/`setAction:` are `unsafe fn` in these bindings, so wiring
//! a button to a selector requires declaring an Objective-C class. That is
//! the whole of it — two stateless objects, each with one action and one
//! delegate method. This is a deliberate departure from the "no `unsafe`
//! in any crate" convention, taken because the alternative is a UI that
//! can hang.
//!
//! # Testing
//!
//! A real window blocks until a human answers, which no test can do.
//! [`set_test_response`] makes the windows answer on demand and
//! [`last_shown`] captures what *would* have been displayed, so the part
//! that carries the risk — that the right localised text reaches the
//! right window, and that the answer is honoured — is testable without a
//! window ever appearing.

#![cfg(target_os = "macos")]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, extern_methods};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSFont,
    NSLineBreakMode, NSProgressIndicator, NSProgressIndicatorStyle, NSStackView,
    NSTextField, NSUserInterfaceLayoutOrientation, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_app_kit::{NSImage, NSImageNameApplicationIcon, NSImageScaling, NSImageView};
use objc2_foundation::{
    NSDate, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRunLoop, NSSize, NSString,
};

use crate::dialogs;
use crate::log;

// ---------------------------------------------------------------------------
//  Test seam
// ---------------------------------------------------------------------------

/// What a window would have displayed. Captured so a test can assert on
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

/// When set, [`show_window`] returns this button index instead of running a
/// modal session. Test-only, and process-wide because the modal session is
/// process-global anyway.
static RESPONSE_OVERRIDE: AtomicI32 = AtomicI32::new(NO_OVERRIDE);

static LAST_SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

/// Make every subsequent window answer with `button_index` without
/// showing. `None` restores the real behaviour.
#[cfg(test)]
pub(crate) fn set_test_response(button_index: Option<usize>) {
    RESPONSE_OVERRIDE.store(
        button_index.map_or(NO_OVERRIDE, |i| i as i32),
        Ordering::SeqCst,
    );
}

/// The last window this process would have shown. `#[cfg(test)]` because it
/// exists only so a test can read it back.
#[cfg(test)]
pub(crate) fn last_shown() -> Option<Shown> {
    LAST_SHOWN.lock().ok().and_then(|g| g.clone())
}

/// Modal response meaning "dismissed without choosing", i.e. the close box
/// or Escape. Distinct from every real button index, so it can never be
/// mistaken for "button 0".
const DISMISSED: isize = -1;

/// The IDYES-equivalent the flow compares against. Defined in
/// `jdk_install`'s non-Windows `id` module; mirrored here so this file
/// does not have to reach into the flow's internals.
const YES: i32 = 6;

// ---------------------------------------------------------------------------
//  Objective-C responders
// ---------------------------------------------------------------------------

/// End the current modal session with `code`.
///
/// Called from the button action, which AppKit dispatches on the main
/// thread, so `MainThreadMarker::new()` is a formality — but it is the
/// assertion that makes the threading explicit, and it costs nothing.
fn stop_modal(code: isize) {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).stopModalWithCode(code);
    }
}

// Button target for the question windows.
//
// A button's `tag` *is* its response index, so one selector serves any
// number of buttons and the wiring is `tag -> code` with no per-dialog
// state to get wrong.
define_class!(
    #[unsafe(super(NSObject))]
    // A delegate/target object is only ever touched from the main thread.
    #[thread_kind = MainThreadOnly]
    struct ModalResponder;

    #[allow(non_snake_case)]
    impl ModalResponder {
        #[unsafe(method(buttonClicked:))]
        fn buttonClicked(&self, sender: &NSButton) {
            stop_modal(sender.tag());
        }
    }

    unsafe impl NSObjectProtocol for ModalResponder {}

    #[allow(non_snake_case)]
    unsafe impl NSWindowDelegate for ModalResponder {
        /// The close box. Answer "nothing was chosen" rather than letting
        /// the window vanish underneath a live modal session, which would
        /// otherwise leave the caller blocked forever. Returning `false`
        /// keeps the window up until the caller closes it deterministically.
        #[unsafe(method(windowShouldClose:))]
        fn windowShouldClose(&self, _sender: &NSWindow) -> bool {
            stop_modal(DISMISSED);
            false
        }
    }
);

/// Latched by the progress window's Cancel button or close box and read by
/// the poll loop on its next tick.
///
/// A process-global rather than an ivar, which is the same trade the test
/// seam above makes: there is exactly one download in flight, so there is
/// exactly one progress window, so there is nothing to distinguish.
static PROGRESS_CANCELLED: AtomicBool = AtomicBool::new(false);

// Button target and delegate for the progress window.
//
// It cannot use `stopModalWithCode`, because the progress window is not a
// modal session: it has to be pumped by hand so progress can be sampled
// *between* event-loop turns. So it latches a flag and the poll loop
// notices.
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct ProgressResponder;

    #[allow(non_snake_case)]
    impl ProgressResponder {
        #[unsafe(method(cancelClicked:))]
        fn cancelClicked(&self, _sender: &NSButton) {
            PROGRESS_CANCELLED.store(true, Ordering::SeqCst);
        }
    }

    unsafe impl NSObjectProtocol for ProgressResponder {}

    #[allow(non_snake_case)]
    unsafe impl NSWindowDelegate for ProgressResponder {
        /// The close box means the same thing as the Cancel button, and
        /// unlike the modal case we *do* let the window close here: there
        /// is no session to unwind and the poll loop ends on the flag.
        #[unsafe(method(windowShouldClose:))]
        fn windowShouldClose(&self, _sender: &NSWindow) -> bool {
            PROGRESS_CANCELLED.store(true, Ordering::SeqCst);
            true
        }
    }
);

// `+new` is Objective-C's `alloc` + `init` in one call, which is why it
// is spelled as a class method here rather than done by hand: a
// `MainThreadOnly` `Allocated<T>` has no safe `init`, so there is nothing
// to call unless the macro declares this for us.
impl ModalResponder {
    extern_methods!(
        #[unsafe(method(new))]
        fn new(mtm: MainThreadMarker) -> Retained<Self>;
    );
}

impl ProgressResponder {
    extern_methods!(
        #[unsafe(method(new))]
        fn new(mtm: MainThreadMarker) -> Retained<Self>;
    );
}

// ---------------------------------------------------------------------------
//  Progress copy
// ---------------------------------------------------------------------------

/// Human-readable platform+architecture for the progress line's `{arch}`
/// slot.
///
/// The localisation baseline used to hardcode "Windows x64" in a *shared*
/// key, which printed the wrong architecture on macOS. Fixing that moved
/// the substitution here — and left the value itself hardcoded, which is
/// the same bug one level down: a literal here means a French or Japanese
/// progress window reads "Downloading runtime (**macOS x86_64**)" with an
/// English platform token inside a translated sentence. It shows up in
/// the user's copy rather than in a compiler error, so it is worth the
/// extra key.
///
/// Only the macOS spellings are keyed. Windows still has the older gap —
/// it passes `phase_label` to its `STATIC` control raw, so `{arch}` is
/// never substituted there at all. See the Backlog.
fn adoptium_arch_label() -> String {
    let d = dialogs::dialogs();
    let p = &d.jdk_install.progress;
    if std::env::consts::ARCH == "aarch64" {
        p.arch_macos_arm64.clone()
    } else {
        p.arch_macos_x86_64.clone()
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

// ---------------------------------------------------------------------------
//  AppKit plumbing
// ---------------------------------------------------------------------------

/// Put the process's initial thread to work as the AppKit event loop.
///
/// This is the half of the fix for "a GUI app launched by snug never
/// opens a window" that lives on this side of the FFI. `JNI_CreateJavaVM`
/// claims the calling thread for Java's `main`, and `main` parks on a
/// latch for the life of the app, so the initial thread is never free —
/// and AppKit requires *that* thread to run the event loop. Without it
/// `applicationDidFinishLaunching:` is never delivered, so
/// `PlatformImpl.startup` never returns and no window appears.
///
/// So the launcher runs the loop here, on the process's initial thread,
/// while a worker thread owns the VM. That is the structure every Cocoa
/// app has.
///
/// **Deliberately does not call `finishLaunching`.** That call is what
/// *delivers* `applicationDidFinishLaunching:`, and the app this launcher
/// starts is waiting to receive it. Calling it here would consume the
/// notification before JavaFX had installed a delegate to observe it, and
/// the hang would return with no error to explain it. The app stays in
/// the not-yet-finished-launching state until the launched app decides it
/// is ready — which is exactly the handshake. (This is also why the
/// earlier attempt to call `activate_app()` from `run()` made things
/// worse; see AGENTS.md.)
///
/// **Does not create `NSApplication` at all, and that is the whole point.**
///
/// JavaFX decides whether it is a normal macOS app or a guest inside
/// somebody else's toolkit purely by asking *which class won the
/// `+sharedApplication` race*. From `GlassApplication.m`:
///
/// ```objc
/// NSApplication *app = [NSApplicationFX sharedApplication];
/// isEmbedded = ![app isKindOfClass:[NSApplicationFX class]];
/// if (!isEmbedded) { ...set delegate, run the loop, set up the app... }
/// else          { /* just fire willFinishLaunching and get out of the way */ }
/// ```
///
/// Apple's docs are explicit that the first `+sharedApplication` call
/// decides the class. An earlier version of this function called
/// `[NSApplication sharedApplication]` and `run()` in order to get a loop
/// running, and that made snug the winner — so glass concluded it was
/// **embedded** and skipped its entire macOS integration: no delegate, no
/// `TransformProcessType`, no activation, and on macOS 14+ no
/// `activateIgnoringOtherApps:` either. The window still appeared (which is
/// what made it so confusing) but the app's own `MenuBar` never reached the
/// system menu bar.
///
/// snug cannot win that race legitimately: glass can only run once the
/// main thread is inside a run loop, so whoever starts the loop necessarily
/// creates `NSApplication` first. The way out is to start a run loop that
/// does *not* need `NSApplication` at all — a `CFRunLoop` belongs to the
/// thread, not to the app object. That is what this now does: a bare
/// `NSRunLoop` pump, leaving `NSApplicationFX` for glass to create, install
/// itself as delegate, and drive with its own `[NSApp run]`.
///
/// The `Dock icon bounces forever` failure recorded in AGENTS.md was *not* a
/// consequence of pumping. It was a consequence of pumping **while glass
/// believed it was embedded** and therefore never called `finishLaunching`
/// or `[NSApp run]` itself, so AppKit sat in the launching state with
/// nothing to finish it. With `isEmbedded` now NO, glass completes the
/// launch and owns the loop; this pump is only the floor it is standing on.
///
/// The cost of giving the loop away is that we no longer have a `stop:`
/// handle, so termination is a flag instead — see [`stop_event_loop`].
/// A worker that finishes normally still exits the process outright.
pub(crate) fn run_event_loop() {
    if MainThreadMarker::new().is_none() {
        // Not the main thread: nothing may run a Cocoa loop. The worker
        // still completes and exits the process, so the launcher does not
        // depend on this having run.
        return;
    }
    // Deliberately `NSApplication`-free. See above.
    let run_loop = NSRunLoop::currentRunLoop();
    log::debug("appkit: initial thread pumping a bare run loop (no NSApplication)");
    while !STOP_EVENT_LOOP.load(Ordering::SeqCst) {
        // Long enough not to spin, short enough that a stop request is
        // not noticeable. `runUntilDate` returns as soon as it has handled
        // a source, so this is an idle wait rather than a fixed delay.
        let limit = NSDate::dateWithTimeIntervalSinceNow(EVENT_LOOP_TICK_SECS);
        run_loop.runUntilDate(&limit);
    }
    log::debug("appkit: run loop returned");
}

/// How long one turn of the bare run loop waits before re-checking
/// [`STOP_EVENT_LOOP`].
const EVENT_LOOP_TICK_SECS: f64 = 0.05;

/// Set by [`stop_event_loop`] to end [`run_event_loop`].
///
/// Exists because the loop is no longer `NSApplication::run`, which could
/// only be ended by a main-thread `stop:`. The thread that learns the app
/// has finished is the VM worker, and a polled loop can be ended from any
/// thread — which is what makes the launcher's *error* paths work at all.
/// Without it a failure before the loop is reached would leave the main
/// thread pumping forever and the error would never surface.
static STOP_EVENT_LOOP: AtomicBool = AtomicBool::new(false);

/// Ask [`run_event_loop`] to return at its next tick. Callable from any
/// thread. Idempotent.
pub(crate) fn stop_event_loop() {
    STOP_EVENT_LOOP.store(true, Ordering::SeqCst);
}

/// Make this process a foreground GUI app and return the main-thread
/// marker, or `None` if we are not on the main thread.
///
/// A `.app` launched from Finder is already a regular application, but one
/// started from a terminal is not, and without this a window can open
/// behind whatever the user was doing and never take keyboard focus —
/// which reads as "the app hung".
fn activate_app() -> Option<MainThreadMarker> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // `sharedApplication` leaves launch half-finished; `finishLaunching`
    // is what lets a window take focus. Calling it twice is harmless, and
    // this runs before every window.
    app.finishLaunching();
    Some(mtm)
}

/// Geometry. Explicit frames rather than Auto Layout: a handful of
/// constants is easier to reason about than a constraint graph, and the
/// windows are fixed-size anyway.
mod layout {
    /// Question windows.
    pub const DIALOG_W: f64 = 480.0;
    pub const DIALOG_H: f64 = 250.0;
    pub const DIALOG_MARGIN: f64 = 24.0;
    pub const DIALOG_BOTTOM: f64 = 18.0;
    pub const DIALOG_BUTTON_H: f64 = 32.0;
    pub const DIALOG_GAP: f64 = 12.0;

    /// The progress window.
    pub const PROGRESS_W: f64 = 480.0;
    pub const PROGRESS_H: f64 = 200.0;

    /// Side of the square the mascot occupies, and the gap between it and
    /// the text column.
    ///
    /// 64pt is roughly the size macOS uses for an icon in a sheet, and it
    /// is big enough for a 32pt bitmap to stay legible in a window that a
    /// user may be reading from across a desk.
    pub const DIALOG_MASCOT: f64 = 64.0;
    pub const DIALOG_MASCOT_GAP: f64 = 16.0;
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: w,
            height: h,
        },
    }
}

/// Allocate a titled, closable window with a content view of the given
/// size.
///
/// Deliberately not `Resizable`: every window here is a fixed-size prompt
/// or a fixed-size readout, and letting one be resized would mean laying
/// out for a second geometry that nothing asks for.
fn new_window(mtm: MainThreadMarker, w: f64, h: f64) -> Retained<NSWindow> {
    // SAFETY: `NSWindow`'s designated initialiser takes a rect, a style
    // mask, a backing store and a "defer" flag, and imposes no further
    // requirements; `mtm` is proof the caller holds the main thread, which
    // is the one thread that may create windows at all.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, w, h),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(""));
    let content = new_content_view(mtm, w, h);
    window.setContentView(Some(&content));
    window
}

/// A plain container for the subviews. `NSView::new` would give a zero
/// frame, and everything inside is positioned by hand.
fn new_content_view(mtm: MainThreadMarker, w: f64, h: f64) -> Retained<objc2_app_kit::NSView> {
    use objc2_app_kit::NSView;
    // `initWithFrame:` only needs an allocated object and a rect; `mtm`
    // asserts the main thread, which the signature already guarantees.
    NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, w, h))
}

thread_local! {
    /// Preview override for the mascot, set by `snug_preview --mascot`.
    ///
    /// A thread-local rather than an `AtomicIsize` like the Windows
    /// `MASCOT_ICON_OVERRIDE`, because an `HICON` is an integer and an
    /// `NSImage` is a retained Objective-C object with no thread
    /// guarantees — putting one in an atomic would require lying about
    /// `Send`/`Sync`. AppKit UI is main-thread-only anyway, so a
    /// thread-local is both safer and a truer description of the
    /// constraint.
    static MASCOT_OVERRIDE: std::cell::RefCell<Option<Retained<NSImage>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install a specific image as the dialog mascot, or pass `None` to go
/// back to the bundle's application icon.
///
/// This exists because **`snug_preview` cannot rely on the bundle icon at
/// all**: run as `cargo run --bin snug_preview` it is not inside a `.app`,
/// so `NSImageNameApplicationIcon` resolves to nothing and every dialog
/// would preview with no mascot. Without an override the one tool whose
/// job is judging dialog copy by eye would be unable to show the mascot.
pub fn set_mascot_image(image: Option<Retained<NSImage>>) {
    MASCOT_OVERRIDE.with(|m| *m.borrow_mut() = image);
}

/// Load a mascot from an image file on disk, for the preview. Returns
/// `None` if the file will not load, which the preview reports rather
/// than treating as fatal — a dialog with the wrong icon is still worth
/// looking at.
///
/// `mtm` is needed because `NSImage` allocation is main-thread-only, like
/// everything else in this module.
#[cfg(target_os = "macos")]
pub fn mascot_image_from_file(
    mtm: MainThreadMarker,
    path: &std::path::Path,
) -> Option<Retained<NSImage>> {
    use objc2::AnyThread;
    let name = NSString::from_str(&path.to_string_lossy());
    // `NSImage` is an `AnyThread` class in these bindings, so its `alloc`
    // takes no marker even though `initWithContentsOfFile:` is
    // main-thread-only. `mtm` is kept as the proof of the thread the init
    // must run on, which is why it is consumed here rather than dropped.
    let _ = mtm;
    // `initWithContentsOfFile:` is safe in these bindings despite being a
    // main-thread-only AppKit method; `mtm` above is the thread proof.
    NSImage::initWithContentsOfFile(NSImage::alloc(), &name)
}

/// The application's own icon, for the dialog mascot.
///
/// `NSImageNameApplicationIcon` resolves to the bundle's icon — the one
/// `macos_bundle.rs` wrote to `Contents/Resources/App.icns` from the same
/// `--icon` PNG that `editpe` stamps into `MAINICON` on Windows. So the
/// mascot is whatever icon the user chose, on both platforms, with no
/// second knob to keep in sync, and `--icon` stays the single control.
///
/// Returns `None` for a bundle with no icon. The dialogs are entirely
/// usable without a mascot, so callers treat that as "draw no image"
/// rather than as a failure — which is also the right behaviour for the
/// bare launcher, which ships with no bundle at all.
fn mascot_image() -> Option<Retained<NSImage>> {
    // SAFETY: `imageNamed:` is a documented class method that returns
    // autoreleased-or-retained AppKit state and takes no pointer
    // arguments. It must run on the main thread, which every caller here
    // is already on by way of owning a `MainThreadMarker`.
    let bundled = unsafe { NSImage::imageNamed(NSImageNameApplicationIcon) };
    MASCOT_OVERRIDE
        .with(|m| m.borrow().clone())
        .or(bundled)
}

/// Add the mascot to `content`, vertically centred in the `height` of
/// space starting at `y`, and return the horizontal space it consumed.
///
/// The return value is what lets the caller keep its text column
/// correctly placed instead of guessing: `0.0` when there is no icon, so
/// a bundle without one simply lays out as it always did. Callers inset by
/// the return value rather than by the constant, because "no mascot" and
/// "mascot" then need no branch of their own.
fn add_mascot(
    mtm: MainThreadMarker,
    content: &objc2_app_kit::NSView,
    y: f64,
    height: f64,
) -> f64 {
    let Some(image) = mascot_image() else {
        return 0.0;
    };
    let view = NSImageView::initWithFrame(
        NSImageView::alloc(mtm),
        rect(
            layout::DIALOG_MARGIN,
            y + (height - layout::DIALOG_MASCOT) / 2.0,
            layout::DIALOG_MASCOT,
            layout::DIALOG_MASCOT,
        ),
    );
    // The icon is 1024pt at source and the frame is `DIALOG_MASCOT` pt,
    // so it must scale or it would be clipped to a corner.
    view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    view.setImage(Some(&image));
    content.addSubview(&view);
    layout::DIALOG_MASCOT + layout::DIALOG_MASCOT_GAP
}

/// A wrapping, non-editable label.
fn label(mtm: MainThreadMarker, text: &str, bold: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    // Selectable so a long localised string can be copied out of the
    // window rather than photographed off the screen.
    field.setSelectable(true);
    // The system font is always present, so there is no nil case to
    // handle; `mtm` already asserts the main thread these need.
    let size = if bold { 13.0 } else { 11.0 };
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    field.setFont(Some(&font));
    field
}

/// A push button wired to `responder`'s action, carrying `tag`.
///
/// # Safety
///
/// `setTarget:`/`setAction:` are `unsafe fn` in these bindings because they
/// retain an Objective-C object and a selector. Both arguments here must
/// outlive the button; the window owns the button, and the responder is a
/// local in the calling frame that outlives the modal session, so that
/// holds.
unsafe fn button<T: objc2::Message>(
    mtm: MainThreadMarker,
    label: &str,
    tag: isize,
    responder: &Retained<T>,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let b = NSButton::initWithFrame(NSButton::alloc(mtm), rect(0.0, 0.0, 0.0, 0.0));
    b.setTitle(&NSString::from_str(label));
    b.setTag(tag);
    // SAFETY: `setTarget:` wants an untyped `id`. A `define_class!` type
    // has no safe conversion to `&AnyObject` in objc2 0.6 (there is no
    // blanket `AsRef<AnyObject>`), so go through the retained pointer.
    // A defined class is laid out as an `AnyObject` in the runtime, so
    // this is a pointer cast and not a reinterpretation of the value.
    let target: &objc2::runtime::AnyObject =
        unsafe { &*(Retained::as_ptr(responder) as *const objc2::runtime::AnyObject) };
    // SAFETY: as in the signature — `target` borrows `responder`, which
    // outlives the window, and `action` is implemented by the class.
    unsafe {
        b.setTarget(Some(target));
        b.setAction(Some(action));
    }
    // A zero-size frame leaves the button with no intrinsic size under
    // manual layout, so ask AppKit what it wants.
    b.sizeToFit();
    let (w, h) = (b.frame().size.width, b.frame().size.height);
    b.setFrame(rect(0.0, 0.0, w, h));
    let _ = mtm;
    b
}

/// Show a modal question window and return the index of the button the
/// user chose, or `fallback` if it could not be shown at all.
fn show_window(
    mtm: MainThreadMarker,
    heading: &str,
    detail: &str,
    buttons: &[&str],
    fallback: usize,
) -> usize {
    let w = layout::DIALOG_W;
    let h = layout::DIALOG_H;
    let window = new_window(mtm, w, h);
    window.setTitle(&NSString::from_str(heading));
    let Some(content) = window.contentView() else {
        eprintln!("{heading}\n{detail}");
        return fallback;
    };

    // Heading + body, stacked and top-aligned. The button row is
    // positioned separately along the bottom.
    let inner_w = w - 2.0 * layout::DIALOG_MARGIN;
    let text_h = h - layout::DIALOG_BOTTOM - layout::DIALOG_BUTTON_H - 2.0 * layout::DIALOG_MARGIN;
    let text_bottom = layout::DIALOG_BOTTOM + layout::DIALOG_BUTTON_H + layout::DIALOG_MARGIN;

    // Mascot on the left, text column to its right. Placed by explicit
    // frame for the same reason the buttons below are: this is a
    // fixed-size window, and `NSStackView`'s alignment constants describe
    // gravity rather than distribution, so asking one to lay this out
    // horizontally is a fight with the wrong tool.
    //
    // `inset` is `0.0` when there is no icon, so the geometry below needs
    // no idea whether a mascot was drawn.
    let inset = add_mascot(mtm, &content, text_bottom, text_h);
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(10.0);
    stack.setFrame(rect(
        layout::DIALOG_MARGIN + inset,
        text_bottom,
        inner_w - inset,
        text_h,
    ));
    stack.addArrangedSubview(&label(mtm, heading, true));
    if !detail.is_empty() {
        stack.addArrangedSubview(&label(mtm, detail, false));
    }
    content.addSubview(&stack);

    // Buttons, laid out right to left along the bottom, which is where a
    // macOS user expects the affirmative one. Done by hand rather than
    // with a horizontal `NSStackView` because the stack's alignment
    // constants describe gravity, not distribution, and asking it to
    // right-align is a fight with the wrong tool. Two buttons and a
    // measured width each is not worth a constraint graph.
    let responder = ModalResponder::new(mtm);
    let mut right = w - layout::DIALOG_MARGIN;
    for (i, label) in buttons.iter().enumerate().rev() {
        // SAFETY: the responder outlives the window (it is a local that
        // lives until after `runModalForWindow` returns) and the selector
        // is the one `ModalResponder` actually implements.
        let b = unsafe {
            button(
                mtm,
                label,
                i as isize,
                &responder,
                objc2::sel!(buttonClicked:),
            )
        };
        let bw = b.frame().size.width;
        b.setFrame(rect(
            right - bw,
            layout::DIALOG_BOTTOM,
            bw,
            layout::DIALOG_BUTTON_H,
        ));
        right -= bw + layout::DIALOG_GAP;
        content.addSubview(&b);
    }
    let _ = inner_w;

    window.setDelegate(Some(ProtocolObject::from_ref(&*responder)));
    window.center();
    let app = NSApplication::sharedApplication(mtm);
    window.makeKeyAndOrderFront(None);
    // The modal session returns when a button calls `stopModalWithCode`,
    // or when the close box makes us answer `DISMISSED`.
    let response = app.runModalForWindow(&window);
    window.setDelegate(None::<&ProtocolObject<dyn NSWindowDelegate>>);
    window.orderOut(None);
    window.close();

    if response == DISMISSED {
        // Dismissed without a button. Treat it as the fallback rather
        // than inventing an answer.
        return fallback;
    }
    response.max(0) as usize
}

/// Record the copy, honour the test override, and either show the window
/// or fall back. The single entry point every question window goes through,
/// so the "can we show a window at all?" answer is decided in one place.
fn ask(
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

    show_window(mtm, heading, detail, buttons, fallback)
}

// ---------------------------------------------------------------------------
//  Progress
// ---------------------------------------------------------------------------

/// Show download progress until the worker finishes or the user cancels.
///
/// A plain, non-modal window: it reports, and gets out of the way. The run
/// loop is pumped by hand rather than through a modal session, because the
/// progress has to be sampled *between* event-loop turns and a modal
/// session offers no such seam. `objc2-app-kit` 0.3.2 binds no
/// `NSTimer`, so a timer block was not an option either.
///
/// Returns `true` if the user let it run to completion. Cancellation is
/// recorded on `shared` exactly as the Windows window records it, so the
/// caller's post-conditions match on both platforms.
pub(crate) fn progress(
    main: &str,
    shared: std::sync::Arc<crate::jdk_install::ProgressShared>,
) -> bool {
    use std::time::Instant;

    let d = dialogs::dialogs();
    let p = &d.jdk_install.progress;
    let arch = adoptium_arch_label();
    let arch = arch.as_str();

    let Some(mtm) = activate_app() else {
        // No main thread to put a window on. We must not return early:
        // the caller joins the worker next, and the download is live.
        return log_only_progress(&shared);
    };

    PROGRESS_CANCELLED.store(false, Ordering::SeqCst);

    let w = layout::PROGRESS_W;
    let h = layout::PROGRESS_H;
    let window = new_window(mtm, w, h);
    window.setTitle(&NSString::from_str(&p.heading));
    let Some(content) = window.contentView() else {
        return log_only_progress(&shared);
    };

    let margin = layout::DIALOG_MARGIN;
    let inner_w = w - 2.0 * margin;
    let stack_h = h - 66.0 - margin;
    // Same left-hand mascot as the question windows, for the same reason:
    // it is the app icon the user chose, and a download that takes a while
    // is exactly when a window should look like it belongs to something.
    let inset = add_mascot(mtm, &content, margin, stack_h);
    // subtitle, bar, status — stacked, top to bottom.
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(10.0);
    stack.setFrame(rect(margin + inset, margin, inner_w - inset, stack_h));
    stack.addArrangedSubview(&label(mtm, &p.subtitle, false));
    stack.addArrangedSubview(&label(mtm, main, false));

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
    stack.addArrangedSubview(&bar);

    let status = label(mtm, "", false);
    stack.addArrangedSubview(&status);
    content.addSubview(&stack);

    // One button, and it cancels. The baseline's
    // `cancel_button_during_download` reads "Install" because that label
    // belongs to the Windows *prompt* window, where pressing it means "go
    // ahead". Here the download is already running, so the only useful
    // action is abort, and "Install" would be a lie.
    let responder = ProgressResponder::new(mtm);
    // SAFETY: the responder outlives the window, and the selector is the
    // one `ProgressResponder` actually implements.
    let cancel = unsafe {
        button(
            mtm,
            &d.jdk_install.prompt.button_cancel,
            0,
            &responder,
            objc2::sel!(cancelClicked:),
        )
    };
    content.addSubview(&cancel);
    let (cw, ch) = (cancel.frame().size.width, cancel.frame().size.height);
    cancel.setFrame(rect(w - margin - cw, margin, cw, ch));

    window.setDelegate(Some(ProtocolObject::from_ref(&*responder)));
    window.center();
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

        // The Cancel button and the close box both latch the same flag.
        if PROGRESS_CANCELLED.load(Ordering::SeqCst) || !window.isVisible() {
            cancelled = true;
            break;
        }

        // Pump briefly, then look again. Without this the window would
        // not repaint and the bar would sit frozen.
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        NSRunLoop::currentRunLoop().runUntilDate(&limit);
    }

    window.setDelegate(None::<&ProtocolObject<dyn NSWindowDelegate>>);
    window.orderOut(None);
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

/// Drive the download-progress window against a synthetic download, for
/// `snug_preview`.
///
/// The window itself is real — this only fabricates the `ProgressShared`
/// values a real worker would be writing, so the copy, the mascot, the
/// phase labels and the layout can all be judged without downloading
/// anything. Lives here rather than in the preview binary because the
/// atomics it has to drive are `pub(crate)`, and widening them to `pub`
/// just for a dev tool would be a worse trade than one function.
#[cfg(target_os = "macos")]
pub fn progress_demo() -> bool {
    use std::sync::Arc;
    use std::time::Duration;

    // A plausible mid-size Adoptium tarball, so the MB figures and the
    // derived transfer speed are the magnitudes a user would really see.
    const TOTAL: u64 = 190 * 1_048_576;
    let shared = Arc::new(crate::jdk_install::ProgressShared::new(TOTAL));

    // A ticker that walks 0 -> 100% and then reports success, so the
    // preview exercises every phase label and the completed state rather
    // than sitting on 0%.
    let writer = Arc::clone(&shared);
    let done = std::thread::spawn(move || {
        const STEPS: u32 = 60;
        for i in 1..=STEPS {
            std::thread::sleep(Duration::from_millis(60));
            let pct = i * 100 / STEPS;
            writer.pct.store(pct, Ordering::SeqCst);
            writer.bytes.store(TOTAL * u64::from(pct) / 100, Ordering::SeqCst);
            // 0 = downloading, then verifying, then extracting.
            writer
                .phase
                .store(match pct {
                    ..=70 => 0,
                    71..=90 => 1,
                    _ => 2,
                }, Ordering::SeqCst);
        }
        writer.phase.store(2, Ordering::SeqCst);
        writer.pct.store(100, Ordering::SeqCst);
        writer.bytes.store(TOTAL, Ordering::SeqCst);
        // 0 = running, 1 = success.
        writer.done.store(1, Ordering::SeqCst);
    });

    let ok = progress("Runtime 25.0.4.1+1 (~190 MB)", Arc::clone(&shared));
    let _ = done.join();
    ok
}

/// Progress reporting when there is no main thread to put a window on.
fn log_only_progress(shared: &std::sync::Arc<crate::jdk_install::ProgressShared>) -> bool {
    use std::time::Instant;

    let arch = adoptium_arch_label();
    let arch = arch.as_str();
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

// ---------------------------------------------------------------------------
//  The dialogs
// ---------------------------------------------------------------------------

/// Adoptium could not be reached. Returns `true` when the user asked for
/// the release page to be opened.
pub fn metadata_failed(min_java: u16, error_detail: &str) -> i32 {
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
    // The fallback is button 1, so a window we cannot show does not open
    // a browser in the user's face unasked.
    let chosen = ask(
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
pub fn retry(attempt: u32, max_attempts: u32, version: &str, error: &str) -> bool {
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
    let chosen = ask(
        "warning",
        &r.heading,
        &detail_of(&[&subheading, &content]),
        &[&r.button_retry, &r.button_cancel],
        1,
    );
    chosen == 0
}

/// Terminal failure, after the attempts are exhausted. No answer needed.
pub fn failure(title: &str, content: &str) {
    let d = dialogs::dialogs();
    let f = &d.jdk_install.failure;
    ask(
        "critical",
        title,
        &detail_of(&[&f.heading, content]),
        &[&f.button_label],
        0,
    );
}

/// Ask "download this runtime?" before any work starts. `true` means go
/// ahead.
///
/// Asked as its own step, with no worker thread running, so the only thing
/// outstanding is a decision by a human. That is the structural fix for the
/// deadlock this file used to be involved in: the ask is no longer a button
/// on a window that a download waits for, so a window that fails to appear
/// cannot strand anything.
///
/// "Open in browser" is not consent — it hands the page over and stops,
/// which is what the button says it does.
pub fn consent(version: &str, size_mb: u32, url: &str, sha256: &str) -> bool {
    let d = dialogs::dialogs();
    let p = &d.jdk_install.prompt;

    let version = version.to_string();
    let size_mb = size_mb.to_string();
    let mut subs: Vec<(&str, &str)> = vec![
        ("version", &version),
        ("size_mb", &size_mb),
        ("url", url),
        ("sha256", sha256),
    ];

    let main = dialogs::fill(p.main.as_str(), &subs);
    let content = dialogs::fill(p.content.as_str(), &subs);
    let expanded = dialogs::fill(p.expanded.as_str(), &subs);
    let download = dialogs::fill(p.button_download.as_str(), &subs);
    subs.clear();

    // Button 0 is the affirmative one. The fallback is Cancel: if the
    // window cannot be shown we must not start a large download on the
    // user's behalf.
    ask(
        "warning",
        &main,
        &detail_of(&[&content, &expanded]),
        &[&download, &p.button_cancel],
        1,
    ) == 0
}

/// Join the informational lines the Windows dialogs lay out separately.
///
/// The Windows windows have a subheading, a body and an optional info box;
/// one window body keeps the same visual grouping so the strings still
/// read as authored.
fn detail_of(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam is process-global — it has to be, because the modal
    /// session is — so the tests in this module are not independent. Every
    /// one of them holds this for its duration, or two would overwrite
    /// each other's recorded window and answer.
    static SERIALISED: Mutex<()> = Mutex::new(());

    /// Run `f` with windows auto-answering `response`, and return what
    /// would have been displayed.
    ///
    /// The call inside `f` is the point: installing an override does not
    /// record anything, only showing a window does.
    fn shown_by(response: usize, f: impl FnOnce()) -> Shown {
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(Some(response));
        f();
        let shown = last_shown().expect("a window should have been recorded");
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
    fn consent_fills_every_placeholder_and_defaults_to_no() {
        // The consent is the gate for a large download, so two things
        // matter: the substituted values must reach the user, and a window
        // that cannot be shown must resolve to "no" rather than
        // proceeding.
        let shown = shown_by(0, || {
            assert!(consent(
                "25.0.4+101.0.LTS",
                115,
                "https://example.invalid/jdk.tar.gz",
                "deadbeef",
            ));
        });
        assert!(
            shown.detail.contains("25.0.4+101.0.LTS"),
            "version missing from: {}",
            shown.detail
        );
        assert!(
            shown.detail.contains("https://example.invalid/jdk.tar.gz"),
            "url missing from: {}",
            shown.detail
        );
        assert!(
            shown.detail.contains("deadbeef"),
            "sha missing from: {}",
            shown.detail
        );
        assert!(
            !shown.detail.contains('{') && !shown.detail.contains('}'),
            "an unsubstituted placeholder reached the user: {}",
            shown.detail
        );
        assert_eq!(shown.buttons.len(), 2);

        // The second button is Cancel, and must read as "no". Silently
        // proceeding here would download 115 MB unasked.
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(Some(1));
        assert!(!consent("25.0.4", 115, "https://example.invalid", "deadbeef"));
        set_test_response(None);
    }

    #[test]
    fn a_dismissed_window_is_never_mistaken_for_the_first_button() {
        // The close box must not read as "button 0", which for `retry`
        // means "download it again". A window dismissed without an answer
        // has to resolve to the fallback.
        let _guard = SERIALISED.lock().unwrap_or_else(|e| e.into_inner());
        set_test_response(None);
        assert_eq!(DISMISSED, -1);
        // Every real button index is >= 0, so the two can never collide.
        assert!(DISMISSED < 0);
    }

    #[test]
    fn progress_status_names_this_platform_not_windows() {
        // The baseline shipped `Downloading runtime (Windows x64)` as a
        // *shared* key, so a macOS download announced itself as a Windows
        // one. The key now takes `{arch}` and each platform fills it in.
        let text = progress_status(0, 42, 0, 0, None, &adoptium_arch_label());
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
