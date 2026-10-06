//! The editor's keyboard-shortcut registry.
//!
//! Every binding is declared once, in [`SHORTCUTS`]. The shell matches key
//! events against this table ([`lookup`]), and the welcome screen is built
//! from it ([`welcome_lines`]) — so the two can never drift apart. To change a
//! shortcut, edit [`SHORTCUTS`]; both the behaviour and the on-screen hint
//! follow.

use crate::action::Action;
use crate::widgets::popup::PopupKind;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key combination, stripped down to the modifiers that actually bind.
///
/// Only `control` and `alt` matter for matching. `shift` is ignored on letter
/// keys because terminals disagree on whether they report it (and most bindings
/// never use it), so `Ctrl+O`, `Ctrl+Shift+O` and `Ctrl+o` are all the same
/// binding. `shift` is tracked only so the welcome screen can label the few
/// chords that genuinely require it (e.g. Shift+Tab to cycle focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    pub code: KeyCode,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Combo {
    /// A control-key combo, with `alt` opting into the Ctrl+Alt variant.
    const fn ctrl(code: KeyCode, alt: bool) -> Self {
        Self {
            code,
            control: true,
            alt,
            shift: false,
        }
    }

    /// A bare key with no modifier, for shell navigation (Esc, Tab).
    const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            control: false,
            alt: false,
            shift: false,
        }
    }

    /// A key held with Shift, for shell navigation (Shift+Tab to cycle focus).
    const fn shifted(code: KeyCode) -> Self {
        Self {
            code,
            control: false,
            alt: false,
            shift: true,
        }
    }

    /// Human-readable label, e.g. `"Ctrl+Alt+O"` or `"Shift+Tab"`.
    pub fn label(self) -> String {
        let mut label = String::new();
        if self.control {
            label.push_str("Ctrl+");
        }
        if self.alt {
            label.push_str("Alt+");
        }
        if self.shift {
            label.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(c) => label.push(c.to_ascii_uppercase()),
            KeyCode::Esc => label.push_str("Esc"),
            KeyCode::Tab => label.push_str("Tab"),
            KeyCode::Enter => label.push_str("Enter"),
            KeyCode::Backspace => label.push_str("Backspace"),
            KeyCode::Delete => label.push_str("Delete"),
            other => label.push_str(&format!("{other:?}")),
        }
        label
    }

    /// True when `key` satisfies this combo.
    ///
    /// Letter case is normalised and `shift` is ignored, so the three forms
    /// of a control-letter binding all match. Any modifier outside
    /// control/alt/shift disqualifies the key.
    fn matches(self, key: KeyEvent) -> bool {
        let normalize = |code: KeyCode| match code {
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        if normalize(key.code) != normalize(self.code) {
            return false;
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // The binding only cares about control and alt; reject anything else
        // so a key held with an unsupported modifier is not silently treated
        // as a shortcut. Shift is tolerated because terminals report it
        // inconsistently on control-letter combinations.
        let known = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        let only_known = (key.modifiers & known) == key.modifiers;

        control == self.control && alt == self.alt && only_known
    }
}

/// A registered shortcut.
///
/// `action` is `None` for shell-level navigation the app handles itself
/// (Esc, Tab, Shift+Tab) rather than through [`crate::app::App::apply`].
/// `welcome` gates the welcome-screen listing, used to hide the redundant
/// Ctrl+Enter alias.
#[derive(Debug, Clone)]
pub struct Shortcut {
    pub combo: Combo,
    pub description: &'static str,
    pub action: Option<Action>,
    pub welcome: bool,
}

/// Every editor shortcut, in the order they appear on the welcome screen.
///
/// The control shortcuts are matched by [`lookup`]; Esc, Tab and Shift+Tab are
/// listed for the welcome but handled directly by the shell because their
/// meaning depends on the current focus.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('o'), false),
        description: "open file",
        action: Some(Action::OpenPopup(PopupKind::OpenFile)),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('o'), true),
        description: "open folder",
        action: Some(Action::OpenPopup(PopupKind::OpenFolder)),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('s'), false),
        description: "save",
        action: Some(Action::Save),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('s'), true),
        description: "save as",
        action: Some(Action::SaveAs),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('b'), false),
        description: "toggle file tree",
        action: Some(Action::ToggleFileTree),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('q'), true),
        description: "commands",
        action: Some(Action::OpenPopup(PopupKind::Command)),
        welcome: true,
    },
    Shortcut {
        combo: Combo::ctrl(KeyCode::Char('m'), false),
        description: "clear messages",
        action: Some(Action::ClearMessages),
        welcome: true,
    },
    // A terminal that cannot tell Ctrl+M from Enter reports the latter; both
    // clear the message box, but only the Ctrl+M form is shown.
    Shortcut {
        combo: Combo::ctrl(KeyCode::Enter, false),
        description: "clear messages",
        action: Some(Action::ClearMessages),
        welcome: false,
    },
    Shortcut {
        combo: Combo::plain(KeyCode::Esc),
        description: "quit",
        action: None,
        welcome: true,
    },
    Shortcut {
        combo: Combo::shifted(KeyCode::Tab),
        description: "switch panel",
        action: None,
        welcome: true,
    },
    Shortcut {
        combo: Combo::plain(KeyCode::Tab),
        description: "insert 4 spaces",
        action: None,
        welcome: true,
    },
];

/// Key the shell uses to quit when nothing is open.
pub const QUIT_KEY: KeyCode = KeyCode::Esc;

/// True when `key` is the focus-cycle chord: Alt+Tab.
///
/// Terminals are inconsistent about how they report Alt+Tab — some send the
/// dedicated [`KeyCode::BackTab`] code, others send [`KeyCode::Tab`] with the
/// Alt modifier — so both forms are accepted. Plain Tab is deliberately not
/// matched here: it falls through to the editor, which inserts indentation.
pub fn is_focus_key(key: KeyEvent) -> bool {
    key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::ALT))
}

/// Maps a key event to the action it triggers, or `None` if it is not a
/// registered shortcut. The caller falls through to typing or component
/// handling on `None`.
pub fn lookup(key: KeyEvent) -> Option<Action> {
    for shortcut in SHORTCUTS {
        if let Some(action) = &shortcut.action
            && shortcut.combo.matches(key)
        {
            return Some(action.clone());
        }
    }
    None
}

/// The `(label, description)` pairs for the welcome screen, in registry order.
///
/// Generated from [`SHORTCUTS`], so it always reflects the live bindings.
pub fn welcome_lines() -> Vec<(String, &'static str)> {
    SHORTCUTS
        .iter()
        .filter(|s| s.welcome)
        .map(|s| (s.combo.label(), s.description))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn every_control_binding_resolves_to_an_action() {
        for shortcut in SHORTCUTS {
            if shortcut.combo.control {
                assert!(
                    shortcut.action.is_some(),
                    "control shortcut {:?} should map to an action",
                    shortcut.combo
                );
            }
        }
    }

    #[test]
    fn the_registry_matches_the_original_bindings() {
        // Ctrl+O / Ctrl+Alt+O -> open file / open folder.
        assert_eq!(
            lookup(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Action::OpenPopup(PopupKind::OpenFile))
        );
        assert_eq!(
            lookup(key(
                KeyCode::Char('O'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(Action::OpenPopup(PopupKind::OpenFolder))
        );

        // Ctrl+S / Ctrl+Alt+S -> save / save as.
        assert_eq!(
            lookup(key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Action::Save)
        );
        assert_eq!(
            lookup(key(
                KeyCode::Char('S'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(Action::SaveAs)
        );

        // Ctrl+B / Ctrl+Alt+Q / Ctrl+M keep their actions.
        assert_eq!(
            lookup(key(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(Action::ToggleFileTree)
        );
        assert_eq!(
            lookup(key(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(Action::OpenPopup(PopupKind::Command))
        );
        assert_eq!(
            lookup(key(KeyCode::Char('m'), KeyModifiers::CONTROL)),
            Some(Action::ClearMessages)
        );
    }

    #[test]
    fn shift_is_ignored_on_letter_bindings() {
        // Ctrl+Shift+O must mean the same as Ctrl+O.
        assert_eq!(
            lookup(key(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Action::OpenPopup(PopupKind::OpenFile))
        );
    }

    #[test]
    fn non_control_keys_are_not_shortcuts() {
        assert_eq!(lookup(key(KeyCode::Char('o'), KeyModifiers::NONE)), None);
        // Alt alone (no control) binds nothing.
        assert_eq!(lookup(key(KeyCode::Char('o'), KeyModifiers::ALT)), None);
    }

    #[test]
    fn navigation_keys_are_listed_but_not_matched() {
        // Esc and Tab have no action in the table, so lookup leaves them to
        // the shell.
        assert_eq!(lookup(key(KeyCode::Esc, KeyModifiers::NONE)), None);
        assert_eq!(lookup(key(KeyCode::Tab, KeyModifiers::NONE)), None);

        let labels: Vec<&str> = welcome_lines().iter().map(|(_, d)| *d).collect();
        assert!(labels.contains(&"quit"));
        assert!(labels.contains(&"switch panel"));
    }

    #[test]
    fn welcome_labels_describe_the_real_bindings() {
        let lines: Vec<(String, &str)> = welcome_lines();
        // No stale "Ctrl+Shift+O" hints survive: folder opening is Ctrl+Alt+O.
        assert!(
            lines
                .iter()
                .any(|(label, desc)| label == "Ctrl+Alt+O" && *desc == "open folder")
        );
        assert!(
            lines
                .iter()
                .any(|(label, desc)| label == "Ctrl+O" && *desc == "open file")
        );
        // The Ctrl+Enter alias is hidden.
        assert!(!lines.iter().any(|(label, _)| label == "Ctrl+Enter"));
    }
}
