use crate::action::{Action, Actions};
use crate::component::Component;
use crate::icon;
use crate::widgets::popup::PopupKind;
use crate::widgets::{Input, InputState};

use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Widget};

use log::{info, warn};

/// Most suggestion rows shown under the command box.
const MAX_SUGGESTIONS: usize = 6;

/// Rows of clearance above the command box.
const COMMAND_BOX_TOP: u16 = 2;

/// Height of the command box itself: one line of text inside a border.
const INPUT_HEIGHT: u16 = 3;

/// Shown in the empty command box.
const COMMAND_PLACEHOLDER: &str = "type a command…";

/// Highlight of the suggestion under the cursor.
const SELECTED_STYLE: Style = Style::new().fg(Color::Black).bg(Color::White);

/// Highlight of every other suggestion.
const SUGGESTION_STYLE: Style = Style::new().fg(Color::Gray);

/// A single command: what to type, what it does and what it asks the app
/// shell for.
///
/// Not `Copy`: [`Action`] carries a path since the file commands exist.
#[derive(Debug, Clone)]
struct Command {
    name: &'static str,
    description: &'static str,
    action: Action,
}

impl Command {
    /// Case-insensitive prefix match against what has been typed so far.
    ///
    /// Input is lowercased before matching, so command names must be written
    /// lowercase.
    fn matches(&self, input: &str) -> bool {
        self.name.starts_with(input)
    }
}

/// Defines the command catalog: one `name => description => action` row per
/// command.
///
/// The palette, its suggestions and dispatch all read from the list this
/// generates, so a new command is one row here — nothing else.
macro_rules! commands {
    ($( $name:literal => $description:literal => $action:expr ),+ $(,)?) => {
        /// Everything the command box accepts.
        const COMMANDS: &[Command] = &[
            $( Command {
                name: $name,
                description: $description,
                action: $action,
            } ),+
        ];
    };
}

commands! {
    "log" => "open the log panel" => Action::OpenPopup(PopupKind::Log),
    "clear messages" => "clear the notification messages" => Action::ClearMessages,
    "open file" => "open a file into the editor" => Action::OpenPopup(PopupKind::OpenFile),
    "open folder" => "open a folder into the file tree" => Action::OpenPopup(PopupKind::OpenFolder),
    "quit" => "exit the editor" => Action::Quit,
    "save" => "save the current buffer" => Action::Save,
    "save as" => "save the buffer to a new path" => Action::SaveAs,
    "toggle file tree" => "show or hide the file tree" => Action::ToggleFileTree,
}

/// Everything [`CommandPalette`] needs: the text being typed, the matching
/// suggestions and the requests on their way to the app shell.
#[derive(Debug, Default)]
pub struct CommandPaletteState {
    pub input_state: InputState,
    suggestions: Vec<usize>,
    selected_suggestion: usize,
    actions: Actions,
}

impl CommandPaletteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes everything this component has asked for since the last drain.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    /// Starts accepting input. Text typed earlier is kept.
    pub fn open(&mut self) {
        self.input_state.is_editing = true;
        self.refresh_suggestions();
    }

    /// Stops accepting input without forgetting it, e.g. while another popup
    /// covers this one.
    pub fn deactivate(&mut self) {
        self.input_state.is_editing = false;
        self.suggestions.clear();
        self.selected_suggestion = 0;
    }

    /// Forgets the input along with its suggestions.
    pub fn close(&mut self) {
        self.input_state.clear();
        self.deactivate();
    }

    fn handle_key(&mut self, key: KeyEvent, event: &Event) {
        match key.code {
            KeyCode::Enter => {
                let command = self.input_state.take_text();
                self.run_command(&command);
                return;
            }

            KeyCode::Tab => {
                self.accept_suggestion();
                self.refresh_suggestions();
                return;
            }

            // While suggestions are open, ↑↓ walk them instead of jumping to
            // the ends of the input line.
            KeyCode::Up if !self.suggestions.is_empty() => {
                self.select_previous();
                return;
            }
            KeyCode::Down if !self.suggestions.is_empty() => {
                self.select_next();
                return;
            }

            _ => {}
        }

        Component::handle_event(Input::new(), event, &mut self.input_state);
        self.refresh_suggestions();
    }

    fn run_command(&mut self, command: &str) {
        let command = command.trim();
        info!("command: {:?}", command);

        let action = match command {
            "" => Action::ClosePopup,
            name => match COMMANDS.iter().find(|registered| registered.name == name) {
                Some(registered) => registered.action.clone(),
                None => {
                    warn!("unknown command: {:?}", name);
                    Action::ClosePopup
                }
            },
        };

        self.actions.emit(action);
    }

    /// Recomputes the suggestion list from the current input.
    ///
    /// Empty input lists every command, so the box doubles as a palette.
    fn refresh_suggestions(&mut self) {
        let input = self.input_state.text().to_lowercase();
        let input = input.trim();

        self.suggestions = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, command)| command.matches(input))
            .map(|(idx, _)| idx)
            .collect();

        if self.selected_suggestion >= self.suggestions.len() {
            self.selected_suggestion = 0;
        }
    }

    /// Replaces the input with the highlighted suggestion.
    fn accept_suggestion(&mut self) {
        let Some(command) = self.selected_command() else {
            return;
        };

        self.input_state.set_text(command.name);
    }

    fn select_previous(&mut self) {
        let len = self.suggestions.len();

        if len > 0 {
            self.selected_suggestion = (self.selected_suggestion + len - 1) % len;
        }
    }

    fn select_next(&mut self) {
        let len = self.suggestions.len();

        if len > 0 {
            self.selected_suggestion = (self.selected_suggestion + 1) % len;
        }
    }

    fn selected_command(&self) -> Option<&'static Command> {
        self.suggestions
            .get(self.selected_suggestion)
            .map(|&idx| &COMMANDS[idx])
    }

    fn suggestion_rows(&self) -> u16 {
        self.suggestions.len().min(MAX_SUGGESTIONS) as u16
    }
}

/// The command box: `80 %` of the overlay width, hung from a fixed offset so it
/// does not jump around as suggestion rows come and go.
fn command_box_area(area: Rect, height: u16) -> Rect {
    let width = (area.width as u32 * 80 / 100) as u16;

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + COMMAND_BOX_TOP,
        width: width.min(area.width),
        height: height.min(area.height.saturating_sub(COMMAND_BOX_TOP)),
    }
}

/// The command palette overlay: a text box that filters [`COMMANDS`] and runs
/// the one that is picked.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommandPalette;

impl Component for CommandPalette {
    type State = CommandPaletteState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        state.handle_key(key, event);
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Input box plus one row per suggestion, never taller than the overlay.
        let rows = state.suggestion_rows();
        let height = (INPUT_HEIGHT + rows).min(area.height.saturating_sub(COMMAND_BOX_TOP));

        let area = command_box_area(area, height);
        Clear.render(area, buf);

        let [input_area, list_area] =
            Layout::vertical([Constraint::Length(INPUT_HEIGHT), Constraint::Fill(1)]).areas(area);

        let placeholder = format!("{} {COMMAND_PLACEHOLDER}", icon::COMMAND);

        Component::render(
            Input::new().placeholder(&placeholder),
            input_area,
            buf,
            &mut state.input_state,
        );

        if rows > 0 {
            render_suggestions(list_area, buf, state);
        }
    }
}

fn render_suggestions(area: Rect, buf: &mut Buffer, state: &CommandPaletteState) {
    for (row, command) in state
        .suggestions
        .iter()
        .map(|&idx| &COMMANDS[idx])
        .take(MAX_SUGGESTIONS)
        .enumerate()
    {
        let row_area = Rect {
            y: area.y + row as u16,
            height: 1,
            ..area
        };

        let selected = row == state.selected_suggestion;
        let style = if selected {
            SELECTED_STYLE
        } else {
            SUGGESTION_STYLE
        };
        // Both markers are one column wide, so the rows stay aligned.
        let marker = if selected { icon::CHEVRON } else { " " };

        Line::from(Span::styled(
            format!(" {marker} {} · {}", command.name, command.description),
            style,
        ))
        .render(row_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::{COMMANDS, CommandPalette, CommandPaletteState};
    use crate::action::Action;
    use crate::component::Component;
    use crate::widgets::popup::PopupKind;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Types `text` into the palette and presses Enter.
    fn command(state: &mut CommandPaletteState, text: &str) -> Vec<Action> {
        state.take_actions();

        for ch in text.chars() {
            Component::handle_event(CommandPalette, &press(KeyCode::Char(ch)), state);
        }
        Component::handle_event(CommandPalette, &press(KeyCode::Enter), state);

        state.take_actions()
    }

    fn open_palette() -> CommandPaletteState {
        let mut state = CommandPaletteState::new();
        state.open();
        state.take_actions();
        state
    }

    #[test]
    fn log_command_requests_the_log_view() {
        let mut state = open_palette();

        assert_eq!(
            command(&mut state, "log"),
            vec![Action::OpenPopup(PopupKind::Log)]
        );
    }

    #[test]
    fn quit_command_requests_exit() {
        let mut state = open_palette();

        assert_eq!(command(&mut state, "quit"), vec![Action::Quit]);
    }

    #[test]
    fn file_commands_reach_the_shell() {
        let cases = [
            ("open file", Action::OpenPopup(PopupKind::OpenFile)),
            ("open folder", Action::OpenPopup(PopupKind::OpenFolder)),
            ("save", Action::Save),
            ("save as", Action::SaveAs),
            ("toggle file tree", Action::ToggleFileTree),
        ];

        for (name, expected) in cases {
            let mut state = open_palette();

            assert_eq!(command(&mut state, name), vec![expected], "{name}");
        }
    }

    #[test]
    fn clear_messages_command_reaches_the_shell() {
        let mut state = open_palette();

        assert_eq!(
            command(&mut state, "clear messages"),
            vec![Action::ClearMessages]
        );
    }

    #[test]
    fn unknown_command_requests_close() {
        let mut state = open_palette();

        assert_eq!(command(&mut state, "nope"), vec![Action::ClosePopup]);
    }

    #[test]
    fn an_untouched_outbox_stays_empty() {
        let mut state = open_palette();

        Component::handle_event(CommandPalette, &press(KeyCode::Char('l')), &mut state);

        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn opening_the_command_box_lists_every_command() {
        let mut state = CommandPaletteState::new();
        state.open();

        assert_eq!(state.suggestions.len(), COMMANDS.len());
    }

    #[test]
    fn command_names_are_lowercase() {
        // Input is lowercased before matching, so an uppercase name could
        // never be typed.
        for command in COMMANDS {
            assert_eq!(command.name, command.name.to_lowercase());
        }
    }

    #[test]
    fn typing_filters_the_suggestions() {
        let mut state = CommandPaletteState::new();
        state.open();

        Component::handle_event(CommandPalette, &press(KeyCode::Char('l')), &mut state);
        assert_eq!(state.suggestions.len(), 1);

        Component::handle_event(CommandPalette, &press(KeyCode::Char('z')), &mut state);
        assert!(state.suggestions.is_empty());
    }

    #[test]
    fn tab_completes_the_highlighted_suggestion() {
        let mut state = CommandPaletteState::new();
        state.open();

        Component::handle_event(CommandPalette, &press(KeyCode::Tab), &mut state);

        assert_eq!(state.input_state.text(), "log");
        assert_eq!(state.input_state.cursor_position(), 3);
    }

    #[test]
    fn closing_forgets_the_input_and_its_suggestions() {
        let mut state = CommandPaletteState::new();
        state.open();
        Component::handle_event(CommandPalette, &press(KeyCode::Char('l')), &mut state);
        assert!(!state.suggestions.is_empty());

        state.close();

        assert!(state.input_state.text().is_empty());
        assert!(state.suggestions.is_empty());
        assert!(!state.input_state.is_editing);
    }

    #[test]
    fn deactivating_keeps_the_input_but_drops_the_suggestions() {
        let mut state = CommandPaletteState::new();
        state.open();
        Component::handle_event(CommandPalette, &press(KeyCode::Char('l')), &mut state);

        state.deactivate();

        assert_eq!(state.input_state.text(), "l");
        assert!(state.suggestions.is_empty());
    }
}
