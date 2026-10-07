//! The five LSP methods v1 speaks, as JSON in and JSON out.
//!
//! Everything here is a pure function of its arguments, which is the point:
//! the whole protocol layer is testable without a server, and the tests are
//! golden snapshots of the exact bytes that go on the wire. The transport below
//! never inspects a message, and the state machine above never builds one.
//!
//! Only `serde_json::Value` is used — no `lsp-types`. That crate would pull
//! `url` and friends for five methods we already have written, and would move
//! the interesting part (what we *do* with a response) somewhere else anyway.

use serde_json::{Value, json};

/// The one *server-initiated request* this client answers: the server has
/// finished (re)computing something and wants us to ask for colors again.
///
/// It is a request, not a notification — it carries an id and expects a
/// response — which is the whole reason it is easy to miss: a client that
/// dispatches on "has id ⇒ look it up in the pending table" drops it silently,
/// and the file stays half-colored until the next edit.
pub const METHOD_SEMANTIC_REFRESH: &str = "workspace/semanticTokens/refresh";

/// The progress notification: what the server is busy with while it indexes.
pub const METHOD_PROGRESS: &str = "$/progress";

/// Establishes the token a [`METHOD_PROGRESS`] notification will report on.
///
/// Another request, not a notification — it arrives with an id and expects an
/// answer. rust-analyzer sends it a dozen times during a cold start.
pub const METHOD_CREATE_PROGRESS: &str = "window/workDoneProgress/create";

/// The diagnostic counterpart of [`METHOD_SEMANTIC_REFRESH`]: the server has
/// redone its analysis and wants clients to drop what they are showing.
///
/// Nothing to do about it here — diagnostics are *pushed* to us — but it is a
/// request like the other two and is answered as one.
pub const METHOD_DIAGNOSTIC_REFRESH: &str = "workspace/diagnostic/refresh";

/// Every server-initiated request this client knows how to answer, in the
/// order they are checked.
///
/// Each one is answered with [`null_response`]; only
/// [`METHOD_SEMANTIC_REFRESH`] asks for anything more.
pub const SERVER_REQUESTS: [&str; 3] = [
    METHOD_SEMANTIC_REFRESH,
    METHOD_DIAGNOSTIC_REFRESH,
    METHOD_CREATE_PROGRESS,
];

/// What a token *is*: a `semanticTokenType` from the server's legend.
///
/// The wire name of each variant lives in [`TokenType::NAMES`] — the one place
/// the vocabulary is spelled out. [`TokenType::names`] feeds the `initialize`
/// capability, and [`TokenType::parse`] reads a server's legend, so a name we
/// have never heard of is an `Option`, not a string that silently matches no
/// arm. Adding a variant is only half the work on purpose: the theme's `match`
/// is exhaustive, so the compiler insists on being told what the new type
/// looks like on screen.
///
/// The list is rust-analyzer's own legend, captured from a real handshake,
/// plus the handful of names the LSP spec and other servers use (`float`,
/// `field`, `variant`, `paren`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Angle,
    Arithmetic,
    Attribute,
    AttributeBracket,
    Bitwise,
    Boolean,
    Brace,
    Bracket,
    BuiltinAttribute,
    BuiltinType,
    Character,
    Colon,
    Comma,
    Comment,
    Comparison,
    Const,
    ConstParameter,
    Decorator,
    Derive,
    DeriveHelper,
    Dot,
    Enum,
    EnumMember,
    EscapeSequence,
    Field,
    Float,
    FormatSpecifier,
    Function,
    Generic,
    Injected,
    Interface,
    InvalidEscapeSequence,
    Keyword,
    Label,
    Lifetime,
    Logical,
    Macro,
    MacroBang,
    Method,
    Namespace,
    Negation,
    Number,
    Operator,
    Paren,
    Parenthesis,
    Parameter,
    ProcMacro,
    Property,
    Punctuation,
    Semicolon,
    SelfKeyword,
    SelfTypeKeyword,
    Static,
    String,
    Struct,
    ToolModule,
    Type,
    TypeAlias,
    TypeParameter,
    Union,
    UnresolvedReference,
    Variable,
    Variant,
}

impl TokenType {
    /// `(wire name, variant)`, alphabetical by name so the two sides can be
    /// read against each other. Every variant appears exactly once: a variant
    /// missing here is never declared to the server and never matched, which
    /// shows up as a dead-code warning rather than as a token without a color.
    const NAMES: &'static [(&'static str, TokenType)] = &[
        ("angle", TokenType::Angle),
        ("arithmetic", TokenType::Arithmetic),
        ("attribute", TokenType::Attribute),
        ("attributeBracket", TokenType::AttributeBracket),
        ("bitwise", TokenType::Bitwise),
        ("boolean", TokenType::Boolean),
        ("brace", TokenType::Brace),
        ("bracket", TokenType::Bracket),
        ("builtinAttribute", TokenType::BuiltinAttribute),
        ("builtinType", TokenType::BuiltinType),
        ("character", TokenType::Character),
        ("colon", TokenType::Colon),
        ("comma", TokenType::Comma),
        ("comment", TokenType::Comment),
        ("comparison", TokenType::Comparison),
        ("const", TokenType::Const),
        ("constParameter", TokenType::ConstParameter),
        ("decorator", TokenType::Decorator),
        ("derive", TokenType::Derive),
        ("deriveHelper", TokenType::DeriveHelper),
        ("dot", TokenType::Dot),
        ("enum", TokenType::Enum),
        ("enumMember", TokenType::EnumMember),
        ("escapeSequence", TokenType::EscapeSequence),
        ("field", TokenType::Field),
        ("float", TokenType::Float),
        ("formatSpecifier", TokenType::FormatSpecifier),
        ("function", TokenType::Function),
        ("generic", TokenType::Generic),
        ("injected", TokenType::Injected),
        ("interface", TokenType::Interface),
        ("invalidEscapeSequence", TokenType::InvalidEscapeSequence),
        ("keyword", TokenType::Keyword),
        ("label", TokenType::Label),
        ("lifetime", TokenType::Lifetime),
        ("logical", TokenType::Logical),
        ("macro", TokenType::Macro),
        ("macroBang", TokenType::MacroBang),
        ("method", TokenType::Method),
        ("namespace", TokenType::Namespace),
        ("negation", TokenType::Negation),
        ("number", TokenType::Number),
        ("operator", TokenType::Operator),
        ("paren", TokenType::Paren),
        ("parenthesis", TokenType::Parenthesis),
        ("parameter", TokenType::Parameter),
        ("procMacro", TokenType::ProcMacro),
        ("property", TokenType::Property),
        ("punctuation", TokenType::Punctuation),
        ("semicolon", TokenType::Semicolon),
        ("selfKeyword", TokenType::SelfKeyword),
        ("selfTypeKeyword", TokenType::SelfTypeKeyword),
        ("static", TokenType::Static),
        ("string", TokenType::String),
        ("struct", TokenType::Struct),
        ("toolModule", TokenType::ToolModule),
        ("type", TokenType::Type),
        ("typeAlias", TokenType::TypeAlias),
        ("typeParameter", TokenType::TypeParameter),
        ("union", TokenType::Union),
        ("unresolvedReference", TokenType::UnresolvedReference),
        ("variable", TokenType::Variable),
        ("variant", TokenType::Variant),
    ];

    /// The names as a server expects them, for the `initialize` capability.
    pub fn names() -> impl Iterator<Item = &'static str> {
        Self::NAMES.iter().map(|(name, _)| *name)
    }

    /// Reads one legend entry. `None` for a name this build does not know:
    /// the caller keeps the legend slot (indices are what tokens carry) and
    /// paints that type plain.
    pub fn parse(name: &str) -> Option<TokenType> {
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, kind)| *kind)
    }
}

/// What a token *is like*: a `semanticTokenModifier` from the server's legend.
///
/// Unlike a type, an unrecognized modifier costs nothing — modifiers only add
/// attributes (bold, italic), never a color — so an unknown one is dropped
/// rather than reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenModifier {
    Async,
    Associated,
    Attribute,
    Callable,
    Constant,
    Consuming,
    ControlFlow,
    CrateRoot,
    Declaration,
    DefaultLibrary,
    Definition,
    Deprecated,
    Documentation,
    Injected,
    IntraDocLink,
    Library,
    Macro,
    Mutable,
    ProcMacro,
    Public,
    Readonly,
    Reference,
    Static,
    Trait,
    Unsafe,
}

impl TokenModifier {
    const NAMES: &'static [(&'static str, TokenModifier)] = &[
        ("async", TokenModifier::Async),
        ("associated", TokenModifier::Associated),
        ("attribute", TokenModifier::Attribute),
        ("callable", TokenModifier::Callable),
        ("constant", TokenModifier::Constant),
        ("consuming", TokenModifier::Consuming),
        ("controlFlow", TokenModifier::ControlFlow),
        ("crateRoot", TokenModifier::CrateRoot),
        ("declaration", TokenModifier::Declaration),
        ("defaultLibrary", TokenModifier::DefaultLibrary),
        ("definition", TokenModifier::Definition),
        ("deprecated", TokenModifier::Deprecated),
        ("documentation", TokenModifier::Documentation),
        ("injected", TokenModifier::Injected),
        ("intraDocLink", TokenModifier::IntraDocLink),
        ("library", TokenModifier::Library),
        ("macro", TokenModifier::Macro),
        ("mutable", TokenModifier::Mutable),
        ("procMacro", TokenModifier::ProcMacro),
        ("public", TokenModifier::Public),
        ("readonly", TokenModifier::Readonly),
        ("reference", TokenModifier::Reference),
        ("static", TokenModifier::Static),
        ("trait", TokenModifier::Trait),
        ("unsafe", TokenModifier::Unsafe),
    ];

    pub fn names() -> impl Iterator<Item = &'static str> {
        Self::NAMES.iter().map(|(name, _)| *name)
    }

    pub fn parse(name: &str) -> Option<TokenModifier> {
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, kind)| *kind)
    }
}

/// `initialize`: the only request whose answer changes what we can do.
///
/// Two of these fields are load-bearing:
///
/// - `multilineTokenSupport: false` — without it a token may straddle a
///   newline, and the per-row model of the coloring layer has nowhere to put
///   half a run.
/// - `formats: ["relative"]` — the simpler of the two encodings.
pub fn initialize(id: u64, root_uri: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "semanticTokens": {
                        "requests": { "full": { "delta": false } },
                        "tokenTypes": TokenType::names().collect::<Vec<_>>(),
                        "tokenModifiers": TokenModifier::names().collect::<Vec<_>>(),
                        "formats": ["relative"],
                        "overlappingTokenSupport": false,
                        "multilineTokenSupport": false
                    },
                    "synchronization": {
                        "dynamicRegistration": false,
                        "didSave": false
                    },
                    "publishDiagnostics": {
                        "relatedInformation": false,
                        "versionSupport": true
                    }
                },
                // Without this the server never sends
                // `workspace/semanticTokens/refresh`, and a file opened while
                // the project is still being indexed keeps the partial colors
                // of that moment until the buffer is edited again.
                "workspace": {
                    "semanticTokens": { "refreshSupport": true }
                },
                // Without this the server stays silent about what it is doing:
                // no `$/progress`, so no way to tell a slow first index from a
                // broken coloring layer.
                "window": {
                    "workDoneProgress": true
                }
            }
        }
    })
}

/// `initialized`: the notification that releases the server to start work.
pub fn initialized() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    })
}

/// `textDocument/didOpen`: hands the server a document it has never seen.
pub fn did_open(uri: &str, language_id: &str, version: u64, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": version,
                "text": text
            }
        }
    })
}

/// `textDocument/didChange`: the whole document, every time.
///
/// The fallback, used for a server that did not ask for incremental sync and
/// for the first change after one that could not be described. Always correct,
/// and only costs bytes — which is why [`did_change_range`] exists.
pub fn did_change(uri: &str, version: u64, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        }
    })
}

/// `textDocument/didChange`: one range replaced, for a server that asked for
/// incremental sync.
///
/// `range` is the span in the document *the server holds* — see
/// [`row_range`] — and `text` is what takes its place. Typing one character
/// therefore sends those two instead of the file.
pub fn did_change_range(uri: &str, version: u64, range: &Value, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "range": range, "text": text }]
        }
    })
}

/// `TextDocumentSyncKind.Incremental`, the value that buys [`did_change_range`].
const SYNC_INCREMENTAL: u64 = 2;

/// Whether the server asked for incremental document sync, which it does by
/// declaring `textDocumentSync` as `2` — bare, or as `{"change": 2}`.
///
/// A server that did not ask gets the whole document: sending a range to one
/// that expects the full text would leave it holding a file that is neither
/// what it had nor what we have, and every token after that would be wrong.
pub fn prefers_incremental_sync(response: &Value) -> bool {
    let kind = response
        .get("result")
        .and_then(|result| result.get("capabilities"))
        .and_then(|capabilities| capabilities.get("textDocumentSync"))
        .and_then(|sync| sync.as_u64().or_else(|| sync.get("change")?.as_u64()));

    kind == Some(SYNC_INCREMENTAL)
}

/// The rows of `document` an incremental change replaces: rows
/// `[first, last)` give way to the caller's rows `[first, last_new)`.
///
/// The range runs from the start of row `first` to the start of row `last` —
/// the row *after* the changed block. That means it covers the newline after
/// every removed row. Naming the previous row's end instead would leave that
/// newline in place, merging the changed block with the row below on an
/// insertion or a multi-row replacement (the bug that dropped every token after
/// an edit).
pub fn row_range(document: &[String], first: usize, last: usize) -> Option<Value> {
    // No baseline row to anchor a range against: the caller falls back to the
    // whole document. (`changed_rows` can ask for the empty range when the
    // server holds nothing yet.)
    if document.is_empty() {
        return None;
    }
    if last > document.len() {
        return None;
    }
    Some(json!({
        "start": { "line": first, "character": 0 },
        "end": { "line": last, "character": 0 }
    }))
}

/// Which rows no longer match: `[first, last_old)` of `old` is replaced by
/// `[first, last_new)` of `new`.
///
/// Matched from both ends, so a change in the middle of a file is one row even
/// when the row count moved — and `None` when the two agree, which is the case
/// where there is nothing to send at all.
pub fn changed_rows(old: &[String], new: &[String]) -> Option<(usize, usize, usize)> {
    let mut first = 0;
    while first < old.len() && first < new.len() && old[first] == new[first] {
        first += 1;
    }

    let mut last_old = old.len();
    let mut last_new = new.len();
    while last_old > first && last_new > first && old[last_old - 1] == new[last_new - 1] {
        last_old -= 1;
        last_new -= 1;
    }

    // Nothing moved: the two agree all the way down.
    (last_old > first || last_new > first).then_some((first, last_old, last_new))
}

/// `textDocument/semanticTokens/full`: the request that produces colors.
pub fn semantic_tokens_full(id: u64, uri: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/semanticTokens/full",
        "params": { "textDocument": { "uri": uri } }
    })
}

/// The answer to any of [`SERVER_REQUESTS`].
///
/// It says nothing — `result: null` — but it must be sent: the server is
/// entitled to wait for it, and for [`METHOD_SEMANTIC_REFRESH`] the asking is
/// the point, which the caller does separately.
pub fn null_response(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": null
    })
}

/// `textDocument/didClose`: the document is no longer being edited.
pub fn did_close(uri: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didClose",
        "params": { "textDocument": { "uri": uri } }
    })
}

/// `shutdown`: the polite half of quitting.
pub fn shutdown(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "shutdown"
    })
}

/// `exit`: the other half. A server may exit without answering `shutdown`, so
/// this is always sent and never waited on.
pub fn exit() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    })
}

/// The `legend` half of an `initialize` result: the mapping from index to
/// meaning that every later token refers to.
///
/// One slot per index, and the slots are `Option`s on purpose: a server is free
/// to name a type this build has never heard of, and its slot has to stay
/// (indices are what the token stream carries) even though there is nothing to
/// map it to. Unknown *modifiers* are dropped the same way — they only ever
/// add an attribute, so losing one costs nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Legend {
    /// Bit *i* of a token's modifier set means `token_modifiers[i]`.
    pub token_types: Vec<Option<TokenType>>,
    pub token_modifiers: Vec<Option<TokenModifier>>,
    /// The token-type names that did not resolve, in legend order. Kept so the
    /// gap can be named once instead of hunted for on screen.
    pub unknown_types: Vec<String>,
}

impl Legend {
    /// Whether the server can color at all. A server without a legend is
    /// treated as a server that should not have been started for this.
    pub fn is_empty(&self) -> bool {
        self.token_types.is_empty()
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the legend out of an `initialize` response.
///
/// `None` means "no usable semantic tokens": the response was an error, or the
/// server does not advertise the capability. Either way the caller shuts the
/// coloring path down rather than retrying.
pub fn parse_initialize_result(response: &Value) -> Option<Legend> {
    let legend = response
        .get("result")?
        .get("capabilities")?
        .get("semanticTokensProvider")?
        .get("legend")?;

    // Parsing here rather than at use: past this point nothing in the editor
    // compares token types by string.
    let names = strings(legend.get("tokenTypes"));
    let unknown_types: Vec<String> = names
        .iter()
        .filter(|name| TokenType::parse(name).is_none())
        .cloned()
        .collect();

    Some(Legend {
        token_types: names.iter().map(|name| TokenType::parse(name)).collect(),
        token_modifiers: strings(legend.get("tokenModifiers"))
            .iter()
            .map(|name| TokenModifier::parse(name))
            .collect(),
        unknown_types,
    })
}

/// How bad a diagnostic is: the `severity` field, `1..4` on the wire.
///
/// Absent means the server left it to the client, which the spec says to treat
/// as `Error` — but we only ever turn it into an underline color, so "unset" is
/// kept as `None` and rendered as the most visible one rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl Severity {
    fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::Error),
            2 => Some(Self::Warning),
            3 => Some(Self::Information),
            4 => Some(Self::Hint),
            _ => None,
        }
    }
}

/// One `Diagnostic`: a range plus how bad it is. The `message` is dropped — a
/// terminal editor with no hover surface has nowhere to put it, and keeping it
/// would mean storing a `String` per diagnostic per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostic {
    pub start_line: usize,
    pub end_line: usize,
    /// UTF-16 columns, the LSP position unit — converted at decode time.
    pub start_char: usize,
    pub end_char: usize,
    pub severity: Option<Severity>,
}

fn position(value: &Value, field: &str) -> Option<usize> {
    value.get(field).and_then(Value::as_u64).map(|n| n as usize)
}

fn diagnostic(item: &Value) -> Option<Diagnostic> {
    let range = item.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;

    Some(Diagnostic {
        start_line: position(start, "line")?,
        end_line: position(end, "line")?,
        start_char: position(start, "character")?,
        end_char: position(end, "character")?,
        // A severity outside 1..4 is dropped rather than clamped: which of the
        // four it "meant" is a guess, and the underline still shows.
        severity: item
            .get("severity")
            .and_then(Value::as_u64)
            .and_then(Severity::from_u64),
    })
}

/// Reads a `textDocument/publishDiagnostics` notification.
///
/// `None` for anything that is not one — including a well-formed one for a
/// file the editor is not showing, which the caller decides. A malformed item
/// in the list is dropped rather than failing the whole batch: the point of a
/// diagnostic is the ones that *did* parse.
pub fn parse_publish_diagnostics(notification: &Value) -> Option<(&str, Vec<Diagnostic>)> {
    if server_method(notification)? != "textDocument/publishDiagnostics" {
        return None;
    }

    let params = notification.get("params")?;
    let uri = params.get("uri")?.as_str()?;

    let items = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(diagnostic).collect())
        .unwrap_or_default();

    Some((uri, items))
}

/// One running job: whatever `$/progress` has last said about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Job {
    /// The progress token, e.g. `rustAnalyzer/Indexing`.
    token: String,
    /// The job's name — `title` on `begin`, `message` on `report`.
    label: String,
    percentage: Option<u8>,
}

/// What the server is busy with, as [`METHOD_PROGRESS`] reports it.
///
/// Jobs are tracked as a set, not one at a time: a cold start runs fetching,
/// crate-graph building, proc-macro loading and indexing at once, and one of
/// them ending must not blank the read-out of the others. The most recently
/// heard-from job is the one shown, because that is the one that moved.
///
/// Both strings are buffers reused in place: an indexing run emits hundreds of
/// `report` notifications, and reusing the allocations keeps them from becoming
/// hundreds of strings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Running jobs, least recently heard from first.
    jobs: Vec<Job>,
}

impl Progress {
    /// Whether anything is running. `false` means the read-out is clear.
    pub fn is_active(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The read-out for the status bar, or `None` when nothing is running.
    ///
    /// This allocates, so it is only asked for when a progress notification
    /// arrives — never per frame.
    pub fn text(&self) -> Option<String> {
        let job = self.jobs.last()?;

        Some(match (job.label.is_empty(), job.percentage) {
            (true, None) => "working".to_string(),
            (true, Some(percentage)) => format!("{percentage}%"),
            (false, None) => job.label.clone(),
            (false, Some(percentage)) => format!("{} {percentage}%", job.label),
        })
    }

    /// Bytes the shown job's label buffer can hold.
    ///
    /// Only the tests care: it is how they prove a long run of reports writes
    /// into one allocation instead of making one per report.
    #[doc(hidden)]
    pub fn label_capacity(&self) -> usize {
        self.jobs.last().map_or(0, |job| job.label.capacity())
    }

    /// The job for `token`, moved to the end so it is the one shown. Creates
    /// it when it has not been heard of before — a `report` can arrive without
    /// a `begin` in front of it.
    fn job(&mut self, token: &str) -> &mut Job {
        if let Some(at) = self.jobs.iter().position(|job| job.token == token) {
            let job = self.jobs.remove(at);
            self.jobs.push(job);
        } else {
            self.jobs.push(Job {
                token: token.to_string(),
                ..Job::default()
            });
        }

        self.jobs.last_mut().expect("just pushed or moved one")
    }
}

/// Reads a [`METHOD_PROGRESS`] notification into `into`.
///
/// `false` for anything else, including a progress notification whose `kind`
/// this build does not know. A `begin` starts a job, a `report` updates it and
/// an `end` removes it — which is what makes the status bar go quiet again.
pub fn parse_progress(notification: &Value, into: &mut Progress) -> bool {
    if server_method(notification) != Some(METHOD_PROGRESS) {
        return false;
    }

    let Some(params) = notification.get("params") else {
        return false;
    };
    let Some(value) = params.get("value") else {
        return false;
    };
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return false;
    };
    let token = params.get("token").and_then(Value::as_str).unwrap_or("");

    match kind {
        "end" => {
            if let Some(at) = into.jobs.iter().position(|job| job.token == token) {
                into.jobs.remove(at);
            }
        }
        "begin" | "report" => {
            let job = into.job(token);
            // `begin` carries a `title`, `report` only a `message`; taking
            // whichever is present keeps the label from blanking out on every
            // report that fails to repeat the title.
            if let Some(text) = value
                .get("title")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
            {
                job.label.clear();
                job.label.push_str(text);
            }
            job.percentage = value
                .get("percentage")
                .and_then(Value::as_u64)
                .map(|n| n.min(100) as u8);
        }
        _ => return false,
    }

    true
}

/// Reads the raw token stream out of a `semanticTokens` response.
///
/// Groups of five: `deltaLine`, `deltaStartChar`, `length`, `tokenType`,
/// `tokenModifiers`. A response with a `result` but no `data` is a server that
/// answered with nothing to say; that is `None` too.
pub fn parse_semantic_tokens(response: &Value) -> Option<Vec<u32>> {
    let data = response.get("result")?.get("data")?.as_array()?;

    Some(
        data.iter()
            .filter_map(|item| item.as_u64().map(|n| n as u32))
            .collect(),
    )
}

/// A legend captured verbatim from a live `initialize` against
/// rust-analyzer 1.99.0, in the server's own order.
///
/// Tests use it so the mapping and the decoder are exercised against the names
/// and indices a real server actually sends, not against a plausible-looking
/// example. A future server that adds a name shows up as a failing test here
/// rather than as a token that is quietly never colored.
#[cfg(test)]
pub const REAL_LEGEND: &[&str] = &[
    "comment",
    "decorator",
    "enumMember",
    "enum",
    "function",
    "interface",
    "keyword",
    "macro",
    "method",
    "namespace",
    "number",
    "operator",
    "parameter",
    "property",
    "string",
    "struct",
    "typeParameter",
    "variable",
    "type",
    "label",
    "angle",
    "arithmetic",
    "attributeBracket",
    "attribute",
    "bitwise",
    "boolean",
    "brace",
    "bracket",
    "builtinAttribute",
    "builtinType",
    "character",
    "colon",
    "comma",
    "comparison",
    "constParameter",
    "const",
    "deriveHelper",
    "derive",
    "dot",
    "escapeSequence",
    "formatSpecifier",
    "generic",
    "invalidEscapeSequence",
    "lifetime",
    "logical",
    "macroBang",
    "negation",
    "parenthesis",
    "procMacro",
    "punctuation",
    "selfKeyword",
    "selfTypeKeyword",
    "semicolon",
    "static",
    "toolModule",
    "typeAlias",
    "union",
    "unresolvedReference",
];

/// Builds a legend the way the parser would, from names instead of JSON. Exposed
/// for tests only: a test that wants a three-type legend should not have to
/// spell out a `Vec<Option<_>>`.
#[cfg(test)]
pub fn legend_for_test(types: &[&str], modifiers: &[&str]) -> Legend {
    let unknown_types = types
        .iter()
        .filter(|name| TokenType::parse(name).is_none())
        .map(|name| name.to_string())
        .collect();

    Legend {
        token_types: types.iter().map(|name| TokenType::parse(name)).collect(),
        token_modifiers: modifiers
            .iter()
            .map(|name| TokenModifier::parse(name))
            .collect(),
        unknown_types,
    }
}

/// The request id a response belongs to, if it is a response.
pub fn response_id(message: &Value) -> Option<u64> {
    message.get("id").and_then(Value::as_u64)
}

/// The method of a server-initiated message (a notification or a request).
pub fn server_method(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

/// The `error` object of a failed response, as text.
pub fn response_error(message: &Value) -> Option<String> {
    message.get("error").map(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::Diagnostic;
    use super::{
        METHOD_CREATE_PROGRESS, METHOD_DIAGNOSTIC_REFRESH, METHOD_PROGRESS,
        METHOD_SEMANTIC_REFRESH, Progress, REAL_LEGEND, SERVER_REQUESTS, Severity, TokenModifier,
        TokenType, changed_rows, did_change, did_change_range, did_close, did_open, exit,
        null_response, parse_initialize_result, parse_progress, parse_publish_diagnostics,
        parse_semantic_tokens, prefers_incremental_sync, response_error, response_id, row_range,
        semantic_tokens_full, server_method, shutdown,
    };
    use serde_json::json;

    /// The names `initialize` declares, as the wire carries them.
    ///
    /// Comparing against the enum's own table rather than a literal list is
    /// what keeps the two from drifting: the test can only pass if whatever is
    /// sent is exactly what [`TokenType::parse`] accepts.
    fn declared() -> Vec<&'static str> {
        TokenType::names().collect()
    }

    fn declared_modifiers() -> Vec<&'static str> {
        TokenModifier::names().collect()
    }

    #[test]
    fn initialize_declares_the_capabilities_that_shape_the_wire_format() {
        let msg = super::initialize(1, "file:///c%3A/proj");
        assert_eq!(msg["id"], 1);
        assert_eq!(msg["method"], "initialize");

        let params = &msg["params"];
        assert_eq!(params["rootUri"], "file:///c%3A/proj");
        // `processId` lets the server exit when we do.
        assert!(params["processId"].is_u64());

        let tokens = &params["capabilities"]["textDocument"]["semanticTokens"];
        assert_eq!(tokens["formats"][0], "relative");
        assert_eq!(tokens["multilineTokenSupport"], false);
        assert_eq!(tokens["overlappingTokenSupport"], false);
        assert_eq!(tokens["requests"]["full"]["delta"], false);

        // The declared names are exactly the enum's vocabulary, in order.
        let types: Vec<&str> = tokens["tokenTypes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(types, declared());
        let modifiers: Vec<&str> = tokens["tokenModifiers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(modifiers, declared_modifiers());
    }

    #[test]
    fn initialize_asks_the_server_to_tell_us_when_to_re_ask() {
        // Without `refreshSupport` the server stays silent about finishing its
        // indexing, and a file opened too early keeps its early, partial
        // colors until the buffer happens to be edited.
        let msg = super::initialize(1, "file:///c%3A/proj");
        let workspace = &msg["params"]["capabilities"]["workspace"];
        assert_eq!(workspace["semanticTokens"]["refreshSupport"], true);
    }

    #[test]
    fn initialize_asks_for_progress_reports() {
        // Without `workDoneProgress` a real rust-analyzer sends no `$/progress`
        // at all, so a slow first index looks exactly like broken coloring.
        let msg = super::initialize(1, "file:///c%3A/proj");
        let window = &msg["params"]["capabilities"]["window"];
        assert_eq!(window["workDoneProgress"], true);
    }

    #[test]
    fn every_server_request_is_answered_with_nothing() {
        // The point of the message is that it arrives; there is no result to
        // carry. Asking for tokens again is the caller's job.
        let msg = null_response(7);
        assert_eq!(msg["id"], 7);
        assert!(msg["result"].is_null());
    }

    #[test]
    fn the_method_names_are_the_ones_the_server_uses() {
        // All of them are recognised by string comparison; a typo there is a
        // request that is silently dropped rather than a compile error.
        assert_eq!(METHOD_SEMANTIC_REFRESH, "workspace/semanticTokens/refresh");
        assert_eq!(METHOD_DIAGNOSTIC_REFRESH, "workspace/diagnostic/refresh");
        assert_eq!(METHOD_CREATE_PROGRESS, "window/workDoneProgress/create");
        assert_eq!(METHOD_PROGRESS, "$/progress");
        assert_eq!(SERVER_REQUESTS.len(), 3);
    }

    #[test]
    fn did_open_carries_the_language_and_the_whole_text() {
        let msg = did_open("file:///a.rs", "rust", 1, "fn main() {}");
        assert_eq!(msg["method"], "textDocument/didOpen");
        let doc = &msg["params"]["textDocument"];
        assert_eq!(doc["uri"], "file:///a.rs");
        assert_eq!(doc["languageId"], "rust");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["text"], "fn main() {}");
    }

    #[test]
    fn did_change_sends_one_full_text_change() {
        let msg = did_change("file:///a.rs", 7, "fn main() {}\n");
        assert_eq!(msg["method"], "textDocument/didChange");
        assert_eq!(msg["params"]["textDocument"]["version"], 7);

        let changes = msg["params"]["contentChanges"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["text"], "fn main() {}\n");
        // Full sync: no range, which is what makes it full.
        assert!(changes[0].get("range").is_none());
    }

    fn doc(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    #[test]
    fn did_change_range_names_the_range_it_replaces() {
        let range = row_range(&doc("a\nb\nc"), 1, 2).expect("a range");
        let msg = did_change_range("file:///a.rs", 7, &range, "bee");

        assert_eq!(msg["params"]["textDocument"]["version"], 7);
        let changes = msg["params"]["contentChanges"].as_array().unwrap();
        assert_eq!(changes[0]["text"], "bee");
        assert_eq!(
            changes[0]["range"]["start"],
            json!({ "line": 1, "character": 0 })
        );
    }

    /// The range runs to the start of the row *after* the changed block, so it
    /// covers the newline after every removed row. Stopping at the end of the
    /// last covered row instead would leave that newline in place and merge the
    /// block with the row below.
    #[test]
    fn a_row_range_runs_to_the_start_of_the_next_row() {
        let range = row_range(&doc("a\nb\nc"), 1, 3).expect("a range");
        assert_eq!(range["start"], json!({ "line": 1, "character": 0 }));
        assert_eq!(range["end"], json!({ "line": 3, "character": 0 }));
    }

    #[test]
    fn a_row_range_ends_at_column_zero() {
        // The character is always zero: the range boundary is a row start, never
        // a mid-row column, so UTF-16 widths never enter the picture.
        let range = row_range(&doc("你好\nb"), 0, 1).expect("a range");
        assert_eq!(range["end"]["character"], 0);
    }

    #[test]
    fn a_row_range_covering_nothing_is_not_a_range() {
        // No baseline row to name the end with.
        assert!(row_range(&[], 0, 0).is_none());
        // Past the end of the document there is no row to start from.
        assert!(row_range(&doc("a"), 0, 3).is_none());
    }

    /// A row inserted in the middle names no old row, so the range is
    /// zero-width at the boundary it lands on — not the end of the row above.
    /// Getting this wrong drops the new text into the wrong place.
    #[test]
    fn a_row_inserted_in_the_middle_is_a_zero_width_range() {
        let range = row_range(&doc("a\nb\nc"), 2, 2).expect("an insertion range");
        assert_eq!(range["start"], json!({ "line": 2, "character": 0 }));
        assert_eq!(range["end"], json!({ "line": 2, "character": 0 }));
    }

    #[test]
    fn a_change_in_one_row_spares_the_others() {
        let old = doc("a\nb\nc");
        let new = doc("a\nbee\nc");

        // One row changed, so one row is sent — even though the row it names
        // ends at a different column than it did.
        assert_eq!(changed_rows(&old, &new), Some((1, 2, 2)));
    }

    #[test]
    fn an_unchanged_document_has_nothing_to_send() {
        let old = doc("a\nb\nc");
        assert_eq!(changed_rows(&old, &old), None);
        assert_eq!(changed_rows(&[], &[]), None);
    }

    /// Rows are matched from both ends, so adding a row in the middle does not
    /// drag the whole tail along with it.
    #[test]
    fn rows_added_in_the_middle_are_one_change() {
        let old = doc("a\nb\nc");
        let new = doc("a\nb\nb2\nc");

        assert_eq!(changed_rows(&old, &new), Some((2, 2, 3)));
    }

    #[test]
    fn a_row_removed_at_the_end_names_no_new_row() {
        let old = doc("a\nb\nc");
        let new = doc("a\nb");

        assert_eq!(changed_rows(&old, &new), Some((2, 3, 2)));
    }

    #[test]
    fn without_a_baseline_every_row_is_new() {
        // The server holds nothing yet, so there is no range to name: the
        // caller falls back to the whole document.
        assert_eq!(changed_rows(&[], &doc("a\nb")), Some((0, 0, 2)));
        assert!(row_range(&[], 0, 0).is_none());
    }

    #[test]
    fn incremental_sync_is_read_both_ways_of_writing_it() {
        // The spec allows a bare kind and an object with a `change` in it.
        let bare = json!({ "result": { "capabilities": { "textDocumentSync": 2 } } });
        let nested =
            json!({ "result": { "capabilities": { "textDocumentSync": { "change": 2 } } } });
        let full = json!({ "result": { "capabilities": { "textDocumentSync": 1 } } });
        let absent = json!({ "result": { "capabilities": {} } });

        assert!(prefers_incremental_sync(&bare));
        assert!(prefers_incremental_sync(&nested));
        assert!(!prefers_incremental_sync(&full));
        assert!(!prefers_incremental_sync(&absent));
    }

    #[test]
    fn semantic_tokens_full_is_a_request_against_one_document() {
        let msg = semantic_tokens_full(42, "file:///a.rs");
        assert_eq!(msg["id"], 42);
        assert_eq!(msg["method"], "textDocument/semanticTokens/full");
        assert_eq!(msg["params"]["textDocument"]["uri"], "file:///a.rs");
    }

    #[test]
    fn notifications_carry_no_id() {
        // `initialized` and `exit` must not be answerable; a stray id would
        // make the server's response collide with a real request's.
        assert!(super::initialized().get("id").is_none());
        assert!(exit().get("id").is_none());
        assert_eq!(did_close("file:///a.rs")["method"], "textDocument/didClose");
        assert_eq!(shutdown(9)["id"], 9);
    }

    #[test]
    fn legend_is_read_out_of_the_initialize_result() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {
                    "semanticTokensProvider": {
                        "legend": {
                            "tokenTypes": ["keyword", "function"],
                            "tokenModifiers": ["declaration"]
                        }
                    }
                }
            }
        });

        let legend = parse_initialize_result(&response).expect("legend");
        assert_eq!(
            legend.token_types,
            [Some(TokenType::Keyword), Some(TokenType::Function)]
        );
        assert_eq!(legend.token_modifiers, [Some(TokenModifier::Declaration)]);
        assert!(legend.unknown_types.is_empty());
        assert!(!legend.is_empty());
    }

    #[test]
    fn a_name_this_build_does_not_know_keeps_its_slot() {
        // The reason the slots are `Option`s: indices are what the token
        // stream carries, so a gap cannot be closed up by dropping entries.
        let response = json!({
            "result": {
                "capabilities": {
                    "semanticTokensProvider": {
                        "legend": {
                            "tokenTypes": ["keyword", "somethingNew", "function"],
                            "tokenModifiers": ["whatever"]
                        }
                    }
                }
            }
        });

        let legend = parse_initialize_result(&response).expect("legend");
        assert_eq!(
            legend.token_types,
            [Some(TokenType::Keyword), None, Some(TokenType::Function)]
        );
        assert_eq!(legend.token_modifiers, [None]);
        // Reported once, so the gap has a name instead of being hunted for.
        assert_eq!(legend.unknown_types, ["somethingNew"]);
    }

    #[test]
    fn legend_is_read_out_of_a_real_initialize_response() {
        // The shape rust-analyzer 1.99.0 answers with, trimmed to the part we
        // read. The names and their order are the real ones: what matters is
        // that the parser reaches the legend through the real nesting.
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {
                    "semanticTokensProvider": {
                        "full": { "delta": false },
                        "range": false,
                        "legend": {
                            "tokenTypes": REAL_LEGEND,
                            "tokenModifiers": [
                                "async", "documentation", "declaration", "static",
                                "defaultLibrary", "deprecated", "associated",
                                "attribute", "callable", "constant", "consuming",
                                "controlFlow", "crateRoot", "injected",
                                "intraDocLink", "library", "macro", "mutable",
                                "procMacro", "public", "reference", "trait", "unsafe"
                            ]
                        }
                    },
                    "textDocumentSync": { "change": 2, "openClose": true },
                    "hoverProvider": true
                },
                "serverInfo": { "name": "rust-analyzer", "version": "1.99.0" }
            }
        });

        let legend = parse_initialize_result(&response).expect("legend");
        assert_eq!(legend.token_types.len(), 58);
        assert!(
            legend.unknown_types.is_empty(),
            "{:?}",
            legend.unknown_types
        );
        // Indices are what the token stream refers to, so the order has to
        // survive the parse exactly.
        assert_eq!(legend.token_types[6], Some(TokenType::Keyword));
        assert_eq!(legend.token_types[9], Some(TokenType::Namespace));
        assert_eq!(legend.token_types[15], Some(TokenType::Struct));
        assert_eq!(legend.token_modifiers[2], Some(TokenModifier::Declaration));
    }

    #[test]
    fn a_server_without_semantic_tokens_yields_no_legend() {
        let response = json!({ "result": { "capabilities": {} } });
        assert!(parse_initialize_result(&response).is_none());

        // An error response is not a legend either.
        let error = json!({ "error": { "code": -32000 } });
        assert!(parse_initialize_result(&error).is_none());
    }

    #[test]
    fn semantic_tokens_data_is_read_as_u32() {
        let response = json!({ "result": { "data": [0, 0, 2, 1, 0] } });
        assert_eq!(
            parse_semantic_tokens(&response).expect("data"),
            vec![0u32, 0, 2, 1, 0]
        );
    }

    #[test]
    fn a_result_without_data_is_not_a_token_set() {
        assert!(parse_semantic_tokens(&json!({ "result": {} })).is_none());
        assert!(parse_semantic_tokens(&json!({ "result": null })).is_none());
    }

    #[test]
    fn publish_diagnostics_is_read_into_ranges() {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///a.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 3, "character": 4 },
                            "end": { "line": 3, "character": 9 }
                        },
                        "severity": 1,
                        "message": "cannot find value"
                    },
                    {
                        "range": {
                            "start": { "line": 7, "character": 0 },
                            "end": { "line": 8, "character": 2 }
                        },
                        "message": "unused"
                    }
                ]
            }
        });

        let (uri, items) = parse_publish_diagnostics(&notification).expect("diagnostics");
        assert_eq!(uri, "file:///a.rs");
        assert_eq!(
            items,
            [
                Diagnostic {
                    start_line: 3,
                    end_line: 3,
                    start_char: 4,
                    end_char: 9,
                    severity: Some(Severity::Error),
                },
                // No `severity` is legal; it is kept as `None`, not guessed at.
                Diagnostic {
                    start_line: 7,
                    end_line: 8,
                    start_char: 0,
                    end_char: 2,
                    severity: None,
                }
            ]
        );
    }

    #[test]
    fn diagnostics_for_another_file_are_still_parsed() {
        // The URI is what lets the caller drop them; parsing itself is
        // file-agnostic.
        let notification = json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///other.rs", "diagnostics": [] }
        });

        let (uri, items) = parse_publish_diagnostics(&notification).expect("diagnostics");
        assert_eq!(uri, "file:///other.rs");
        assert!(items.is_empty());
    }

    #[test]
    fn anything_that_is_not_a_diagnostic_notification_is_not_one() {
        // An empty set is a real report: "this file is clean". `None` means
        // "this message is not a diagnostics notification".
        assert!(parse_publish_diagnostics(&json!({ "method": "$/progress" })).is_none());
        assert!(
            parse_publish_diagnostics(&json!({
                "method": "textDocument/publishDiagnostics",
                "params": { "diagnostics": [] }
            }))
            .is_none()
        ); // no uri
        // ...but a notification with an empty list is, and reports zero.
        let (_, items) = parse_publish_diagnostics(&json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///a.rs", "diagnostics": [] }
        }))
        .expect("diagnostics");
        assert!(items.is_empty());
    }

    #[test]
    fn a_malformed_diagnostic_is_dropped_not_fatal() {
        // One bad item must not cost the whole batch: the point of a batch is
        // the ones that did parse.
        let notification = json!({
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///a.rs",
                "diagnostics": [
                    { "message": "no range at all" },
                    {
                        "range": {
                            "start": { "line": 1, "character": 0 },
                            "end": { "line": 1, "character": 3 }
                        },
                        "severity": 9 // outside 1..4
                    }
                ]
            }
        });

        let (_, items) = parse_publish_diagnostics(&notification).expect("diagnostics");
        assert_eq!(items.len(), 1);
        // The severity is dropped rather than clamped to one of the four.
        assert_eq!(items[0].severity, None);
    }

    /// A `begin` / `report` / `end` notification as rust-analyzer sends it
    /// while it indexes a project: `token` names the job, `title`/`message`
    /// describe it.
    fn progress(
        token: &str,
        kind: &str,
        title: Option<&str>,
        message: Option<&str>,
        percentage: Option<u64>,
    ) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        value.insert("kind".to_string(), json!(kind));
        if let Some(title) = title {
            value.insert("title".to_string(), json!(title));
        }
        if let Some(message) = message {
            value.insert("message".to_string(), json!(message));
        }
        if let Some(percentage) = percentage {
            value.insert("percentage".to_string(), json!(percentage));
        }

        json!({
            "jsonrpc": "2.0",
            "method": METHOD_PROGRESS,
            "params": { "token": token, "value": value }
        })
    }

    /// The jobs the tests keep referring to; a cold start runs several at
    /// once.
    const INDEXING: &str = "rustAnalyzer/Indexing";
    const FETCHING: &str = "rustAnalyzer/Fetching";

    #[test]
    fn progress_runs_from_begin_to_end() {
        let mut view = Progress::default();
        assert!(!view.is_active());
        assert_eq!(view.text(), None);

        assert!(parse_progress(
            &progress(INDEXING, "begin", Some("Indexing"), None, Some(0)),
            &mut view
        ));
        assert!(view.is_active());
        assert_eq!(view.text().as_deref(), Some("Indexing 0%"));

        // A `report` carries a message, not a title; the label follows it.
        assert!(parse_progress(
            &progress(INDEXING, "report", None, Some("3/120"), Some(42)),
            &mut view
        ));
        assert_eq!(view.text().as_deref(), Some("3/120 42%"));

        assert!(parse_progress(
            &progress(INDEXING, "end", None, Some("done"), None),
            &mut view
        ));
        assert!(!view.is_active());
        assert_eq!(view.text(), None, "an ended job must go quiet");
    }

    /// A cold start runs several jobs at once — fetching, crate graph,
    /// indexing — so one of them ending must not blank the others.
    #[test]
    fn one_job_ending_does_not_clear_another() {
        let mut view = Progress::default();
        parse_progress(
            &progress(INDEXING, "begin", Some("Indexing"), None, None),
            &mut view,
        );
        parse_progress(
            &progress(FETCHING, "begin", Some("Fetching"), None, None),
            &mut view,
        );
        assert_eq!(view.text().as_deref(), Some("Fetching"));

        // Fetching finishes while indexing is still going: the read-out falls
        // back to the job that is still running instead of going blank.
        parse_progress(&progress(FETCHING, "end", None, None, None), &mut view);

        assert!(view.is_active(), "the job still running stays on screen");
        assert_eq!(view.text().as_deref(), Some("Indexing"));
    }

    #[test]
    fn the_job_that_spoke_last_is_the_one_shown() {
        // Whichever job moved last is the one worth showing.
        let mut view = Progress::default();
        parse_progress(
            &progress(INDEXING, "begin", Some("Indexing"), None, None),
            &mut view,
        );

        parse_progress(
            &progress(
                "rustAnalyzer/Roaming",
                "report",
                None,
                Some("12 files"),
                None,
            ),
            &mut view,
        );
        assert_eq!(view.text().as_deref(), Some("12 files"));

        // ...and once indexing speaks again, it comes back to the front.
        parse_progress(
            &progress(INDEXING, "report", None, Some("4/9"), None),
            &mut view,
        );
        assert_eq!(view.text().as_deref(), Some("4/9"));
    }

    #[test]
    fn progress_reuses_its_label_instead_of_reallocating() {
        // Indexing emits hundreds of reports; the label buffer is the
        // difference between one allocation and hundreds.
        let mut view = Progress::default();
        parse_progress(
            &progress(INDEXING, "begin", Some("Indexing"), None, None),
            &mut view,
        );
        let capacity = view.label_capacity();

        for i in 0..50 {
            let message = format!("{i}/120");
            parse_progress(
                &progress(INDEXING, "report", None, Some(&message), Some(i)),
                &mut view,
            );
            let expected = format!("{message} {i}%");
            assert_eq!(view.text().as_deref(), Some(expected.as_str()));
        }

        assert_eq!(view.label_capacity(), capacity, "the buffer grew");
    }

    #[test]
    fn a_nameless_job_still_reads_out() {
        let mut view = Progress::default();
        assert!(parse_progress(
            &progress(INDEXING, "begin", None, None, None),
            &mut view
        ));
        assert_eq!(view.text().as_deref(), Some("working"));

        // No percentage either, then only a percentage.
        parse_progress(
            &progress(INDEXING, "report", None, None, Some(7)),
            &mut view,
        );
        assert_eq!(view.text().as_deref(), Some("7%"));
    }

    #[test]
    fn a_percentage_out_of_range_is_clamped() {
        let mut view = Progress::default();
        parse_progress(
            &progress(INDEXING, "report", None, None, Some(5000)),
            &mut view,
        );
        assert_eq!(view.text().as_deref(), Some("100%"));
    }

    #[test]
    fn anything_that_is_not_progress_is_not_progress() {
        let mut view = Progress::default();
        assert!(!parse_progress(
            &json!({ "method": "textDocument/publishDiagnostics" }),
            &mut view
        ));
        // A progress notification with an unknown kind is left alone...
        assert!(!parse_progress(
            &progress(INDEXING, "whatever", None, None, None),
            &mut view
        ));
        // ...so is one with no `value` at all.
        assert!(!parse_progress(
            &json!({ "method": METHOD_PROGRESS, "params": {} }),
            &mut view
        ));
        assert!(!view.is_active());
    }

    #[test]
    fn message_shape_helpers() {
        assert_eq!(response_id(&json!({ "id": 3, "result": {} })), Some(3));
        assert_eq!(response_id(&json!({ "method": "$/progress" })), None);
        assert_eq!(
            server_method(&json!({ "method": "$/progress" })),
            Some("$/progress")
        );
        assert!(response_error(&json!({ "error": { "message": "boom" } })).is_some());
    }
}
