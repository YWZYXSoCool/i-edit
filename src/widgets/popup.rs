use std::path::PathBuf;

use crate::action::{Action, Actions, ConfirmChoice};
use crate::component::Component;
use crate::icon;
use crate::widgets::command::{CommandPalette, CommandPaletteState};
use crate::widgets::picker::{Picker, PickerMode, PickerState};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use tui_logger::{TuiLoggerWidget, TuiWidgetEvent, TuiWidgetState};

use log::{debug, info};

/// Keys that dismiss an open popup.
///
/// Add `KeyCode::Char('q')` here to close on `q` as well — but note the command
/// box swallows printable keys first, so it would only work in the log viewer.
const CLOSE_POPUP_KEYS: &[KeyCode] = &[KeyCode::Esc];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PopupKind {
    #[default]
    None = 0,
    Command,
    Log,
    OpenFile,
    OpenFolder,
    SaveAs,
    Confirm,
}

impl PopupKind {
    pub fn toggle(self, kind: PopupKind) -> PopupKind {
        match self {
            PopupKind::None => kind,
            _ => PopupKind::None,
        }
    }

    #[inline]
    pub fn is_none(self) -> bool {
        self == PopupKind::None
    }
}

/// Which question the confirm popup is asking.
///
/// The popup only reports the key the user pressed; what "save & continue" or
/// "overwrite" resumes stays in the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfirmKind {
    /// The pending action would drop unsaved buffer changes.
    #[default]
    UnsavedChanges,
    /// The save-as target already exists on disk.
    Overwrite,
}

/// Everything the confirm popup owns: just the question it is asking.
#[derive(Debug, Default)]
pub struct ConfirmState {
    pub kind: ConfirmKind,
}

/// Popup kind a picker mode is shown as.
fn picker_kind(mode: PickerMode) -> PopupKind {
    match mode {
        PickerMode::File => PopupKind::OpenFile,
        PickerMode::Folder => PopupKind::OpenFolder,
        PickerMode::Save => PopupKind::SaveAs,
    }
}

/// Maps a key press to a `TuiWidgetEvent` handled by `TuiLoggerWidget`.
///
/// Returns `None` for keys the log widget does not care about.
pub fn log_widget_event(modifiers: KeyModifiers, code: KeyCode) -> Option<TuiWidgetEvent> {
    if modifiers != KeyModifiers::NONE {
        return None;
    }

    Some(match code {
        KeyCode::Char(' ') => TuiWidgetEvent::SpaceKey,
        KeyCode::Char('+') | KeyCode::Char('=') => TuiWidgetEvent::PlusKey,
        KeyCode::Char('-') => TuiWidgetEvent::MinusKey,
        KeyCode::Char('h') => TuiWidgetEvent::HideKey,
        KeyCode::Char('f') => TuiWidgetEvent::FocusKey,
        KeyCode::Up => TuiWidgetEvent::UpKey,
        KeyCode::Down => TuiWidgetEvent::DownKey,
        KeyCode::Left => TuiWidgetEvent::LeftKey,
        KeyCode::Right => TuiWidgetEvent::RightKey,
        KeyCode::PageUp => TuiWidgetEvent::PrevPageKey,
        KeyCode::PageDown => TuiWidgetEvent::NextPageKey,
        KeyCode::Esc => TuiWidgetEvent::EscapeKey,
        _ => return None,
    })
}

#[derive(Default)]
pub struct PopupState {
    pub kind: PopupKind,
    pub command: CommandPaletteState,
    pub picker: PickerState,
    pub confirm: ConfirmState,
    pub log_state: TuiWidgetState,
    actions: Actions,
}

impl PopupState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes everything the popup asked for since the last drain, the command
    /// box's and the picker's requests included.
    pub fn take_actions(&mut self) -> Vec<Action> {
        let mut actions = self.actions.drain();
        actions.extend(self.command.take_actions());
        actions.extend(self.picker.take_actions());
        actions
    }

    /// Moves everything the popup asked for onto the end of `out`, the command
    /// box's and the picker's requests included, in the same order
    /// [`Self::take_actions`] merges them.
    pub fn take_actions_into(&mut self, out: &mut Vec<Action>) {
        self.actions.take_into(out);
        self.command.take_actions_into(out);
        self.picker.take_actions_into(out);
    }

    /// Opens `kind` when the shell has no better starting point for it.
    ///
    /// Picker kinds are built on the process working directory with nothing
    /// pre-highlighted; the shell normally calls [`Self::open_picker`] with the
    /// buffer's own directory instead.
    pub fn open(&mut self, kind: PopupKind) {
        match kind {
            PopupKind::OpenFile | PopupKind::OpenFolder | PopupKind::SaveAs => {
                let mode = match kind {
                    PopupKind::OpenFile => PickerMode::File,
                    PopupKind::OpenFolder => PickerMode::Folder,
                    _ => PickerMode::Save,
                };
                let start_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                self.open_picker(mode, start_dir, None);
            }
            _ => {
                info!("popup opened: {:?}", kind);
                self.kind = kind;

                match kind {
                    PopupKind::Command => self.command.open(),
                    _ => self.command.deactivate(),
                }
            }
        }
    }

    /// Opens the picker at a location the shell knows: the directory the buffer
    /// lives in, plus the file to pre-highlight (`File`) or pre-fill (`Save`).
    pub fn open_picker(&mut self, mode: PickerMode, start_dir: PathBuf, preset: Option<PathBuf>) {
        let kind = picker_kind(mode);
        info!("popup opened: {:?} ({:?})", kind, mode);

        self.kind = kind;
        self.picker = PickerState::new(mode, start_dir, preset);
        self.command.deactivate();
    }

    /// Opens the confirm popup with a question to ask.
    pub fn open_confirm(&mut self, kind: ConfirmKind) {
        info!("popup opened: {:?} ({:?})", PopupKind::Confirm, kind);

        self.kind = PopupKind::Confirm;
        self.confirm.kind = kind;
        self.command.deactivate();
    }

    pub fn close(&mut self) {
        debug!("popup closed: {:?}", self.kind);

        // Returns the log view's scroll anchor to the newest entry.
        self.log_state.transition(TuiWidgetEvent::EscapeKey);

        self.kind = PopupKind::None;
        self.command.close();
    }
}

// `TuiWidgetState` is not `Debug`, so hand-roll it and skip the log state.
impl core::fmt::Debug for PopupState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PopupState")
            .field("kind", &self.kind)
            .field("command", &self.command)
            .field("picker", &self.picker)
            .field("confirm", &self.confirm)
            .finish_non_exhaustive()
    }
}

/// Overlay host for the command box, the log viewer, the file picker and the
/// confirm dialog.
///
/// Owns everything about being open: the keys that close it, which overlay is
/// up and the keys forwarded to each one. What an overlay does with its own
/// keys and how it draws itself belongs to that overlay — the command box lives
/// in [`CommandPalette`], the picker in [`Picker`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Popup;

impl Component for Popup {
    type State = PopupState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        if CLOSE_POPUP_KEYS.contains(&key.code) {
            state.actions.emit(Action::ClosePopup);
            return;
        }

        match state.kind {
            PopupKind::None => {}
            PopupKind::Command => {
                Component::handle_event(CommandPalette, event, &mut state.command);
            }
            PopupKind::Log => self.log_input(key, state),
            PopupKind::OpenFile | PopupKind::OpenFolder | PopupKind::SaveAs => {
                Component::handle_event(Picker, event, &mut state.picker);
            }
            PopupKind::Confirm => self.confirm_input(key, state),
        }
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        match state.kind {
            PopupKind::None => {}
            PopupKind::Command => {
                Component::render(CommandPalette, area, buf, &mut state.command);
            }
            PopupKind::Log => self.render_log(area, buf, state),
            PopupKind::OpenFile | PopupKind::OpenFolder | PopupKind::SaveAs => {
                Component::render(Picker, area, buf, &mut state.picker);
            }
            PopupKind::Confirm => self.render_confirm(area, buf, state),
        }
    }
}

impl Popup {
    fn log_input(self, key: KeyEvent, state: &mut PopupState) {
        if let Some(widget_event) = log_widget_event(key.modifiers, key.code) {
            state.log_state.transition(widget_event);
        }
    }

    /// Maps a confirm key to the answer it stands for.
    ///
    /// `Esc` never reaches here: the popup dismisses every kind on it first,
    /// which is exactly the cancel answer both questions offer.
    fn confirm_input(self, key: KeyEvent, state: &mut PopupState) {
        let action = match (state.confirm.kind, key.code) {
            (ConfirmKind::UnsavedChanges, KeyCode::Char('s')) => {
                Action::ConfirmChoice(ConfirmChoice::Save)
            }
            (ConfirmKind::UnsavedChanges, KeyCode::Char('d')) => {
                Action::ConfirmChoice(ConfirmChoice::Discard)
            }
            (ConfirmKind::Overwrite, KeyCode::Enter | KeyCode::Char('y')) => {
                Action::ConfirmChoice(ConfirmChoice::Overwrite)
            }
            (ConfirmKind::Overwrite, KeyCode::Char('n')) => Action::ClosePopup,
            _ => return,
        };

        state.actions.emit(action);
    }

    fn render_log(&self, area: Rect, buf: &mut Buffer, state: &mut PopupState) {
        let area = area.centered(Constraint::Percentage(90), Constraint::Percentage(80));
        Clear.render(area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::White))
            .title(format!("{} Log", icon::LOG));

        // `TuiLoggerWidget` implements only `Widget`: `log_state` is wrapped in
        // an `Arc<Mutex<..>>` and carried inside the widget by `.state(..)`.
        TuiLoggerWidget::default()
            .block(block)
            .state(&state.log_state)
            .render(area, buf);
    }

    fn render_confirm(&self, area: Rect, buf: &mut Buffer, state: &PopupState) {
        let area = area.centered(Constraint::Percentage(60), Constraint::Length(7));
        if area.width == 0 || area.height == 0 {
            return;
        }

        Clear.render(area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(format!("{} Confirm", icon::WARNING));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let (headline, hint) = match state.confirm.kind {
            ConfirmKind::UnsavedChanges => (
                "Unsaved changes",
                "s: save & continue   d: discard   Esc: cancel",
            ),
            ConfirmKind::Overwrite => ("File already exists", "Enter: overwrite   Esc: cancel"),
        };

        // The box is fixed-height, but a tiny terminal can still squeeze it:
        // skip a row that does not fit rather than draw outside the box.
        let rows = [
            (1u16, headline, Style::default().fg(Color::White)),
            (3u16, hint, Style::default().fg(Color::DarkGray)),
        ];

        for (offset, text, style) in rows {
            if inner.height > offset {
                // One column of padding keeps the text off the border.
                let row = Rect {
                    x: inner.x + 1,
                    y: inner.y + offset,
                    width: inner.width.saturating_sub(1),
                    height: 1,
                };

                Line::from(Span::styled(text, style)).render(row, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfirmKind, Popup, PopupKind, PopupState};
    use crate::action::{Action, ConfirmChoice};
    use crate::component::Component;
    use crate::widgets::picker::PickerMode;

    use std::path::PathBuf;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn open_command() -> PopupState {
        let mut state = PopupState::new();
        state.open(PopupKind::Command);
        state.take_actions();
        state
    }

    #[test]
    fn escape_requests_close() {
        let mut state = open_command();

        Component::handle_event(Popup, &press(KeyCode::Esc), &mut state);

        assert_eq!(state.take_actions(), vec![Action::ClosePopup]);
    }

    #[test]
    fn command_actions_surface_through_the_popup() {
        let mut state = open_command();

        for ch in "log".chars() {
            Component::handle_event(Popup, &press(KeyCode::Char(ch)), &mut state);
        }
        Component::handle_event(Popup, &press(KeyCode::Enter), &mut state);

        assert_eq!(
            state.take_actions(),
            vec![Action::OpenPopup(PopupKind::Log)]
        );
    }

    #[test]
    fn opening_another_popup_deactivates_the_command_box() {
        let mut state = open_command();
        assert!(state.command.input_state.is_editing);

        state.open(PopupKind::Log);
        assert!(!state.command.input_state.is_editing);
    }

    #[test]
    fn closing_clears_the_command_box() {
        let mut state = open_command();
        Component::handle_event(Popup, &press(KeyCode::Char('l')), &mut state);
        assert!(!state.command.input_state.text().is_empty());

        state.close();

        assert!(state.command.input_state.text().is_empty());
    }

    #[test]
    fn open_initializes_the_picker_for_the_picker_kinds() {
        let kinds = [
            (PopupKind::OpenFile, PickerMode::File),
            (PopupKind::OpenFolder, PickerMode::Folder),
            (PopupKind::SaveAs, PickerMode::Save),
        ];

        for (kind, mode) in kinds {
            let mut state = PopupState::new();
            state.open(kind);

            assert_eq!(state.kind, kind);
            assert_eq!(state.picker.mode(), mode);
        }
    }

    #[test]
    fn open_picker_sets_the_kind_from_the_mode() {
        let modes = [
            (PickerMode::File, PopupKind::OpenFile),
            (PickerMode::Folder, PopupKind::OpenFolder),
            (PickerMode::Save, PopupKind::SaveAs),
        ];

        for (mode, kind) in modes {
            let mut state = PopupState::new();
            state.open_picker(mode, PathBuf::from("."), None);

            assert_eq!(state.kind, kind);
            assert_eq!(state.picker.mode(), mode);
            assert!(!state.command.input_state.is_editing);
        }
    }

    #[test]
    fn open_confirm_switches_to_the_confirm_popup() {
        let mut state = PopupState::new();

        state.open_confirm(ConfirmKind::Overwrite);

        assert_eq!(state.kind, PopupKind::Confirm);
        assert_eq!(state.confirm.kind, ConfirmKind::Overwrite);
        assert!(!state.command.input_state.is_editing);
    }

    #[test]
    fn unsaved_confirm_keys_emit_the_choice() {
        let mut state = PopupState::new();
        state.open_confirm(ConfirmKind::UnsavedChanges);
        state.take_actions();

        Component::handle_event(Popup, &press(KeyCode::Char('s')), &mut state);
        assert_eq!(
            state.take_actions(),
            vec![Action::ConfirmChoice(ConfirmChoice::Save)]
        );

        Component::handle_event(Popup, &press(KeyCode::Char('d')), &mut state);
        assert_eq!(
            state.take_actions(),
            vec![Action::ConfirmChoice(ConfirmChoice::Discard)]
        );
    }

    #[test]
    fn overwrite_confirm_accepts_enter_or_y_and_cancels_with_n() {
        let mut state = PopupState::new();
        state.open_confirm(ConfirmKind::Overwrite);
        state.take_actions();

        for key in [KeyCode::Enter, KeyCode::Char('y')] {
            Component::handle_event(Popup, &press(key), &mut state);
            assert_eq!(
                state.take_actions(),
                vec![Action::ConfirmChoice(ConfirmChoice::Overwrite)]
            );
        }

        Component::handle_event(Popup, &press(KeyCode::Char('n')), &mut state);
        assert_eq!(state.take_actions(), vec![Action::ClosePopup]);
    }

    /// `d` is a confirm answer, but the picker owns it while a picker is up:
    /// routing the key to the wrong handler would leak a `Discard`.
    #[test]
    fn picker_keys_do_not_reach_the_confirm_handler() {
        let mut state = PopupState::new();
        state.open_picker(PickerMode::File, PathBuf::from("."), None);
        state.take_actions();

        Component::handle_event(Popup, &press(KeyCode::Char('d')), &mut state);

        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn escape_still_requests_close_for_every_kind() {
        let kinds = [
            PopupKind::OpenFile,
            PopupKind::OpenFolder,
            PopupKind::SaveAs,
            PopupKind::Confirm,
        ];

        for kind in kinds {
            let mut state = PopupState::new();
            state.open(kind);
            state.take_actions();

            Component::handle_event(Popup, &press(KeyCode::Esc), &mut state);

            assert_eq!(state.take_actions(), vec![Action::ClosePopup]);
        }
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        for kind in [
            PopupKind::OpenFile,
            PopupKind::OpenFolder,
            PopupKind::SaveAs,
            PopupKind::Confirm,
        ] {
            let mut state = PopupState::new();
            state.open(kind);
            let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));

            Component::render(Popup, Rect::new(0, 0, 1, 1), &mut buffer, &mut state);
        }
    }
}
