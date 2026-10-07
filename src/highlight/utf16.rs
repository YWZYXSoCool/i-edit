/// Converts an LSP line-column (UTF-16 code units) to a byte offset within
/// `line`. Called only on the injection path (once per token), never on render,
/// so its linear scan is free.
///
/// UTF-16 is the LSP wire unit, not bytes and not display columns; converting
/// up front means the renderer can stay in byte space and zero-convert.
pub fn utf16_to_byte(line: &str, col16: usize) -> usize {
    let mut units = 0;
    for (offset, ch) in line.char_indices() {
        if units >= col16 {
            return offset;
        }
        units += ch.len_utf16();
    }
    line.len()
}

/// Moves one edge of a run by `by` bytes, never below zero.
pub(crate) fn shifted(edge: u32, by: i64) -> u32 {
    (edge as i64 + by).max(0) as u32
}
