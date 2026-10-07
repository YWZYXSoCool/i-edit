//! The coloring layer: turns `(byte range, Style)` instructions into painted
//! text, without ever deciding what the colors *mean*.
//!
//! This module is deliberately agnostic. It accepts "row `y`, bytes
//! `[start, end)` get this [`Style`]" and draws it. Where the color comes from
//! — a demo source today, rust-analyzer's `semanticTokens` or diagnostics
//! tomorrow — is the producer's business, decided entirely outside this layer.
//! That is the property that lets the renderer sit underneath a future LSP
//! without being bound by its token legend.
//!
//! Three layers, resolved per byte by priority:
//!
//! ```text
//! Base (diagnostics)  <  Semantic (rust-analyzer tokens)  <  Overlay (selection)
//! ```
//!
//! A producer owns exactly one layer and replaces it wholesale. That is the
//! reason there are three and not one: three producers that refresh on their
//! own schedules would otherwise erase each other every time one of them
//! pushed. One layer each is what lets them coexist.
//!
//! **Resolution is per attribute, not per layer.** Each of `fg`, `bg`,
//! `underline_color` comes from the highest-priority layer that *sets* it, and
//! modifiers are OR-ed across all of them. That is what makes the layers
//! additive rather than exclusive: a diagnostic in the lowest layer only sets
//! an underline, so the token color above it and the selection background above
//! that all land on the same character. Picking one layer's whole `Style` would
//! make two of the three invisible.
//!
//! No container is ever built on the render path — resolving costs a `Style`
//! and one cursor per layer, which the project's zero-allocation render policy
//! requires.

pub use crate::highlight::edit::TextEdit;
pub use crate::highlight::highlights::Highlights;
pub use crate::highlight::layer::{LAYER_COUNT, LAYER_ORDER, LayerId};
pub use crate::highlight::run::StyledRun;
pub use crate::highlight::utf16::utf16_to_byte;

mod edit;
mod highlights;
mod layer;
mod run;
mod utf16;

#[cfg(test)]
mod tests;
