//! Helpers shared between codex-potter and the single-turn TUI runner.
//!
//! This crate intentionally keeps only the small subset of the upstream Codex
//! protocol/model helpers that are required by the renderer.

/// Placeholder label inserted into user text when attaching a local image.
///
/// This must remain byte-for-byte compatible with the legacy `codex-tui` UI so
/// prompt rendering and placeholder replacement stay unchanged.
pub fn local_image_label_text(label_number: usize) -> String {
    format!("[Image #{label_number}]")
}
