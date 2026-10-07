use crate::highlight::edit::TextEdit;
use crate::highlight::run::StyledRun;
use crate::highlight::utf16::shifted;

/// Which layer a producer writes to, in ascending priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerId {
    /// Diagnostics: `publishDiagnostics` underlines. Lowest priority by
    /// design — it only ever sets an underline color and never a foreground, so
    /// the token color above it shows through and priority never has to be
    /// fought over. Nothing is "below" a decoration that claims no foreground.
    Base,
    /// Semantic coloring: `textDocument/semanticTokens`. Owns the foreground
    /// for code; loses nothing by sitting above the diagnostics, which do not
    /// set one.
    Semantic,
    /// The selection: transient, urgent, and the only layer that paints a
    /// background over code the user is looking at.
    Overlay,
}

/// The layers in ascending priority: later entries win where they cover a byte.
///
/// Also the order of the array [`Highlights::line_runs`] returns, so a producer
/// that wants to walk them generically (the renderer) has one source of truth
/// instead of three hardcoded arms.
pub const LAYER_ORDER: [LayerId; 3] = [LayerId::Base, LayerId::Semantic, LayerId::Overlay];

/// How many layers there are. Used where a literal `3` would drift from
/// [`LAYER_ORDER`] the next time a layer is added.
pub const LAYER_COUNT: usize = LAYER_ORDER.len();

/// One row's `[start, end)` slice into [`Layer::arena`].
#[derive(Debug, Clone, Copy, Default)]
struct LineSpan {
    start: u32,
    end: u32,
}

/// A flat arena of runs plus a per-row index into it.
///
/// Storing every row's runs in a single `Vec` (rather than
/// `Vec<Vec<StyledRun>>`) keeps injection to two allocations and is
/// cache-friendly on the render path, which walks the arena through indices
/// only — never through per-row allocations.
#[derive(Debug, Default)]
pub(crate) struct Layer {
    arena: Vec<StyledRun>,
    lines: Vec<LineSpan>,
    /// Rows `0..valid_upto` are consistent with the current text; rows at or
    /// past it are stale and render as empty. Lowering the watermark on an edit
    /// is O(1) and allocation-free — no per-row work, no splice.
    valid_upto: usize,
}

impl Layer {
    /// Runs for row `y`, or an empty slice when `y` is stale or out of range.
    pub(crate) fn runs_for(&self, y: usize) -> &[StyledRun] {
        if y < self.valid_upto && y < self.lines.len() {
            let span = &self.lines[y];
            &self.arena[span.start as usize..span.end as usize]
        } else {
            &[]
        }
    }

    /// Replaces every row's runs. `rows` must be in ascending row order, each
    /// row's runs in ascending `start`. This is the push side of the model: it
    /// may allocate (once for the arena, once for the index) but only when new
    /// data arrives — never on the render or key paths.
    pub(crate) fn replace(&mut self, rows: &[Vec<StyledRun>]) {
        let total: usize = rows.iter().map(Vec::len).sum();
        self.arena.clear();
        self.arena.reserve(total);
        self.lines.clear();
        self.lines.reserve(rows.len());
        for row in rows {
            let start = self.arena.len() as u32;
            self.arena.extend_from_slice(row);
            let end = self.arena.len() as u32;
            self.lines.push(LineSpan { start, end });
        }
        self.valid_upto = self.lines.len();
    }

    /// Drops all runs, keeping capacity so the next `replace` is
    /// allocation-free in the steady state.
    pub(crate) fn clear(&mut self) {
        self.arena.clear();
        self.lines.clear();
        self.valid_upto = 0;
    }

    /// The arena index of the first run on row `y` that `keep` rejects — the
    /// end of the surviving prefix, in other words.
    fn scan(&self, y: usize, mut keep: impl FnMut(&StyledRun) -> bool) -> usize {
        let span = self.lines[y];
        let mut i = span.start as usize;
        while i < span.end as usize && keep(&self.arena[i]) {
            i += 1;
        }
        i
    }

    /// Carries the runs across an edit instead of dropping them.
    ///
    /// Three things survive, and everything else is what the change touched:
    /// the runs on the first row that end before it began, the runs on the last
    /// row that start after it ended, and every row below it — which keeps its
    /// runs untouched and only changes row number. Dropping the lot instead
    /// would blank the whole file below the cursor on every keystroke, and the
    /// next response from the server is hundreds of milliseconds away.
    ///
    /// A run that *straddles* the change is dropped: it covers text that no
    /// longer exists, and guessing where it ended up would invent a color.
    pub(crate) fn apply_edit(&mut self, edit: &TextEdit) {
        let rows = self.lines.len();
        let first = edit.line;

        // Past the end of the index the change only appended rows, and none of
        // them has runs.
        if first >= rows {
            let was_complete = self.valid_upto >= rows;
            self.lines
                .resize(first + edit.inserted_rows, LineSpan::default());
            if was_complete {
                self.valid_upto = self.lines.len();
            }
            return;
        }

        // A change claiming more rows than there are is clipped to what exists.
        let last = (first + edit.removed_rows - 1).min(rows - 1);
        let covered = last - first + 1;

        // Runs are only facts below the watermark. A row at or past it is
        // waiting for new data, and what it still holds describes text that is
        // already gone — carrying it forward would invent colors.
        let trusted = last < self.valid_upto;

        let row_start = self.lines[first].start;
        let head_end = self.scan(first, |run| run.end <= edit.column as u32);
        let tail_start = if trusted {
            self.scan(last, |run| run.start < edit.removed_to as u32)
        } else {
            self.lines[last].end as usize
        };
        let tail_end = self.lines[last].end as usize;

        let slide = edit.inserted_to as i64 - edit.removed_to as i64;
        if slide != 0 {
            for run in &mut self.arena[tail_start..tail_end] {
                run.start = shifted(run.start, slide);
                run.end = shifted(run.end, slide);
            }
        }

        // Between the head and the tail: the rest of the first row, every row
        // swallowed whole, and the head of the last row.
        let dropped = tail_start - head_end;
        self.arena.drain(head_end..tail_start);

        // Runs below the change moved down in the arena by what was dropped.
        // The covered rows are replaced below, so they are left alone.
        if dropped > 0 {
            let dropped = dropped as u32;
            for span in &mut self.lines[last + 1..] {
                span.start -= dropped;
                span.end -= dropped;
            }
        }

        // What the change produced: the head stays on its first row, the tail
        // lands on its last, and the rows between the two arrived empty.
        let head_end = head_end as u32;
        let tail_end = (tail_end - dropped) as u32;
        let blank = LineSpan {
            start: head_end,
            end: head_end,
        };
        let produced = edit.inserted_rows;
        self.lines.splice(
            first..=last,
            (0..produced).map(|i| {
                if produced == 1 {
                    LineSpan {
                        start: row_start,
                        end: tail_end,
                    }
                } else if i == 0 {
                    LineSpan {
                        start: row_start,
                        end: head_end,
                    }
                } else if i + 1 == produced {
                    LineSpan {
                        start: head_end,
                        end: tail_end,
                    }
                } else {
                    blank
                }
            }),
        );

        // The rows the change produced are exact, so they count as valid;
        // anything below them keeps its standing, on its new row number.
        self.valid_upto = if self.valid_upto > last {
            (self.valid_upto + produced).saturating_sub(covered)
        } else if self.valid_upto > first {
            first + produced
        } else {
            self.valid_upto
        };
        self.valid_upto = self.valid_upto.min(self.lines.len());
    }

    /// Resizes the index to `line_count` empty rows, e.g. after a file load.
    pub(crate) fn reset(&mut self, line_count: usize) {
        self.arena.clear();
        self.lines.clear();
        self.lines.reserve(line_count);
        self.lines.resize(line_count, LineSpan::default());
        self.valid_upto = line_count;
    }
}

#[cfg(test)]
impl Layer {
    /// Marks every row at or past `line` as no longer describing the text — the
    /// state a layer is in while it waits for fresh data. Only tests can put a
    /// layer there; in the editor it happens by itself.
    pub(crate) fn invalidate_from(&mut self, line: usize) {
        if line < self.valid_upto {
            self.valid_upto = line;
        }
    }
}
