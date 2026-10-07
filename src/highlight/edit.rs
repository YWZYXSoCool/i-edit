/// One replacement of a span of text, described well enough to move the runs
/// around it.
///
/// A producer hands the coloring layer one of these per edit. Everything the
/// layer needs to know follows from it: which rows disappeared, which took
/// their place, and how far the surviving text slid along its row.
///
/// `removed_rows` / `inserted_rows` count the rows the change *covered* and
/// *produced*, so a change inside one row is `1` and `1`, and pressing Enter is
/// `1` and `2`.
///
/// `column` and the two `*_to` columns are byte offsets within their row —
/// `removed_to` on the last covered row, `inserted_to` on the last produced
/// one. For a change inside one row they are the two ends of the swap, which is
/// what makes the common case a single subtraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    /// Row the change starts on.
    pub line: usize,
    /// Byte column, on that row, where the removed text started.
    pub column: usize,
    /// How many rows the change covered.
    pub removed_rows: usize,
    /// How many rows it produced.
    pub inserted_rows: usize,
    /// Byte column, on the last covered row, where the removed text ended.
    pub removed_to: usize,
    /// Byte column, on the last produced row, where the inserted text ends.
    pub inserted_to: usize,
}

impl TextEdit {
    /// A swap inside one row: `removed` bytes at `column` become `inserted`
    /// bytes.
    pub fn within_line(line: usize, column: usize, removed: usize, inserted: usize) -> Self {
        Self {
            line,
            column,
            removed_rows: 1,
            inserted_rows: 1,
            removed_to: column + removed,
            inserted_to: column + inserted,
        }
    }

    /// The same change run backwards — what undoing it does to the runs.
    ///
    /// The two sides swap: undoing a paste removes what it inserted and puts
    /// back what it removed, at the same place.
    pub fn inverse(&self) -> Self {
        Self {
            removed_rows: self.inserted_rows,
            inserted_rows: self.removed_rows,
            removed_to: self.inserted_to,
            inserted_to: self.removed_to,
            ..*self
        }
    }
}
