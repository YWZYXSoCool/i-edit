use unicode_width::UnicodeWidthStr;

pub fn find_best_char_position(line: &str, target_display_col: usize) -> usize {
    let mut best_char_idx = 0;
    let mut min_diff = usize::MAX;
    let mut current_display_pos: usize = 0;

    for (i, c) in line.char_indices() {
        let char_width = UnicodeWidthStr::width(c.to_string().as_str());

        let diff = current_display_pos.abs_diff(target_display_col);
        if diff < min_diff {
            min_diff = diff;
            best_char_idx = i;
        }

        current_display_pos += char_width;
    }

    let end_diff = current_display_pos.abs_diff(target_display_col);
    if end_diff < min_diff {
        best_char_idx = line.len();
    }

    best_char_idx
}

pub fn digit_count(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    while n > 0 {
        n /= 10;
        count += 1;
    }
    count
}

pub(crate) fn key_press(event: &crossterm::event::Event) -> Option<crossterm::event::KeyEvent> {
    match event {
        crossterm::event::Event::Key(ev) if ev.is_press() => Some(*ev),
        _ => None,
    }
}
