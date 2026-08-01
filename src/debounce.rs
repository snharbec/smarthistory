//! Shared bookkeeping for every mode that fetches results on a
//! debounced background thread (files, paperless, browser, ag,
//! segments, similar). Each mode's own `*State`/`*Request` types
//! still carry their mode-specific extras (`SegmentsState`'s
//! `context_cache`, `PaperlessState`'s `tag_names`, differing
//! result types on the channel, ...) — only the verbatim-identical
//! "arm/clear the debounce timer, cancel a superseded request,
//! check whether the window has elapsed" logic lives here, instead
//! of being copy-pasted once per mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A cancellation handle shared with a spawned background-fetch
/// thread. Every mode's `*Request` type carries a `cancelled: Arc<
/// AtomicBool>` field; implementing this trait exposes it so
/// [`touch`] can cancel a superseded request without knowing
/// anything else about the request's shape.
pub trait Cancellable {
    fn cancelled_flag(&self) -> &Arc<AtomicBool>;
}

/// The four fields every debounced-fetch mode's `*State` struct
/// carries verbatim: when the debounce timer was armed, what
/// pattern the last dispatched fetch used, whether one is
/// currently in flight, and its request handle (if any).
/// Implemented once per mode so [`touch`] and [`debounce_elapsed`]
/// can be written once instead of six times.
pub trait Debounced {
    type Request: Cancellable;
    fn debounce_started(&mut self) -> &mut Option<Instant>;
    fn last_pattern(&mut self) -> &mut Option<String>;
    fn in_flight(&mut self) -> &mut bool;
    fn request(&mut self) -> &mut Option<Self::Request>;
}

/// Arm or clear a mode's debounce timer depending on whether its
/// prefix is still active — the shared body of every `*_touch`
/// method (`files_touch`, `paperless_touch`, `browser_touch`,
/// `ag_touch`, `segments_touch`, `similar_touch`). When entering
/// the mode (or re-arming on every keystroke while still in it),
/// stamps `debounce_started` with `now` and cancels+drops any
/// stale in-flight request (the pattern is about to change).
/// When leaving the mode, clears every field back to its rest
/// state.
pub fn touch<S: Debounced>(state: &mut S, active: bool) {
    if active {
        *state.debounce_started() = Some(Instant::now());
        if let Some(request) = state.request().take() {
            request.cancelled_flag().store(true, Ordering::Relaxed);
        }
        *state.in_flight() = false;
    } else {
        *state.debounce_started() = None;
        *state.in_flight() = false;
        *state.request() = None;
        *state.last_pattern() = None;
    }
}

/// True when nothing is already in flight and the debounce window
/// has elapsed since it was armed — the common four-line guard
/// every `*_maybe_autocall` starts with, before its mode-specific
/// spawn (and, for paperless/segments/similar, a config-precondition
/// check interleaved after this one). Callers are still
/// responsible for their own `is_X_query()` check before calling
/// this — it assumes the mode is already known to be active.
pub fn debounce_elapsed<S: Debounced>(state: &mut S, debounce: Duration) -> bool {
    if *state.in_flight() {
        return false;
    }
    match state.debounce_started() {
        Some(started) => started.elapsed() >= debounce,
        None => false,
    }
}
