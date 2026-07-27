//! Segmented progress bar — shows update progress as a unified pill bar
//! divided into three logical sections.
//!
//! ## Visual design
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │ ██████████████████████▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░░░░░░ │  12px pill
//! │  System Updates        Application Updates  Dev     │  labels
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! - **System Updates** (50 %) — the OS image via bootc. While active,
//!   shows animated diagonal stripes moving left-to-right to suggest
//!   layer-by-layer downloading ("chunka chunk" feel).
//! - **Application Updates** (30 %) — Flatpak updates. Pulses softly.
//! - **Developer Tools** (20 %) — Homebrew + Distrobox combined. Pulses.
//!
//! Segment boundaries are marked by 1 px dividers at 20 % opacity.
//! Labels are rendered with Pango directly inside the drawing area so
//! they stay pixel-perfect under each segment regardless of window width.
//!
//! ## Usage
//!
//! `ignore`, not a runnable example: `container` is whatever box the caller
//! is filling, and constructing the widget at all needs an initialised GTK.
//! It was previously fenced as `rust`, so `cargo test --workspace` failed on
//! it with "cannot find value `container`".
//!
//! ```ignore
//! let bar = SegmentedProgress::new();
//! container.append(&bar.widget());
//! bar.set_module_active("system");   // start shimmer
//! bar.set_module_complete("system"); // solid fill
//! bar.mark_all_complete();           // all solid, animation stops
//! ```

use adw::prelude::*;
use gtk::cairo;
use gtk::glib;
use gtk::pango;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

// ── Segment layout constants ─────────────────────────────────────────────────

/// Proportional widths — must sum to 1.0.
const WEIGHTS: [f64; 3] = [0.50, 0.30, 0.20];

/// User-facing labels shown below each segment.
const LABELS: [&str; 3] = ["System Updates", "Application Updates", "Developer Tools"];

/// Bar height in pixels.
const BAR_H: f64 = 12.0;

/// Vertical gap between bar bottom and label baseline.
const LABEL_GAP: f64 = 6.0;

/// Font size for labels in Pango units (points × SCALE).
/// 8 pt renders as a small caption — readable but unobtrusive.
const LABEL_SIZE: i32 = 8 * pango::SCALE;

/// Total drawing area height: bar + gap + approximate label line height.
const DRAW_H: i32 = 32;

// ── Module → segment mapping ─────────────────────────────────────────────────

/// Map a uupd module key to a segment index (0 / 1 / 2).
fn module_to_seg(key: &str) -> Option<usize> {
    match key {
        "system" => Some(0),
        "flatpak" => Some(1),
        // Both Brew and Distrobox share the "Developer Tools" segment.
        "brew" | "distrobox" => Some(2),
        _ => None,
    }
}

/// Returns true if two module keys share the same visual segment.
/// Used by callers to avoid spurious "complete previous" transitions
/// when e.g. Brew and Distrobox are both part of Developer Tools.
pub fn same_segment(a: &str, b: &str) -> bool {
    module_to_seg(a) == module_to_seg(b)
}

#[derive(Debug, Clone, PartialEq)]
enum SegStatus {
    /// Not started — rendered as the dim background only.
    Pending,
    /// Running with diagonal-stripe shimmer (used for System to hint at
    /// layer-by-layer downloading).
    Downloading,
    /// Running with a soft pulse (used for Flatpak / Dev Tools).
    Active,
    /// Finished successfully — solid accent fill.
    Complete,
    /// Finished with an error — red fill.
    Failed,
}

#[derive(Debug, Clone)]
struct Segment {
    status: SegStatus,
}

impl Default for Segment {
    fn default() -> Self {
        Self {
            status: SegStatus::Pending,
        }
    }
}

// ── Public widget struct ──────────────────────────────────────────────────────

/// Segmented progress bar widget.
///
/// Not a relm4 component — call [`widget`](SegmentedProgress::widget) to get
/// the `gtk::Widget` to attach, then drive state via the public methods.
pub struct SegmentedProgress {
    root: gtk::Box,
    drawing_area: gtk::DrawingArea,
    /// Shared with the draw closure and the GLib animation timer.
    segments: Rc<RefCell<[Segment; 3]>>,
    /// Animation phase in [0.0, 1.0) — shared with the draw closure.
    phase: Rc<Cell<f64>>,
    /// Whether the animation timer is currently running.
    animating: Rc<Cell<bool>>,
    /// When false the animation timer shuts itself down.
    alive: Rc<Cell<bool>>,
}

impl SegmentedProgress {
    pub fn new() -> Self {
        let drawing_area = gtk::DrawingArea::new();
        drawing_area.set_content_height(DRAW_H);
        drawing_area.set_hexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&drawing_area);

        let segments: Rc<RefCell<[Segment; 3]>> =
            Rc::new(RefCell::new(std::array::from_fn(|_| Segment::default())));
        let phase = Rc::new(Cell::new(0.0_f64));
        let alive = Rc::new(Cell::new(true));
        let animating = Rc::new(Cell::new(false));

        // ── Draw function ────────────────────────────────────────────────
        {
            let segs_ref = segments.clone();
            let phase_ref = phase.clone();
            drawing_area.set_draw_func(move |widget, cr, width, _height| {
                let segs = segs_ref.borrow();
                let p = phase_ref.get();
                draw_bar(widget, cr, width, &segs, p);
            });
        }

        Self {
            root,
            drawing_area,
            segments,
            phase,
            animating,
            alive,
        }
    }

    /// Start the animation timer if not already running.
    fn ensure_animating(&self) {
        if self.animating.get() {
            return;
        }
        self.animating.set(true);

        let segs_ref = self.segments.clone();
        let phase_ref = self.phase.clone();
        let alive_ref = self.alive.clone();
        let animating_ref = self.animating.clone();
        let da = self.drawing_area.clone();

        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if !alive_ref.get() {
                animating_ref.set(false);
                return glib::ControlFlow::Break;
            }
            let is_active = segs_ref
                .borrow()
                .iter()
                .any(|s| matches!(s.status, SegStatus::Active | SegStatus::Downloading));
            if is_active {
                phase_ref.set((phase_ref.get() + 0.040) % 1.0);
                da.queue_draw();
                glib::ControlFlow::Continue
            } else {
                // No segments animating — stop the timer to save CPU.
                animating_ref.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// The root widget — append this to the updating content box.
    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Reset all segments to Pending (call when a new update run starts).
    pub fn reset(&self) {
        for seg in self.segments.borrow_mut().iter_mut() {
            seg.status = SegStatus::Pending;
        }
        self.drawing_area.queue_draw();
    }

    /// Mark a module as actively running.
    /// System module uses the shimmer animation; others pulse.
    /// Never downgrades a Failed segment back to Active.
    pub fn set_module_active(&self, key: &str) {
        if let Some(idx) = module_to_seg(key) {
            let mut segs = self.segments.borrow_mut();
            if segs[idx].status == SegStatus::Failed {
                return;
            }
            segs[idx].status = if idx == 0 {
                SegStatus::Downloading
            } else {
                SegStatus::Active
            };
            drop(segs);
            self.drawing_area.queue_draw();
            self.ensure_animating();
        }
    }

    /// Mark a module as successfully complete (solid fill).
    /// Never overwrites a Failed segment.
    pub fn set_module_complete(&self, key: &str) {
        if let Some(idx) = module_to_seg(key) {
            let mut segs = self.segments.borrow_mut();
            if segs[idx].status == SegStatus::Failed {
                return;
            }
            segs[idx].status = SegStatus::Complete;
            drop(segs);
            self.drawing_area.queue_draw();
        }
    }

    /// Mark a module as failed (red fill).
    pub fn set_module_failed(&self, key: &str) {
        if let Some(idx) = module_to_seg(key) {
            self.segments.borrow_mut()[idx].status = SegStatus::Failed;
            self.drawing_area.queue_draw();
        }
    }

    /// Mark all non-failed segments as complete (called on overall success).
    pub fn mark_all_complete(&self) {
        for seg in self.segments.borrow_mut().iter_mut() {
            if seg.status != SegStatus::Failed {
                seg.status = SegStatus::Complete;
            }
        }
        self.drawing_area.queue_draw();
    }
}

impl Drop for SegmentedProgress {
    fn drop(&mut self) {
        self.alive.set(false);
    }
}

// ── Cairo draw function ───────────────────────────────────────────────────────

fn draw_bar(
    widget: &gtk::DrawingArea,
    cr: &cairo::Context,
    width: i32,
    segs: &[Segment; 3],
    phase: f64,
) {
    let w = width as f64;
    let r = BAR_H / 2.0; // full pill radius

    // Accent color from Adwaita (adapts to user accent preference).
    let accent = adw::StyleManager::default().accent_color().to_rgba();
    let (ar, ag, ab) = (
        accent.red() as f64,
        accent.green() as f64,
        accent.blue() as f64,
    );

    // Foreground text color — correct for both light and dark themes.
    // GTK 4.10+ deprecated style_context().color(); use Widget::color() directly.
    let fg = widget.color();
    let (fr, fg_g, fb) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);

    // ── 1. Background pill ───────────────────────────────────────────────
    pill_path(cr, 0.0, 0.0, w, BAR_H, r);
    cr.set_source_rgba(fr, fg_g, fb, 0.08);
    cr.fill().ok();

    // ── 2. Per-segment fills ─────────────────────────────────────────────
    let mut x = 0.0f64;
    for (i, (seg, &weight)) in segs.iter().zip(WEIGHTS.iter()).enumerate() {
        let sw = w * weight;

        if seg.status != SegStatus::Pending {
            cr.save().ok();

            // Clip = pill ∩ segment rectangle — handles left/right rounding
            // automatically without special-casing first/last segments.
            pill_path(cr, 0.0, 0.0, w, BAR_H, r);
            cr.clip();
            cr.rectangle(x, 0.0, sw, BAR_H);
            cr.clip();

            match &seg.status {
                SegStatus::Pending => {}

                SegStatus::Downloading => {
                    // Solid accent fill.
                    cr.set_source_rgba(ar, ag, ab, 0.80);
                    cr.paint().ok();

                    // Animated diagonal stripes — "download chunks" effect.
                    // Parallelograms sheared at ~30° moving left → right.
                    let pitch = BAR_H * 2.5; // distance between stripe centres
                    let stripe_w = pitch * 0.45;
                    let shift = phase * pitch; // moves with animation phase

                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.18);
                    let mut sx = x - pitch + shift;
                    while sx < x + sw + pitch {
                        // Parallelogram: top-left, top-right, bottom-right, bottom-left
                        cr.move_to(sx + BAR_H, 0.0);
                        cr.line_to(sx + BAR_H + stripe_w, 0.0);
                        cr.line_to(sx + stripe_w, BAR_H);
                        cr.line_to(sx, BAR_H);
                        cr.close_path();
                        cr.fill().ok();
                        sx += pitch;
                    }
                }

                SegStatus::Active => {
                    // Soft pulse: opacity oscillates between 0.50 and 0.80.
                    let alpha = 0.50 + 0.30 * (0.5 + 0.5 * (phase * 2.0 * PI).sin());
                    cr.set_source_rgba(ar, ag, ab, alpha);
                    cr.paint().ok();
                }

                SegStatus::Complete => {
                    cr.set_source_rgba(ar, ag, ab, 1.0);
                    cr.paint().ok();
                }

                SegStatus::Failed => {
                    // Adwaita destructive red.
                    cr.set_source_rgba(0.78, 0.15, 0.15, 0.9);
                    cr.paint().ok();
                }
            }
            cr.restore().ok();
        }

        // Subtle divider between segments (skip after last).
        if i < 2 {
            let div_x = x + sw;
            cr.save().ok();
            cr.move_to(div_x, 2.0);
            cr.line_to(div_x, BAR_H - 2.0);
            cr.set_source_rgba(fr, fg_g, fb, 0.20);
            cr.set_line_width(1.0);
            cr.stroke().ok();
            cr.restore().ok();
        }

        x += sw;
    }

    // ── 3. Labels ────────────────────────────────────────────────────────
    let label_y = BAR_H + LABEL_GAP;
    let mut x = 0.0f64;

    for (i, (seg, &weight)) in segs.iter().zip(WEIGHTS.iter()).enumerate() {
        let sw = w * weight;
        let cx = x + sw / 2.0; // horizontal centre of segment

        let alpha = match &seg.status {
            SegStatus::Pending => 0.35,
            SegStatus::Active | SegStatus::Downloading => 0.85,
            SegStatus::Complete | SegStatus::Failed => 1.0,
        };

        draw_label(cr, widget, LABELS[i], cx, label_y, (fr, fg_g, fb), alpha);
        x += sw;
    }
}

/// Render a short text string centred at (`cx`, `y`) using Pango.
fn draw_label(
    cr: &cairo::Context,
    widget: &gtk::DrawingArea,
    text: &str,
    cx: f64,
    y: f64,
    color: (f64, f64, f64),
    alpha: f64,
) {
    let ctx = widget.pango_context();
    let layout = pango::Layout::new(&ctx);
    layout.set_text(text);

    // Inherit the system font family; override size to caption scale.
    let mut font = ctx
        .font_description()
        .unwrap_or_else(pango::FontDescription::new);
    font.set_size(LABEL_SIZE);
    layout.set_font_description(Some(&font));

    let (lw, _lh) = layout.size();
    let lw_px = lw as f64 / pango::SCALE as f64;

    cr.move_to(cx - lw_px / 2.0, y);
    cr.set_source_rgba(color.0, color.1, color.2, alpha);
    pangocairo::functions::show_layout(cr, &layout);
}

/// Construct a Cairo rounded-rectangle path (pill when `r == h/2`).
fn pill_path(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── module_to_seg ───────────────────────────────────────────────────
    // Maps a uupd module key onto one of the three visible segments of the
    // progress bar. brew + distrobox share segment 2 ("Developer Tools").

    #[test]
    fn system_maps_to_first_segment() {
        assert_eq!(module_to_seg("system"), Some(0));
    }

    #[test]
    fn flatpak_maps_to_second_segment() {
        assert_eq!(module_to_seg("flatpak"), Some(1));
    }

    #[test]
    fn brew_maps_to_third_segment() {
        assert_eq!(module_to_seg("brew"), Some(2));
    }

    #[test]
    fn distrobox_shares_third_segment_with_brew() {
        // brew and distrobox both render under the "Developer Tools" pill.
        assert_eq!(module_to_seg("distrobox"), Some(2));
        assert_eq!(module_to_seg("distrobox"), module_to_seg("brew"));
    }

    #[test]
    fn unknown_module_returns_none() {
        assert!(module_to_seg("mystery").is_none());
        assert!(module_to_seg("").is_none());
    }

    // ── same_segment ────────────────────────────────────────────────────
    // Callers use this to skip the "complete previous module" transition
    // when the previous and incoming module share a segment (e.g. brew →
    // distrobox shouldn't visually "complete" before going to "running"
    // again).

    #[test]
    fn same_segment_brew_and_distrobox() {
        assert!(same_segment("brew", "distrobox"));
        assert!(same_segment("distrobox", "brew"));
    }

    #[test]
    fn same_segment_self() {
        assert!(same_segment("system", "system"));
    }

    #[test]
    fn same_segment_distinct_modules() {
        assert!(!same_segment("system", "flatpak"));
        assert!(!same_segment("flatpak", "brew"));
    }

    #[test]
    fn same_segment_unknown_vs_known() {
        // An unknown module should never share a segment with a known one.
        assert!(!same_segment("mystery", "system"));
        assert!(!same_segment("flatpak", "mystery"));
    }

    #[test]
    fn same_segment_two_unknowns_reports_same() {
        // Both unknowns map to None, so `Option::eq` returns true. The
        // caller (UpdateList state machine) never actually feeds unknown
        // modules into this — `detect_module_start` is the only producer
        // and it returns Some for the 4 known modules or None. Pin the
        // behaviour anyway so a future refactor doesn't quietly change it.
        assert!(same_segment("mystery", "another-mystery"));
    }
}
