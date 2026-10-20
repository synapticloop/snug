//! Shared dialog frame: the geometry, fonts and colours that every
//! snug launcher dialog is built from.
//!
//! [`crate::modal_window`] and [`crate::progress_window`] are two
//! separate Win32 implementations — the second is not a subclass of
//! the first, because it repaints on a timer and has a different body
//! region. What they *do* share is the frame around that body: the
//! title bar, the mascot slot, the text column, the light-blue info
//! box and the button row. Those numbers used to be declared twice,
//! side by side, and held in agreement by comments reading "aligned
//! with `modal_window::…`" — which is a promise a compiler cannot
//! check and a reviewer has to remember to verify.
//!
//! Here they are declared once. Both dialogs `use crate::frame::*;`,
//! so "the family is one height" stops being a convention and becomes
//! a consequence.
//!
//! Deliberately *not* here:
//!
//! - **Control IDs.** `IDC_HEADING` and friends only have to be unique
//!   within a single `HWND`, and these are two separate windows.
//!   Sharing them would imply a coupling that does not exist.
//! - **Window class names.** `snug_modal_dialog_v1` and
//!   `snug_progress_dialog_v2` register distinct Win32 classes and
//!   must differ.
//! - **The `SS_*` / `WM_SETFONT` / `STM_SETICON` / `GWLP_USERDATA`
//!   transcriptions** from `winuser.h`. Both files declare them
//!   because `windows-sys` 0.59 exports no named constant for them,
//!   but nobody tunes those, so duplicating them costs nothing and
//!   keeping them out keeps this module about the shared *design*.

#![cfg(windows)]

use windows_sys::Win32::Graphics::Gdi::{FW_BOLD, FW_NORMAL, FW_SEMIBOLD};

// ============================================================================
//  Window geometry
// ============================================================================

/// Client width. Every dialog in the launcher is this wide.
pub(crate) const WINDOW_W: i32 = 640;

/// Padding between the frame edge and the frame's contents.
pub(crate) const MARGIN: i32 = 16;

/// Mascot slot, top-left of the body.
pub(crate) const MASCOT_X: i32 = MARGIN;
pub(crate) const MASCOT_Y: i32 = MARGIN;
pub(crate) const MASCOT_W: i32 = 154;
pub(crate) const MASCOT_H: i32 = 154;

/// Width/height we ask for when loading the EXE icon for the mascot
/// slot. Asking for 256 selects a detailed source for the 154 px
/// mascot box rather than enlarging the 32 px title-bar icon.
pub(crate) const MASCOT_LOAD_CX: i32 = 256;
pub(crate) const MASCOT_LOAD_CY: i32 = 256;

/// The text column, to the right of the mascot.
pub(crate) const TEXT_X: i32 = MASCOT_X + MASCOT_W + MARGIN;
pub(crate) const TEXT_W: i32 = WINDOW_W - TEXT_X - MARGIN - MARGIN;

pub(crate) const HEADING_Y: i32 = 8;
pub(crate) const HEADING_H: i32 = 48;

pub(crate) const SUBTITLE_Y: i32 = 50;
pub(crate) const SUBTITLE_H: i32 = 50;

// ============================================================================
//  Body band
// ============================================================================

/// Top of the region the dialog body may occupy, just below the
/// subtitle.
///
/// This is a *band*, not a control. `modal_window` fills it with body
/// copy and `progress_window` with a bar, a phase line and a detail
/// line — different shapes, same footprint, both starting at 108. It
/// lives here rather than in either file because the info box is
/// anchored below it, and anchoring something to a band only one
/// consumer knows about is how a gap silently appears.
pub(crate) const BODY_Y: i32 = 108;

/// Height of the band.
///
/// This is the only thing standing between the body copy and the info
/// box, so it is also the only lever for making the dialog shorter: the
/// mascot is never the constraint, because the body sits below it. It
/// was 88, which held four lines of the install prompt's body copy, and
/// is now 72 -- about a line less. Every dialog in the family got that
/// shorter, which is the point of the shared band, but the install
/// prompt is the one that was using the space.
pub(crate) const BODY_H: i32 = 72;

/// Bottom edge of the body band — the lowest the body may reach.
pub(crate) const BODY_BOTTOM: i32 = BODY_Y + BODY_H;

/// `i32::max` is not const, and this is a layout constant.
const fn lower_of(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

/// Padding between the button row and the bottom of the window.
pub(crate) const BOTTOM_PAD: i32 = 16;

/// Gap between the bottom of the info box and the button row.
///
/// Deliberately tighter than [`BOTTOM_PAD`], so the box reads as
/// attached to its buttons rather than floating between two equal gaps.
pub(crate) const INFO_TO_BUTTON_GAP: i32 = 8;

// ============================================================================
//  Info box
// ============================================================================

/// Horizontal extent of the light-blue box.
pub(crate) const INFO_BOX_X: i32 = MARGIN;
/// Symmetric: the box used to be `WINDOW_W - MARGIN * 3`, which left
/// twice the left margin as whitespace on the right (16 px in, 32 px
/// out) because the buttons used to sit inside the box's band. With
/// the buttons moved to their own row the extra reserve has no owner.
pub(crate) const INFO_BOX_W: i32 = WINDOW_W - MARGIN * 2;
/// Gap between the info box and whatever sits directly above it — the
/// mascot, or the body band, whichever reaches lower.
///
/// Anchoring to the icon alone reads well and does not lay out: the
/// modal's body copy runs to 196 while the mascot stops at 170, so a box
/// placed under the icon would sit on top of the fourth line of the
/// install prompt. Taking the lower of the two is what makes "sit below
/// the icon" and "do not overlap the body" the same statement.
pub(crate) const INFO_BOX_GAP: i32 = MARGIN;
pub(crate) const INFO_BOX_Y: i32 = lower_of(MASCOT_Y + MASCOT_H, BODY_BOTTOM) + INFO_BOX_GAP;
/// Sized for the heading plus **two** subtext lines. [`WINDOW_H`] is
/// derived from this, so growing the box grows every dialog.
pub(crate) const INFO_BOX_H: i32 = 74;
pub(crate) const INFO_BOX_CORNER_DIAMETER: i32 = INFO_BOX_H / 4;

pub(crate) const INFO_PAD: i32 = 8;
pub(crate) const INFO_ICON_SIZE: i32 = 16;
pub(crate) const INFO_ICON_Y_OFFSET: i32 = INFO_PAD + 2;
pub(crate) const INFO_HEADING_Y_OFFSET: i32 = INFO_PAD - 2;
pub(crate) const INFO_SUBTEXT_Y_OFFSET: i32 = INFO_PAD + 20;
/// Height reserved for one subtext row. The second line sits directly
/// below the first, so both are laid out from this one constant rather
/// than from a second hand-picked Y.
pub(crate) const INFO_SUBTEXT_H: i32 = 16;
pub(crate) const INFO_SUBTEXT2_Y_OFFSET: i32 = INFO_SUBTEXT_Y_OFFSET + INFO_SUBTEXT_H + 2;
pub(crate) const INFO_TEXT_X: i32 = INFO_BOX_X + INFO_PAD + INFO_ICON_SIZE + 28;
pub(crate) const INFO_TEXT_W: i32 = INFO_BOX_W - (INFO_TEXT_X - INFO_BOX_X) - INFO_PAD;

// ============================================================================
//  Button row
// ============================================================================

/// Button captions get a real font of their own rather than the system
/// default. That is not cosmetic: sizing a button from the stock GUI
/// font while it *renders* in the themed UI font under-measures by
/// ~15%, which clips the longest labels. Applying and measuring the
/// same `HFONT` makes the two agree by construction.
pub(crate) const BUTTON_PT: i32 = 9;
pub(crate) const BUTTON_H: i32 = 25;
/// Floor, so a two-letter label like `OK` still looks like a button
/// rather than a sliver.
pub(crate) const BUTTON_MIN_W: i32 = 80;
/// Right edge of the button group, and the leftward step between
/// buttons.
pub(crate) const BUTTON_RIGHT: i32 = WINDOW_W - MARGIN;
pub(crate) const BUTTON_Y: i32 = INFO_BOX_Y + INFO_BOX_H + INFO_TO_BUTTON_GAP;

/// Height of the **client** area, and the same value for every dialog
/// in the launcher.
///
/// Derived upward from the info box rather than hardcoded:
/// [`INFO_BOX_Y`] is the anchor, then the box,
/// [`INFO_TO_BUTTON_GAP`], the button row, and [`BOTTOM_PAD`]. Change
/// any one of those and every dialog in the family follows — which is
/// the whole reason this module exists.
pub(crate) const WINDOW_H: i32 =
    INFO_BOX_Y + INFO_BOX_H + INFO_TO_BUTTON_GAP + BUTTON_H + BOTTOM_PAD;

// ============================================================================
//  Fonts
// ============================================================================

pub(crate) const FONT_FACE: &str = "Segoe UI\0";

pub(crate) const HEADING_PT: i32 = 22;
pub(crate) const HEADING_WEIGHT: i32 = FW_SEMIBOLD as i32;

pub(crate) const SUBTITLE_PT: i32 = 14;
pub(crate) const SUBTITLE_WEIGHT: i32 = FW_NORMAL as i32;

pub(crate) const INFO_HEADING_PT: i32 = 12;
pub(crate) const INFO_HEADING_WEIGHT: i32 = FW_BOLD as i32;

pub(crate) const INFO_SUBTEXT_PT: i32 = 9;
pub(crate) const INFO_SUBTEXT_WEIGHT: i32 = FW_NORMAL as i32;

// ============================================================================
//  Colours (COLORREF = 0x00BBGGRR)
// ============================================================================

pub(crate) const COLOR_BG: u32 = 0x00FFFFFF;
pub(crate) const COLOR_SUBTITLE: u32 = 0x005F6368;

// ============================================================================
//  Invariants
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The family invariant, stated as a fact rather than repeated in
    /// a comment beside every constant. Both dialogs read this same
    /// `WINDOW_H`, so these cannot drift — but pinning the number
    /// catches an edit to the stack above that was not meant to change
    /// the overall height.
    const _: () = assert!(WINDOW_H == 319);
    const _: () = assert!(INFO_BOX_Y == 196);

    /// The derivations themselves, so a reordering or a dropped term
    /// fails here rather than as a misplaced pixel.
    const _: () = assert!(INFO_BOX_Y == lower_of(MASCOT_Y + MASCOT_H, BODY_BOTTOM) + INFO_BOX_GAP);
    /// The box must clear the mascot and the body band, not just one of
    /// them. The mascot is the shorter of the two today, so asserting
    /// against it alone would keep passing if the body grew into it.
    const _: () = assert!(INFO_BOX_Y >= MASCOT_Y + MASCOT_H + INFO_BOX_GAP);
    const _: () = assert!(INFO_BOX_Y >= BODY_BOTTOM + INFO_BOX_GAP);
    const _: () =
        assert!(WINDOW_H == INFO_BOX_Y + INFO_BOX_H + INFO_TO_BUTTON_GAP + BUTTON_H + BOTTOM_PAD);

    /// The info box has to clear the button row by exactly
    /// [`INFO_TO_BUTTON_GAP`], or the box stops reading as attached
    /// to its buttons.
    const _: () = assert!(BUTTON_Y - (INFO_BOX_Y + INFO_BOX_H) == INFO_TO_BUTTON_GAP);
}
