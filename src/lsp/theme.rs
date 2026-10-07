//! Turning a token legend into colors.
//!
//! One Dark color palette adapted for 16-color / true-color terminals.

use crate::lsp::protocol::{Legend, Severity, TokenModifier, TokenType};

use log::warn;
use ratatui::style::{Color, Modifier, Style};

/// One row per legend entry: the style for token type *n* is `types[n]`.
#[derive(Debug, Clone, Default)]
pub struct StyleTable {
    types: Vec<Style>,
    modifiers: Vec<Modifier>,
}

impl StyleTable {
    pub fn build(legend: &Legend) -> Self {
        let types = legend
            .token_types
            .iter()
            .map(|kind| kind.map_or_else(Style::default, style_for))
            .collect();

        let modifiers = legend
            .token_modifiers
            .iter()
            .map(|kind| kind.map_or(Modifier::empty(), modifier_for))
            .collect();

        if !legend.unknown_types.is_empty() {
            warn!(
                "lsp: no color mapped for token types {:?}",
                legend.unknown_types
            );
        }

        Self { types, modifiers }
    }

    pub fn style(&self, type_index: u32, modifier_bits: u32) -> Style {
        let mut style = self
            .types
            .get(type_index as usize)
            .copied()
            .unwrap_or_default();

        let mut bits = modifier_bits;
        while bits != 0 {
            let bit = bits.trailing_zeros();
            bits &= bits - 1;
            if let Some(modifier) = self.modifiers.get(bit as usize) {
                style = style.add_modifier(*modifier);
            }
        }

        style
    }
}

/// One Dark semantic colors (true-color safe, 16-color tolerant)
fn style_for(kind: TokenType) -> Style {
    use Color as C;
    use TokenType as T;

    match kind {
        //
        // Comments & Docs
        //
        T::Comment => Style::new()
            .fg(C::Rgb(92, 99, 112)) // #5c6370
            .add_modifier(Modifier::ITALIC),

        //
        // Keywords
        //
        T::Keyword | T::SelfKeyword | T::SelfTypeKeyword => Style::new().fg(C::Rgb(198, 120, 221)), // #c678dd

        //
        // Strings & Characters
        //
        T::Character | T::String => Style::new().fg(C::Rgb(152, 195, 121)), // #98c379

        //
        // Booleans
        //
        T::Boolean => Style::new().fg(C::Rgb(209, 154, 102)), // #d19a66

        //
        // Numbers
        //
        T::EnumMember | T::Float | T::Number | T::Variant => Style::new().fg(C::Rgb(209, 154, 102)),

        //
        // Escape / Format
        //
        T::EscapeSequence | T::FormatSpecifier => Style::new()
            .fg(C::Rgb(209, 154, 102))
            .add_modifier(Modifier::BOLD),

        //
        // Functions & Methods
        //
        T::Function | T::Method => Style::new().fg(C::Rgb(97, 175, 239)), // #61afef

        //
        // Macros / Attributes
        //
        T::Attribute
        | T::AttributeBracket
        | T::BuiltinAttribute
        | T::Decorator
        | T::Derive
        | T::DeriveHelper
        | T::Label
        | T::Macro
        | T::MacroBang
        | T::ProcMacro => Style::new().fg(C::Rgb(224, 108, 117)), // #e06c75

        //
        // Types
        //
        T::BuiltinType
        | T::ConstParameter
        | T::Enum
        | T::Generic
        | T::Interface
        | T::Namespace
        | T::Struct
        | T::ToolModule
        | T::Type
        | T::TypeAlias
        | T::TypeParameter
        | T::Union => Style::new().fg(C::Rgb(229, 192, 123)), // #e5c07b

        //
        // Lifetimes & Parameters
        //
        T::Lifetime | T::Parameter => Style::new().fg(C::Rgb(156, 164, 185)), // #9ca4b9

        //
        // Variables / Neutral
        //
        T::Const | T::Field | T::Injected | T::Property | T::Static | T::Variable => Style::new(),

        //
        // Errors
        //
        T::InvalidEscapeSequence | T::UnresolvedReference => Style::new()
            .fg(C::Rgb(224, 108, 117))
            .add_modifier(Modifier::UNDERLINED),

        //
        // Punctuation fallback
        //
        _ => Style::new().fg(C::Rgb(171, 178, 191)), // #abb2bf
    }
}

/// Diagnostic underlines (One Dark)
pub fn style_for_severity(severity: Option<Severity>) -> Style {
    use Color as C;
    use Severity as S;

    let color = match severity.unwrap_or(S::Error) {
        S::Error => C::Rgb(224, 108, 117),      // red
        S::Warning => C::Rgb(229, 192, 123),    // yellow
        S::Information => C::Rgb(97, 175, 239), // blue
        S::Hint => C::Rgb(92, 99, 112),         // gray
    };

    Style::new()
        .underline_color(color)
        .add_modifier(Modifier::UNDERLINED)
}

/// Modifier rendering (unchanged policy)
fn modifier_for(kind: TokenModifier) -> Modifier {
    use TokenModifier as M;

    match kind {
        M::Declaration | M::Definition => Modifier::BOLD,
        M::Deprecated => Modifier::CROSSED_OUT,
        M::Mutable => Modifier::UNDERLINED,
        M::Async => Modifier::ITALIC,
        M::Injected | M::IntraDocLink => Modifier::ITALIC,
        _ => Modifier::empty(),
    }
}
