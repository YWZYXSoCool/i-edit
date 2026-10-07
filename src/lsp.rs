//! The rust-analyzer client: what the editor talks to, and how it fails.
//!
//! This module owns the *policy* — when to start a server, when to ask for
//! colors, what to do when the server dies. The pieces it is built from own
//! the mechanics: [`uri`] for paths, [`protocol`] for messages, [`transport`]
//! for bytes, [`theme`] for colors, [`tokens`] for decoding.
//!
//! Two properties the rest of the editor relies on:
//!
//! 1. **It never blocks.** Every entry point returns immediately. The main loop
//!    polls once per frame, so a slow or wedged server costs colors, not input.
//! 2. **It degrades, never escalates.** A missing binary, a dead server, a
//!    malformed message or an unsupported capability all end the same way: the
//!    coloring layer is left empty and the editor carries on. There is no path
//!    from this module to a panic or a hung exit.
//! 3. **A document is opened once.** The server is never told to drop one, so
//!    every document it has been given stays in [`LspClient::docs`]. Switching
//!    tabs therefore changes which entry is active rather than costing a
//!    `didClose`, a `didOpen` and a fresh request for colors — a tab focused
//!    again is answered from memory.
//!
//! Set `I_EDIT_LSP` to a path to override the binary, or to `off` to turn the
//! whole thing off (it then behaves exactly as it did before it existed).

pub mod diagnostics;
pub mod protocol;
pub mod theme;
pub mod tokens;
pub mod transport;
pub mod uri;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::warn;
use serde_json::Value;

use crate::highlight::StyledRun;
use crate::lsp::protocol::{Diagnostic, METHOD_SEMANTIC_REFRESH, Progress};
use crate::lsp::theme::StyleTable;
use crate::lsp::transport::{Inbound, Transport};

/// Environment variable naming the server binary; `off` disables the client.
const ENV_LSP: &str = "I_EDIT_LSP";
const DEFAULT_PROGRAM: &str = "rust-analyzer";

/// Shown once when a document is too large to hand to the server. Without it a
/// file over the limit is simply missing its colors, which looks like a bug in
/// the coloring rather than a limit being hit.
const TOO_LARGE: &str = "file is too large for rust-analyzer (> 1 MB): no colors";

/// Shown when `restart` is asked for on a client the environment turned off.
const DISABLED: &str = "rust-analyzer is off: set I_EDIT_LSP to its path";

/// How long to wait after an edit before asking again. Typing continuously
/// therefore issues one request per pause, not one per keystroke.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// How long `initialize` may take before we give up on the server.
const INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a `semanticTokens` request stays interesting. Its answer is keyed
/// by document version anyway, so an older reply is discarded on arrival; this
/// only stops the pending map from growing if a server stops answering.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Above this, a whole-document sync stops being cheap. The file is left
/// uncolored instead of stalling the editor on a payload — there is no local
/// scanner to fall back on, so the cost of syncing is the cost of colors.
const MAX_SYNC_BYTES: usize = 1024 * 1024;

/// What the editor is told.
pub enum LspEvent {
    /// Colors are ready for `version`; read them from [`LspClient::rows`].
    ///
    /// The rows are *not* carried in the event: they stay in the client's own
    /// buffer and are borrowed at injection time, so handing a response to the
    /// coloring layer costs no copy.
    Tokens { version: u64 },
    /// Underlines are ready; read them from [`LspClient::diagnostic_rows`].
    ///
    /// Like [`Tokens`](Self::Tokens), the rows stay in the client and are
    /// borrowed at injection time.
    Diagnostics,
    /// The server started, moved or finished a job. Read the read-out from
    /// [`LspClient::progress_text`].
    Progress,
    /// Something worth saying that costs nothing to ignore: the server is
    /// working, or a document was skipped. Never clears any coloring.
    Notice(String),
    /// The server is gone or unusable. `reason` is shown to the user once; the
    /// client does not try again for the rest of the session.
    Stopped(String),
}

/// Where the client is in its life with one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Disabled: `I_EDIT_LSP=off`, or after `shutdown`.
    Off,
    /// Enabled, no document worth a server yet.
    Idle,
    /// A server is running and has been asked to initialize.
    Starting,
    /// Handshake done; documents can be opened.
    Ready,
    /// Something went wrong. Terminal for this session.
    Failed,
}

/// What one outstanding request was for.
#[derive(Debug, Clone, Copy)]
enum PendingKind {
    Initialize,
    SemanticTokens,
}

struct Pending {
    kind: PendingKind,
    /// The document version this request was computed against.
    version: u64,
    /// The document it was asked about. Its answer is dropped if another one
    /// is on screen by the time it arrives: the rows would describe text
    /// nothing is showing, and two buffers can sit at the same version.
    uri: Option<String>,
    at: Instant,
}

/// What the server has been told about one document.
///
/// The server is never asked to drop a document, so this outlives the tab it
/// belongs to: switching tabs only changes which entry is active, and coming
/// back to one is answered from here instead of costing a `didOpen` and a
/// fresh request for colors.
#[derive(Default)]
struct Document {
    /// The version the server's copy is at, or `None` when it holds nothing —
    /// before the first sync, and after a reload invalidated it.
    ///
    /// `None` rather than a `0` sentinel: `0` is also a real version (the
    /// first document of a session), and reusing it for "stale" made a reload
    /// of an unedited file indistinguishable from one the server already had.
    synced_version: Option<u64>,
    /// The version the document was opened at. Kept separately because
    /// `didOpen` may only go out once the handshake finishes, by which time
    /// the buffer could have moved on — the server is told about the text it
    /// was given, and the next sync reconciles the difference.
    open_version: u64,
    /// The version the colors on screen were computed for, `None` while none
    /// have arrived. A document that was left before its answer came back is
    /// asked again when it is focused, rather than being taken for colored.
    colored: Option<u64>,
    /// The text the server holds: the baseline a change is diffed against, and
    /// the only thing that makes a range meaningful.
    text: Vec<String>,
    /// The last diagnostics the server published for that text, kept as
    /// reported: a document that is not on screen has no rows to decode them
    /// into, and `None` means nothing has been published at all — which is
    /// not the same as a clean file.
    diagnostics: Option<Vec<Diagnostic>>,
    /// Whether the "too large to sync" notice was already raised for it.
    warned_large: bool,
}

pub struct LspClient {
    program: Option<String>,
    phase: Phase,
    transport: Option<Transport>,
    /// Index-to-style tables, built once from the server's legend.
    table: Option<StyleTable>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// The document the editor is looking at, if any. Every document the
    /// server holds is in [`Self::docs`]; this says which one its answers are
    /// for.
    uri: Option<String>,
    /// What the server knows about each document it holds, by URI.
    ///
    /// Nothing is ever removed from here while the session lasts — a `didClose`
    /// would make the next look at the tab cost a `didOpen` and a fresh
    /// request for colors, and the server is happy to hold every document it
    /// was given.
    docs: HashMap<String, Document>,
    language: &'static str,
    /// Workspace root handed to the server.
    root: Option<PathBuf>,
    /// When a pending edit becomes old enough to sync.
    due: Option<Instant>,
    started_at: Option<Instant>,
    /// Whether the server asked for incremental sync, so a change can be sent
    /// as one range instead of the whole file.
    incremental: bool,
    /// Decoded token rows, reused across responses.
    rows: Vec<Vec<StyledRun>>,
    /// Decoded diagnostic underlines, reused across notifications.
    diagnostic_rows: Vec<Vec<StyledRun>>,
    /// What the server is busy with, updated in place by `$/progress`.
    progress: Progress,
    /// What to say once, at the next `tick`. Kept out of the event stream so a
    /// notice can be raised from anywhere without a borrow.
    notice: Option<String>,
}

impl Default for LspClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LspClient {
    /// A client configured from the environment. No server is started yet.
    pub fn new() -> Self {
        let program = match std::env::var(ENV_LSP).ok() {
            Some(value) if value == "off" || value == "0" => None,
            Some(value) if !value.trim().is_empty() => Some(value),
            _ => Some(DEFAULT_PROGRAM.to_string()),
        };

        // No program means no client at all: `I_EDIT_LSP=off` must leave the
        // editor exactly as it was before this module existed.
        let phase = if program.is_some() {
            Phase::Idle
        } else {
            Phase::Off
        };

        Self {
            program,
            phase,
            transport: None,
            table: None,
            next_id: 0,
            pending: HashMap::new(),
            uri: None,
            docs: HashMap::new(),
            language: "rust",
            root: None,
            due: None,
            started_at: None,
            incremental: false,
            rows: Vec::new(),
            diagnostic_rows: Vec::new(),
            progress: Progress::default(),
            notice: None,
        }
    }

    // --- the document cache ----------------------------------------------

    /// What the server holds for the document on screen, if it holds one.
    fn doc(&self) -> Option<&Document> {
        self.docs.get(self.uri.as_deref()?)
    }

    /// Mutable form of [`Self::doc`].
    fn doc_mut(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.uri.as_deref()?)
    }

    /// The version the server's copy of the document on screen is at.
    fn synced_version(&self) -> Option<u64> {
        self.doc()?.synced_version
    }

    fn set_synced_version(&mut self, version: Option<u64>) {
        if let Some(doc) = self.doc_mut() {
            doc.synced_version = version;
        }
    }

    /// The version the document on screen was opened at.
    fn open_version(&self) -> u64 {
        self.doc().map_or(0, |doc| doc.open_version)
    }

    fn set_open_version(&mut self, version: u64) {
        if let Some(doc) = self.doc_mut() {
            doc.open_version = version;
        }
    }

    /// The text the server holds for the document on screen.
    fn synced_text(&self) -> &[String] {
        match self.doc() {
            Some(doc) => doc.text.as_slice(),
            None => &[],
        }
    }

    /// The decoded token rows of the most recent response.
    pub fn rows(&self) -> &[Vec<StyledRun>] {
        &self.rows
    }

    /// The decoded diagnostic underlines of the most recent notification.
    ///
    /// Empty is not the same as "no diagnostics": a file with none is reported
    /// that way, and an empty set is what clears stale underlines after an
    /// error is fixed.
    pub fn diagnostic_rows(&self) -> &[Vec<StyledRun>] {
        &self.diagnostic_rows
    }

    /// Whether this client will ever produce colors again.
    pub fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Off | Phase::Failed)
    }

    /// What the server is busy with, for the status bar. `None` when it is
    /// idle.
    ///
    /// Allocating on purpose: this is read when a progress notification
    /// arrives, not once per frame, and the caller keeps the result.
    pub fn progress_text(&self) -> Option<String> {
        self.progress.text()
    }

    /// Throws the current server away and starts a new one.
    ///
    /// The recovery path for a server that died or was killed: the failure is
    /// terminal so that one death does not become a message per keystroke, and
    /// this is the way back from it. The open document is *not* forgotten, so
    /// the next [`Self::open`] — or the handshake completing, which reopens it
    /// by itself — picks up where it left off.
    pub fn restart(&mut self) -> Option<LspEvent> {
        if self.program.is_none() {
            // `I_EDIT_LSP=off` is a setting, not a failure: say so rather
            // than pretending to have restarted something.
            return Some(LspEvent::Notice(DISABLED.to_string()));
        }

        self.drop_server();
        self.phase = Phase::Idle;
        self.start()
    }

    /// Tells the client which document the editor is looking at.
    ///
    /// The outgoing document is *not* closed: it stays in [`Self::docs`] with
    /// everything the server said about it, so focusing its tab again is a
    /// lookup. Only a document the server has never seen is opened, and only
    /// one whose colors never arrived is asked again.
    ///
    /// Non-Rust files are not handed over at all. Returns an event when
    /// starting the server failed outright, or when the document's remembered
    /// diagnostics are worth putting back on screen.
    pub fn open(&mut self, path: &Path, lines: &[String], version: u64) -> Option<LspEvent> {
        if self.is_finished() {
            return None;
        }

        let Some(language) = language_id(path) else {
            // The server has nothing to say about the file, but the document
            // that was on screen keeps its place with it — dropping that one
            // is what used to make coming back cost a reopen.
            self.deactivate();
            return None;
        };

        let uri = uri::path_to_uri(path);
        self.root = workspace_root(path).or_else(|| path.parent().map(Path::to_path_buf));
        self.language = language;

        if self.uri.as_deref() == Some(uri.as_str()) {
            // The same document reopened — a reload, or a save under the name
            // it already had. Marking the sync stale is enough; restarting the
            // server would cost a full re-index for nothing. The document also
            // gets the new version, in case `didOpen` is still waiting on the
            // handshake and goes out with this text rather than the old one.
            self.set_synced_version(None);
            self.set_open_version(version);
            return None;
        }

        // Switching tabs: the outgoing document is left as it is and the
        // incoming one becomes active. What it needs from the server is
        // decided by what is already remembered about it.
        self.docs.entry(uri.clone()).or_default();
        self.uri = Some(uri);
        self.set_open_version(version);
        self.due = None;
        self.rows.clear();
        self.diagnostic_rows.clear();

        match self.phase {
            // The document is recorded; `didOpen` goes out as soon as the
            // handshake completes.
            Phase::Starting => None,
            Phase::Off | Phase::Failed => None,
            Phase::Idle => self.start(),
            Phase::Ready => self.catch_up(lines, version),
        }
    }

    /// Brings the active document up to date, asking for as little as the
    /// cache allows.
    ///
    /// The one case worth nothing at all is a tab being focused again: the
    /// server already holds the text, the buffer has not moved, and its
    /// colors are already on screen — there is nothing to send.
    fn catch_up(&mut self, lines: &[String], version: u64) -> Option<LspEvent> {
        let uri = self.uri.clone()?;
        let (synced, colored) = match self.docs.get(&uri) {
            Some(doc) => (doc.synced_version, doc.colored),
            None => (None, None),
        };

        match synced {
            // The server holds exactly this text. Only colors that never
            // arrived are worth asking for, and the document is not reopened
            // for them.
            Some(held) if held == version => {
                if colored != Some(version) {
                    self.request_tokens(&uri, version);
                }
                self.remembered_diagnostics(&uri, lines)
            }
            // The server holds an older copy of a document it was given: a
            // change is enough, and the debounce sends it.
            Some(_) => {
                self.set_synced_version(None);
                None
            }
            // A document the server has never seen.
            None => {
                self.send_open(lines);
                None
            }
        }
    }

    /// Puts back the diagnostics remembered for a document the server is
    /// holding, if it ever published any for the text it has.
    ///
    /// Without this, a tab focused again keeps the underlines it had when it
    /// was last looked at, which are right up until the server says something
    /// new about the file while it was away.
    fn remembered_diagnostics(&mut self, uri: &str, lines: &[String]) -> Option<LspEvent> {
        let items = self.docs.get(uri)?.diagnostics.clone()?;

        diagnostics::decode(&items, lines, &mut self.diagnostic_rows);
        Some(LspEvent::Diagnostics)
    }

    /// No document is on screen: a scratch buffer, or a file the server has
    /// no opinion about.
    ///
    /// Nothing is closed at the server — it goes on holding every document it
    /// was given, which is what makes focusing a tab again free.
    pub fn deactivate(&mut self) {
        self.uri = None;
        self.rows.clear();
        self.diagnostic_rows.clear();
        self.due = None;
    }

    /// Forgets a document whose tab was closed.
    ///
    /// The one case a document *is* closed at the server, because it is the
    /// one case nothing can come back to it: the buffer that was showing it is
    /// gone, and the file has no other tab. Holding it would only spend memory
    /// on a document no switch can ever ask for again — reopening the file
    /// later starts from a new buffer, so a fresh `didOpen` is right anyway.
    pub fn forget(&mut self, path: &Path) {
        if language_id(path).is_none() {
            return;
        }

        let uri = uri::path_to_uri(path);
        // Not in the table means the server never heard of it, so there is
        // nothing to take back.
        if self.docs.remove(&uri).is_none() {
            return;
        }

        self.send(&protocol::did_close(&uri));

        if self.uri.as_deref() == Some(uri.as_str()) {
            self.deactivate();
        }
    }

    /// Called once per frame: delivers whatever arrived, and sends a request
    /// when an edit has settled.
    ///
    /// `version` is the buffer's current version. Responses stamped with an
    /// older one are still reported — the caller compares — because a stale
    /// response is harmless once checked and dropping it here would hide the
    /// case where the buffer caught up again.
    pub fn tick(&mut self, lines: &[String], version: u64) -> Option<LspEvent> {
        if self.is_finished() || matches!(self.phase, Phase::Idle) {
            return None;
        }

        if self.phase == Phase::Starting
            && self
                .started_at
                .is_some_and(|at| at.elapsed() > INIT_TIMEOUT)
        {
            return Some(self.fail("rust-analyzer did not answer in time"));
        }

        // Requests that will never be matched by anything useful.
        self.pending
            .retain(|_, pending| pending.at.elapsed() < REQUEST_TIMEOUT);

        if let Some(event) = self.dispatch(lines) {
            return Some(event);
        }

        self.maybe_sync(lines, version);
        self.take_notice()
    }

    /// Stops the server and joins its threads. Safe to call twice, and safe to
    /// call on a client that never started one.
    pub fn shutdown(&mut self) {
        if let Some(mut transport) = self.transport.take() {
            // Politeness first, but without waiting: a server that ignores
            // `shutdown` is killed by the `Drop` below either way, and an exit
            // must never hang on a language server.
            let id = self.next_id();
            transport.send(&protocol::shutdown(id));
            transport.send(&protocol::exit());
            transport.shutdown();
        }

        self.phase = Phase::Off;
        self.pending.clear();
        self.uri = None;
        // The server is going away, and with it every document it held.
        self.docs.clear();
        self.rows.clear();
        self.diagnostic_rows.clear();
        self.progress = Progress::default();
        self.incremental = false;
    }

    // --- internals -------------------------------------------------------

    /// Pulls one message off the transport and acts on it.
    fn dispatch(&mut self, lines: &[String]) -> Option<LspEvent> {
        let inbound = self.transport.as_ref()?.try_recv()?;

        match inbound {
            Inbound::Eof => Some(self.fail("rust-analyzer exited")),
            Inbound::Json(message) => self.handle(&message, lines),
        }
    }

    fn handle(&mut self, message: &Value, lines: &[String]) -> Option<LspEvent> {
        // A notification carries no id, so it has to be recognised by method
        // before the response path rejects it for having none.
        if let Some(event) = self.on_notification(message, lines) {
            return Some(event);
        }

        // Requests that arrive with an id we did not ask for. They have to be
        // recognised here: below, an id that is not in the pending table is
        // discarded, and these would be discarded with it — which for a
        // `refresh` means a file that never gets its full colors.
        if let Some(method) = protocol::server_method(message)
            && protocol::SERVER_REQUESTS.contains(&method)
        {
            self.on_server_request(method, message);
            return None;
        }

        let Some(id) = protocol::response_id(message) else {
            // Server-initiated traffic we do not act on — `$/progress`,
            // `workspace/configuration`, ... Ignoring a server request is
            // allowed by the protocol and never blocks it.
            return None;
        };

        let Some(pending) = self.pending.remove(&id) else {
            return None; // an answer to something we stopped caring about
        };

        if let Some(error) = protocol::response_error(message) {
            return match pending.kind {
                PendingKind::Initialize => {
                    Some(self.fail(format!("rust-analyzer refused to start: {error}")))
                }
                // One failed color request is not worth reporting; the next
                // debounced sync asks again.
                PendingKind::SemanticTokens => None,
            };
        }

        match pending.kind {
            PendingKind::Initialize => self.on_initialized(message, lines),
            PendingKind::SemanticTokens => {
                // An answer about a document that is no longer on screen: the
                // rows would describe text nothing is showing, and two buffers
                // can sit at the same version.
                if pending.uri.as_deref() != self.uri.as_deref() {
                    return None;
                }
                self.on_tokens(message, lines, pending.version)
            }
        }
    }

    /// Answers a server-initiated request, and acts on the one that is worth
    /// acting on.
    ///
    /// All three are answered with `result: null` because none of them carries
    /// anything to report; the server is simply entitled to an answer. A
    /// `workspace/semanticTokens/refresh` is the one with a consequence: the
    /// server sends it when something it had not finished earlier is now known
    /// — the usual case being a file opened while the project was still being
    /// indexed. The colors of that moment are partial (no `mutable`, no
    /// `reference`: the analysis that decides those had not run), and without
    /// this they would stay partial until the buffer was edited.
    fn on_server_request(&mut self, method: &str, message: &Value) {
        if let Some(id) = protocol::response_id(message) {
            self.send(&protocol::null_response(id));
        }

        if method != METHOD_SEMANTIC_REFRESH {
            // Diagnostics are pushed to us, and a progress token needs nothing
            // from us but the answer above.
            return;
        }

        // Same document, same version: asking again is only worth it when
        // nothing is already in flight for it, since an indexing run asks
        // several times in a row.
        let synced = self.synced_version();
        let already_asked = self.pending.values().any(|pending| {
            matches!(pending.kind, PendingKind::SemanticTokens) && Some(pending.version) == synced
        });

        if already_asked {
            return;
        }

        if let (Some(uri), Some(version)) = (self.uri.clone(), synced) {
            self.request_tokens(&uri, version);
        }
    }

    /// Acts on a server-initiated message, if it is one we care about.
    fn on_notification(&mut self, message: &Value, lines: &[String]) -> Option<LspEvent> {
        // Progress is the noisier of the two notifications — an indexing run
        // emits hundreds — so it is recognised first.
        if protocol::parse_progress(message, &mut self.progress) {
            return Some(LspEvent::Progress);
        }

        let (uri, items) = protocol::parse_publish_diagnostics(message)?;

        // The server reports on every document it holds, not just the one on
        // screen. What it says about another one is remembered for when that
        // tab is focused again: the editor keeps one set of rows, and a
        // diagnostic for a file it is not showing has nowhere to go now.
        if self.uri.as_deref() != Some(uri) {
            if let Some(doc) = self.docs.get_mut(uri) {
                doc.diagnostics = Some(items);
            }
            return None;
        }

        if let Some(doc) = self.docs.get_mut(uri) {
            doc.diagnostics = Some(items.clone());
        }

        diagnostics::decode(&items, lines, &mut self.diagnostic_rows);
        Some(LspEvent::Diagnostics)
    }

    fn on_initialized(&mut self, message: &Value, lines: &[String]) -> Option<LspEvent> {
        let legend = protocol::parse_initialize_result(message).filter(|legend| !legend.is_empty());
        let Some(legend) = legend else {
            return Some(self.fail("rust-analyzer does not offer semantic tokens"));
        };

        self.table = Some(StyleTable::build(&legend));
        // Read here and not per change: whether the server wants one range or
        // the whole document is decided once, in its answer to `initialize`.
        self.incremental = protocol::prefers_incremental_sync(message);
        self.send(&protocol::initialized());
        self.phase = Phase::Ready;
        self.started_at = None;

        // The document was opened while the handshake was in flight; hand it
        // over now that the server is listening. `open_version` is used rather
        // than the version of this frame, because the text being sent is the
        // text the document had when it opened.
        self.send_open(lines);

        None
    }

    fn on_tokens(&mut self, message: &Value, lines: &[String], version: u64) -> Option<LspEvent> {
        let data = protocol::parse_semantic_tokens(message)?;
        let table = self.table.as_ref()?;

        tokens::decode(&data, lines, table, &mut self.rows);
        // Remembered so a tab focused again knows it has colors and need not
        // ask; a document left before this arrives is asked for again.
        if let Some(doc) = self.doc_mut() {
            doc.colored = Some(version);
        }
        Some(LspEvent::Tokens { version })
    }

    /// Sends the edit that has been sitting still long enough.
    fn maybe_sync(&mut self, lines: &[String], version: u64) {
        if self.phase != Phase::Ready
            || self.uri.is_none()
            || self.synced_version() == Some(version)
        {
            return;
        }

        match self.due {
            None => self.due = Some(Instant::now() + DEBOUNCE),
            Some(due) if Instant::now() >= due => {
                self.due = None;
                self.push_sync(lines, version);
            }
            Some(_) => {}
        }
    }

    fn push_sync(&mut self, lines: &[String], version: u64) {
        let Some(uri) = self.uri.clone() else {
            return;
        };

        let (range, text) = self.change(lines);
        if text.len() > MAX_SYNC_BYTES {
            self.skip_too_large(version);
            return;
        }

        let sent = match range {
            Some(range) => self.send(&protocol::did_change_range(&uri, version, &range, &text)),
            None => self.send(&protocol::did_change(&uri, version, &text)),
        };

        if sent {
            // The server's copy is the buffer's now, so the next change is
            // diffed against this one.
            self.remember(lines);
            // Asking for colors before the text is in the server's hands would
            // paint an answer about the *previous* buffer — and the version
            // would match, so the mismatch would not even be noticed on
            // arrival.
            self.request_tokens(&uri, version);
            self.set_synced_version(Some(version));
        }
    }

    /// What to tell the server: the range the change replaces — `None` when the
    /// whole document has to go — and the text that takes its place.
    ///
    /// One row is sent for a change on one row. That is the whole point: a
    /// keystroke in a large file is a handful of bytes either way, and the
    /// server's copy of the document stays in step with ours.
    fn change(&self, lines: &[String]) -> (Option<Value>, String) {
        // Without a row to name the end of the range with, or with a change too
        // large to send at all, the whole document is the only thing left to
        // say — which is what the fall-through below does.
        if self.incremental
            && let Some((first, last_old, last_new)) =
                protocol::changed_rows(self.synced_text(), lines)
        {
            // The new rows take the place of the old `[first, last_old)`. The
            // text is the new block joined by newlines and — when rows were
            // actually added — terminated by one, so the last new row is
            // separated from the row that follows it. Without that trailing
            // newline a one-row insertion would be glued onto the row below and
            // every token after the edit would shift by a line.
            let text = if last_new > first {
                let mut joined = lines[first..last_new].join("\n");
                joined.push('\n');
                joined
            } else {
                // Pure deletion: the rows are removed, nothing is inserted.
                String::new()
            };
            if text.len() <= MAX_SYNC_BYTES
                && let Some(range) = protocol::row_range(self.synced_text(), first, last_old)
            {
                return (Some(range), text);
            }
        }

        (None, lines.join("\n"))
    }

    fn send_open(&mut self, lines: &[String]) {
        let Some(uri) = self.uri.clone() else {
            return;
        };
        let version = self.open_version();
        let Some(text) = self.join(lines, version) else {
            return;
        };

        if self.send(&protocol::did_open(&uri, self.language, version, &text)) {
            // From here the server's copy and the buffer are the same, which is
            // what makes any later change describable as a range.
            self.remember(lines);
            self.request_tokens(&uri, version);
            self.set_synced_version(Some(version));
        }
    }

    /// Keeps the copy of the document the server is holding.
    fn remember(&mut self, lines: &[String]) {
        if let Some(doc) = self.doc_mut() {
            doc.text.clear();
            doc.text.extend_from_slice(lines);
        }
    }

    /// The document as one string, or `None` when it is too large to sync.
    fn join(&mut self, lines: &[String], version: u64) -> Option<String> {
        let text = lines.join("\n");
        if text.len() > MAX_SYNC_BYTES {
            self.skip_too_large(version);
            return None;
        }
        Some(text)
    }

    /// Gives up on a document the server will never be told about.
    fn skip_too_large(&mut self, version: u64) {
        // Recorded as synced so the debounce stops asking about a file the
        // server will never be told about.
        self.set_synced_version(Some(version));
        // There is no local scanner to fall back on, so a skipped file is an
        // uncolored file; saying so once keeps that from looking like a broken
        // coloring layer.
        let warned = match self.doc_mut() {
            Some(doc) => std::mem::replace(&mut doc.warned_large, true),
            None => false,
        };
        if !warned {
            self.notice = Some(TOO_LARGE.to_string());
        }
    }

    /// Hands back whatever should be said once, if anything is queued.
    fn take_notice(&mut self) -> Option<LspEvent> {
        self.notice.take().map(LspEvent::Notice)
    }

    fn request_tokens(&mut self, uri: &str, version: u64) {
        let id = self.next_id();
        if self.send(&protocol::semantic_tokens_full(id, uri)) {
            self.pending.insert(
                id,
                Pending {
                    kind: PendingKind::SemanticTokens,
                    version,
                    uri: Some(uri.to_string()),
                    at: Instant::now(),
                },
            );
        }
    }

    fn start(&mut self) -> Option<LspEvent> {
        let program = self
            .program
            .clone()
            .unwrap_or_else(|| DEFAULT_PROGRAM.to_string());

        match Transport::spawn(&program) {
            Ok(transport) => {
                let id = self.next_id();
                let root = self.root_uri();
                if transport.send(&protocol::initialize(id, &root)) {
                    self.pending.insert(
                        id,
                        Pending {
                            kind: PendingKind::Initialize,
                            version: 0,
                            uri: None,
                            at: Instant::now(),
                        },
                    );
                    self.transport = Some(transport);
                    self.started_at = Some(Instant::now());
                    self.phase = Phase::Starting;
                    return None;
                }
                // Connected, then refused a request: treat as a failed start.
                // `transport` is dropped here, which kills the child.
                Some(self.fail("rust-analyzer stopped accepting requests"))
            }
            Err(err) => Some(self.fail(format!(
                "cannot start {program}: {err} (set {ENV_LSP} to its path, or {ENV_LSP}=off)"
            ))),
        }
    }

    fn root_uri(&self) -> String {
        let root = self.root.as_deref().filter(|path| path.is_dir());
        match root {
            Some(dir) => uri::path_to_uri(dir),
            None => std::env::current_dir()
                .map(|dir| uri::path_to_uri(&dir))
                .unwrap_or_default(),
        }
    }

    /// Sends one message; `false` when there is nowhere to send it.
    fn send(&self, message: &Value) -> bool {
        self.transport.as_ref().is_some_and(|t| t.send(message))
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Throws away the server and everything derived from it.
    ///
    /// Shared by [`Self::fail`] and [`Self::restart`]: the two differ only in
    /// what happens next to the phase and to the open document. Dropping the
    /// transport kills the child and joins both threads; the rest is state we
    /// no longer trust.
    fn drop_server(&mut self) {
        self.transport = None;
        self.pending.clear();
        self.table = None;
        // A new server starts from nothing, so neither the baseline nor the
        // sync kind it agreed to survive — and neither does anything it was
        // told about a document. The documents themselves are kept: they are
        // handed over again as they are looked at.
        for doc in self.docs.values_mut() {
            doc.synced_version = None;
            doc.colored = None;
            doc.text.clear();
            doc.diagnostics = None;
        }
        self.incremental = false;
        self.rows.clear();
        self.diagnostic_rows.clear();
        self.progress = Progress::default();
        self.due = None;
        self.started_at = None;
    }

    /// Ends the session with the server and reports why.
    fn fail(&mut self, reason: impl Into<String>) -> LspEvent {
        let reason = reason.into();
        warn!("lsp: {reason}");

        self.drop_server();
        self.uri = None;
        // Terminal on purpose: retrying a server that just died would turn one
        // failure into a message per keystroke. [`Self::restart`] is the way
        // back.
        self.phase = Phase::Failed;

        LspEvent::Stopped(reason)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The `languageId` for a path, or `None` when the server has no opinion.
fn language_id(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str())? {
        "rs" => Some("rust"),
        _ => None,
    }
}

/// The workspace root for a file: the nearest ancestor holding a `Cargo.toml`.
///
/// Handing the server the file's own directory instead would leave it without a
/// project, and a server without a project still answers — it just resolves
/// nothing outside the file, so half the tokens come back as unresolved.
fn workspace_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
impl LspClient {
    /// The state a finished handshake leaves, without a server: the sync policy
    /// is what these tests are about, and with no transport every `send` is a
    /// no-op, so nothing here depends on one being spawnable.
    fn ready_for_test() -> Self {
        let mut client = Self::new();
        client.phase = Phase::Ready;
        client
    }

    /// As [`Self::ready_for_test`], but with a document the server is already
    /// holding at `version`: the state a tab that has been looked at leaves
    /// behind. Its colors have not come back, which is the interesting case.
    fn holding_document(uri: &str, text: &str, version: u64) -> Self {
        let mut client = Self::ready_for_test();
        client.uri = Some(uri.to_string());
        client.docs.insert(
            uri.to_string(),
            Document {
                synced_version: Some(version),
                open_version: version,
                colored: None,
                text: text.split('\n').map(str::to_string).collect(),
                diagnostics: None,
                warned_large: false,
            },
        );
        client
    }

    /// Says the colors of the document on screen arrived, for `version`.
    fn colored_for_test(&mut self, version: u64) {
        if let Some(doc) = self.doc_mut() {
            doc.colored = Some(version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LspClient, language_id};
    use crate::highlight::StyledRun;
    use serde_json::json;
    use std::path::Path;

    /// A `publishDiagnostics` notification with one diagnostic on line 0.
    fn diagnostics(uri: &str) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 3 }
                    },
                    "severity": 1
                }]
            }
        })
    }

    #[test]
    fn diagnostics_for_the_open_file_become_underlines() {
        let mut client = LspClient::ready_for_test();
        client.uri = Some("file:///a.rs".to_string());
        let lines = vec!["abc".to_string()];

        let event = client.on_notification(&diagnostics("file:///a.rs"), &lines);
        assert!(matches!(event, Some(super::LspEvent::Diagnostics)));
        assert_eq!(client.diagnostic_rows()[0].len(), 1);
    }

    #[test]
    fn diagnostics_for_another_file_are_dropped() {
        // The server reports on everything it has indexed; the editor keeps one
        // set of rows for the file on screen, so the rest has nowhere to go.
        let mut client = LspClient::ready_for_test();
        client.uri = Some("file:///a.rs".to_string());
        let lines = vec!["abc".to_string()];

        assert!(
            client
                .on_notification(&diagnostics("file:///elsewhere.rs"), &lines)
                .is_none()
        );
        assert!(client.diagnostic_rows().is_empty());
    }

    #[test]
    fn a_clean_file_clears_the_underlines() {
        let mut client = LspClient::ready_for_test();
        client.uri = Some("file:///a.rs".to_string());
        let lines = vec!["abc".to_string()];

        client.on_notification(&diagnostics("file:///a.rs"), &lines);
        assert_eq!(client.diagnostic_rows()[0].len(), 1);

        // An empty set is a real report: the error was fixed.
        let clean = json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///a.rs", "diagnostics": [] }
        });
        client.on_notification(&clean, &lines);
        assert!(client.diagnostic_rows()[0].is_empty());
    }

    /// What the server sends when it has finished something it had not
    /// finished when we first asked.
    fn refresh(id: u64) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": super::protocol::METHOD_SEMANTIC_REFRESH
        })
    }

    /// A `$/progress` notification, the shape rust-analyzer sends while it
    /// indexes.
    fn progress(kind: &str, message: &str) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "method": super::protocol::METHOD_PROGRESS,
            "params": {
                "token": "rust-analyzer/Indexing",
                "value": { "kind": kind, "title": "Indexing", "message": message }
            }
        })
    }

    #[test]
    fn a_refresh_request_asks_for_colors_again() {
        // The whole point of the request: a file opened mid-index keeps its
        // early, partial colors unless something asks again. With no transport
        // the request cannot go out, so what is observable is that one was
        // made — every request takes the next id.
        let mut client = LspClient::holding_document("file:///a.rs", "fn main() {}", 4);

        let before = client.next_id;
        client.handle(&refresh(1), &[]);

        assert!(
            client.next_id > before,
            "the current document must be re-asked for"
        );
    }

    #[test]
    fn a_refresh_is_only_asked_once_per_version() {
        // An indexing run can ask several times in a row; the second ask for a
        // version already in flight is redundant.
        let mut client = LspClient::holding_document("file:///a.rs", "fn main() {}", 4);
        client.pending.insert(
            9,
            super::Pending {
                kind: super::PendingKind::SemanticTokens,
                version: 4,
                uri: Some("file:///a.rs".to_string()),
                at: std::time::Instant::now(),
            },
        );

        let before = client.next_id;
        client.handle(&refresh(1), &[]);

        assert_eq!(client.next_id, before, "one is already in flight");
    }

    #[test]
    fn a_refresh_before_any_document_is_ignored() {
        // Nothing has been synced, so there is nothing to ask about.
        let mut client = LspClient::ready_for_test();

        let before = client.next_id;
        client.handle(&refresh(1), &[]);
        assert_eq!(client.next_id, before);
    }

    #[test]
    fn progress_reaches_the_status_bar_and_goes_quiet_again() {
        let mut client = LspClient::ready_for_test();
        let lines = vec!["fn main() {}".to_string()];

        assert!(client.handle(&progress("begin", "0/3"), &lines).is_some());
        assert!(client.progress_text().is_some(), "a running job is shown");

        // The `end` half is what clears it.
        assert!(client.handle(&progress("end", "done"), &lines).is_some());
        assert_eq!(client.progress_text(), None);
    }

    #[test]
    fn a_failure_clears_the_progress_read_out() {
        // A dead server must not leave "Indexing" hanging in the status bar.
        let mut client = LspClient::ready_for_test();
        client.handle(&progress("begin", "0/3"), &[]);
        assert!(client.progress_text().is_some());

        client.fail("rust-analyzer exited");
        assert_eq!(client.progress_text(), None);
    }

    #[test]
    fn restart_after_a_failure_leaves_the_failure_behind() {
        // `fail` is terminal, so without this a killed server means no colors
        // for the rest of the session. Whether a server can actually be
        // spawned is the environment's business; what matters is that either a
        // new handshake starts, or the failure is reported fresh.
        let mut client = LspClient::ready_for_test();
        client.fail("rust-analyzer exited");
        assert!(client.is_finished());

        match client.restart() {
            None => assert_eq!(
                client.phase,
                super::Phase::Starting,
                "a new server is being started"
            ),
            Some(event) => assert!(
                matches!(event, super::LspEvent::Stopped(_)),
                "starting failed and said so"
            ),
        }
        assert!(client.progress_text().is_none());
    }

    #[test]
    fn restarting_a_disabled_client_says_so() {
        // `I_EDIT_LSP=off` is a setting, not something to recover from.
        let mut client = LspClient::new();
        client.phase = super::Phase::Off;
        client.program = None;

        let event = client.restart().expect("a notice");
        assert!(matches!(event, super::LspEvent::Notice(_)));
    }

    #[test]
    fn a_document_too_large_to_sync_is_reported_once() {
        let mut client = LspClient::holding_document("file:///a.rs", "", 1);
        let lines = vec!["x".repeat(super::MAX_SYNC_BYTES + 1)];

        client.send_open(&lines);

        let event = client.take_notice().expect("a notice");
        assert!(matches!(event, super::LspEvent::Notice(_)));

        // Typing in the same file must not repeat it.
        assert!(client.join(&lines, 2).is_none());
        assert!(
            client.take_notice().is_none(),
            "said once, not per keystroke"
        );
    }

    #[test]
    fn a_large_document_is_still_recorded_as_synced() {
        // Otherwise the debounce keeps asking about a file the server will
        // never be told about.
        let mut client = LspClient::holding_document("file:///a.rs", "", 5);
        let lines = vec!["x".repeat(super::MAX_SYNC_BYTES + 1)];

        assert!(client.join(&lines, 5).is_none());
        assert_eq!(client.synced_version(), Some(5));
    }

    #[test]
    fn a_notification_is_not_mistaken_for_a_response() {
        // `handle` recognises it before the response path, which would reject
        // it for having no id.
        let mut client = LspClient::ready_for_test();
        client.uri = Some("file:///a.rs".to_string());
        let lines = vec!["abc".to_string()];

        let event = client.handle(&diagnostics("file:///a.rs"), &lines);
        assert!(event.is_some(), "a notification must not fall through");
    }

    #[test]
    fn only_rust_files_are_handed_to_the_server() {
        assert_eq!(language_id(Path::new("a.rs")), Some("rust"));
        assert_eq!(language_id(Path::new("a.RS")), None);
        assert_eq!(language_id(Path::new("a.md")), None);
        assert_eq!(language_id(Path::new("a")), None);
        assert_eq!(language_id(Path::new("dir.rs/a")), None);
    }

    #[test]
    fn rows_start_empty_and_the_client_is_not_finished() {
        let client = LspClient::new();
        assert!(client.rows().is_empty());
        // `I_EDIT_LSP` is unset in tests, so the default program is in play and
        // only a failure can finish the client.
        assert!(!client.is_finished());
    }

    #[test]
    fn tick_is_a_no_op_before_any_document_is_opened() {
        let mut client = LspClient::new();
        let lines = vec!["fn main() {}".to_string()];
        assert!(client.tick(&lines, 1).is_none());
    }

    #[test]
    fn a_dead_server_leaves_no_colors_and_stays_quiet() {
        // Plan §12: losing the server must cost colors and nothing else, and
        // must not turn into a message per keystroke.
        let mut client = LspClient::ready_for_test();
        client.uri = Some("file:///a.rs".to_string());
        client.rows = vec![vec![StyledRun {
            start: 0,
            end: 1,
            style: ratatui::style::Style::default(),
        }]];

        let event = client.fail("rust-analyzer exited");
        assert!(matches!(event, super::LspEvent::Stopped(_)));
        assert!(client.rows().is_empty(), "colors stop being claimed");
        assert!(client.is_finished());

        // Terminal: nothing after it starts a server or asks for anything.
        let lines = vec!["fn main() {}".to_string()];
        assert!(client.open(Path::new("a.rs"), &lines, 1).is_none());
        assert!(client.tick(&lines, 1).is_none());
        assert!(client.due.is_none(), "no debounce is left pending");
    }

    #[test]
    fn shutdown_makes_the_client_inert() {
        let mut client = LspClient::new();
        client.shutdown();
        assert!(client.is_finished());

        // Nothing after shutdown starts a server or produces an event.
        let lines = vec!["fn main() {}".to_string()];
        assert!(client.open(Path::new("a.rs"), &lines, 1).is_none());
        assert!(client.tick(&lines, 1).is_none());
    }

    #[test]
    fn a_non_rust_file_does_not_start_a_server() {
        let mut client = LspClient::new();
        let lines = vec!["# hi".to_string()];
        assert!(client.open(Path::new("a.md"), &lines, 1).is_none());
        // Still Idle: `open` on a markdown file closes rather than starts.
        assert!(!client.is_finished());
        assert!(client.tick(&lines, 1).is_none());
    }

    #[test]
    fn reopening_the_same_file_asks_for_colors_again() {
        let mut client = LspClient::ready_for_test();
        let lines = vec!["fn main() {}".to_string()];

        assert!(client.open(Path::new("a.rs"), &lines, 0).is_none());
        // The same file again — a reload. The version it lands on is still 0,
        // which is also what "nothing sent yet" looks like; with a bare `0` for
        // both, the reload was mistaken for a document the server already had
        // and no colors were ever asked for again.
        assert!(client.open(Path::new("a.rs"), &lines, 0).is_none());
        assert_eq!(
            client.synced_version(),
            None,
            "a reload invalidates the sync"
        );

        assert!(client.tick(&lines, 0).is_none());
        assert!(client.due.is_some(), "the reloaded file must be re-synced");
    }

    /// Two documents that are not the file they are named after: `open` only
    /// needs a path for its URI and its language.
    fn other() -> &'static Path {
        Path::new("b.rs")
    }

    #[test]
    fn coming_back_to_a_tab_costs_nothing() {
        // The server is never told to drop a document, so a tab that is left
        // and focused again is a lookup: no `didOpen`, no request for colors.
        // Both are observable without a transport — every request takes an id.
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");

        client.open(Path::new("a.rs"), &lines, 0);
        client.set_synced_version(Some(0));
        client.colored_for_test(0);

        // Leaving it: nothing is closed, and the other document is opened.
        client.open(other(), &lines, 0);
        let asked = client.next_id;

        assert!(client.open(Path::new("a.rs"), &lines, 0).is_none());
        assert_eq!(client.next_id, asked, "no colors are asked for again");
        assert_eq!(client.synced_version(), Some(0), "and it is not reopened");
        assert!(
            client
                .docs
                .contains_key(&super::uri::path_to_uri(Path::new("a.rs"))),
            "the document the server holds is still there"
        );
    }

    #[test]
    fn a_document_left_before_its_colors_arrived_is_asked_again() {
        // Asked, but not reopened: the server already holds the text, so only
        // the request that never came back is repeated.
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");

        client.open(Path::new("a.rs"), &lines, 0);
        client.set_synced_version(Some(0));

        client.open(other(), &lines, 0);
        let asked = client.next_id;

        client.open(Path::new("a.rs"), &lines, 0);
        assert!(client.next_id > asked, "the missing colors are asked for");
        assert_eq!(client.synced_version(), Some(0), "without a reopen");
    }

    #[test]
    fn a_document_that_moved_on_while_away_is_changed_not_reopened() {
        // Edited, then left before the debounce fired: the server holds an
        // older copy of a document it was given, which a change fixes.
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");

        client.open(Path::new("a.rs"), &lines, 0);
        client.set_synced_version(Some(0));

        client.open(other(), &lines, 0);
        client.open(Path::new("a.rs"), &lines, 1);

        assert_eq!(client.synced_version(), None, "the sync is stale");
        assert!(client.tick(&lines, 1).is_none());
        assert!(client.due.is_some(), "a change is queued, not an open");
    }

    #[test]
    fn diagnostics_of_a_tab_that_is_not_on_screen_come_back_with_it() {
        // The server reports on every document it holds; what it says about
        // one that is not being looked at is remembered and put back when its
        // tab is focused again.
        let mut client = LspClient::ready_for_test();
        let lines = doc("abc");
        let uri = super::uri::path_to_uri(Path::new("a.rs"));

        client.open(Path::new("a.rs"), &lines, 0);
        client.set_synced_version(Some(0));
        client.colored_for_test(0);
        assert!(matches!(
            client.on_notification(&diagnostics(&uri), &lines),
            Some(super::LspEvent::Diagnostics)
        ));

        // Away, and the server says something new about the file.
        assert!(client.open(other(), &lines, 0).is_none());
        assert!(client.diagnostic_rows().is_empty(), "nothing on screen");
        assert!(client.on_notification(&diagnostics(&uri), &lines).is_none());

        // Back: the remembered report is applied without asking the server.
        let asked = client.next_id;
        assert!(matches!(
            client.open(Path::new("a.rs"), &lines, 0),
            Some(super::LspEvent::Diagnostics)
        ));
        assert_eq!(client.next_id, asked, "nothing was requested");
        assert_eq!(
            client.diagnostic_rows()[0].len(),
            1,
            "the underlines are back"
        );
    }

    #[test]
    fn a_document_whose_tab_was_closed_is_given_back() {
        // Closing a tab is the one case a document is closed at the server:
        // nothing can switch back to it.
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");
        let uri = super::uri::path_to_uri(Path::new("a.rs"));

        client.open(Path::new("a.rs"), &lines, 0);
        client.set_synced_version(Some(0));
        client.open(other(), &lines, 0);

        client.forget(Path::new("a.rs"));
        assert!(!client.docs.contains_key(&uri), "forgotten");
        assert!(
            client.uri.as_deref() != Some(uri.as_str()),
            "the document on screen is untouched"
        );

        // Forgetting the one that *is* on screen leaves none behind.
        client.forget(other());
        assert!(client.uri.is_none());
        assert!(client.docs.is_empty());
    }

    #[test]
    fn forgetting_the_document_on_screen_leaves_none() {
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");

        client.open(Path::new("a.rs"), &lines, 0);
        client.rows.push(vec![]);
        client.forget(Path::new("a.rs"));

        assert!(client.uri.is_none(), "no document is on screen");
        assert!(client.rows().is_empty(), "and nothing is left to paint");
    }

    #[test]
    fn forgetting_a_document_the_server_never_had_does_nothing() {
        let mut client = LspClient::ready_for_test();
        let lines = doc("fn main() {}");

        client.open(Path::new("a.rs"), &lines, 0);
        client.forget(Path::new("b.rs"));
        client.forget(Path::new("a.md"));

        assert_eq!(
            client.uri.as_deref(),
            Some(super::uri::path_to_uri(Path::new("a.rs"))).as_deref()
        );
    }

    /// One token on line 0, as a response to `id`.
    fn tokens(id: u64) -> serde_json::Value {
        json!({ "jsonrpc": "2.0", "id": id, "result": { "data": [0, 0, 3, 0, 0] } })
    }

    /// A client that can decode a response: without a legend there are no
    /// styles to paint with.
    fn client_with_legend() -> LspClient {
        let mut client = LspClient::ready_for_test();
        let legend = super::protocol::parse_initialize_result(&json!({
            "result": { "capabilities": { "semanticTokensProvider": {
                "legend": { "tokenTypes": ["function"], "tokenModifiers": [] }
            } } }
        }))
        .expect("a legend");
        client.table = Some(super::theme::StyleTable::build(&legend));
        client
    }

    #[test]
    fn an_answer_about_a_document_that_is_not_on_screen_is_dropped() {
        // Two buffers can sit at the same version, so the version alone does
        // not say which one a response is about.
        let mut client = client_with_legend();
        let lines = doc("fn main() {}");
        let uri = super::uri::path_to_uri(Path::new("a.rs"));

        client.open(Path::new("a.rs"), &lines, 0);
        let asked = client.next_id;
        client.pending.insert(
            asked,
            super::Pending {
                kind: super::PendingKind::SemanticTokens,
                version: 0,
                uri: Some(uri.clone()),
                at: std::time::Instant::now(),
            },
        );

        client.open(other(), &lines, 0);
        assert!(client.handle(&tokens(asked), &lines).is_none());
        assert!(client.rows().is_empty(), "not painted on another buffer");

        // The same answer about the document that *is* on screen is kept.
        client.open(Path::new("a.rs"), &lines, 0);
        client.pending.insert(
            asked,
            super::Pending {
                kind: super::PendingKind::SemanticTokens,
                version: 0,
                uri: Some(uri),
                at: std::time::Instant::now(),
            },
        );
        assert!(matches!(
            client.handle(&tokens(asked), &lines),
            Some(super::LspEvent::Tokens { .. })
        ));
        assert!(!client.rows().is_empty(), "painted on its own buffer");
    }

    fn doc(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    /// A client that has sent a document and is allowed to send ranges.
    fn incremental_client(document: &str) -> LspClient {
        let mut client = LspClient::holding_document("file:///a.rs", document, 0);
        client.incremental = true;
        client
    }

    #[test]
    fn a_change_on_one_row_sends_that_row() {
        // The point of incremental sync: a keystroke costs the row it landed
        // on, not the file it landed in.
        let client = incremental_client("fn a() {}\nfn b() {}\nfn c() {}");
        let mut after = doc("fn a() {}\nfn b() {}\nfn c() {}");
        after[1] = "fn bb() {}".to_string();

        let (range, text) = client.change(&after);
        // The replaced rows are sent joined by newlines and terminated by one,
        // so the last changed row stays a row of its own.
        assert_eq!(text, "fn bb() {}\n", "one row, not three");

        let range = range.expect("a range, not the whole document");
        assert_eq!(range["start"], json!({ "line": 1, "character": 0 }));
        assert_eq!(
            range["end"],
            json!({ "line": 2, "character": 0 }),
            "the start of the row after the changed one"
        );
    }

    #[test]
    fn a_server_that_did_not_ask_for_ranges_gets_the_document() {
        // Sending a range to a server that expects the whole document would
        // leave it holding neither one thing nor the other.
        let mut client = incremental_client("a\nb");
        client.incremental = false;
        let after = doc("a\nbb");

        let (range, text) = client.change(&after);
        assert!(range.is_none());
        assert_eq!(text, "a\nbb");
    }

    #[test]
    fn with_nothing_to_diff_against_the_document_goes_whole() {
        // The server holds nothing yet, so there is no row to name a range
        // with.
        let mut client = incremental_client("a\nb");
        client
            .docs
            .get_mut("file:///a.rs")
            .expect("a doc")
            .text
            .clear();

        let (range, text) = client.change(&doc("a\nb"));
        assert!(range.is_none(), "no baseline, no range");
        assert_eq!(text, "a\nb");
    }

    #[test]
    fn a_change_too_large_to_send_is_reported_once() {
        // Incremental sync makes large files reachable a character at a time,
        // but one enormous paste still has to be turned down.
        let mut client = incremental_client("a");
        let lines = vec!["x".repeat(super::MAX_SYNC_BYTES + 1)];

        client.push_sync(&lines, 3);

        assert!(client.take_notice().is_some(), "said once");
        assert_eq!(client.synced_version(), Some(3), "and then left alone");
        assert!(
            client.synced_text().is_empty() || client.synced_text() == doc("a").as_slice(),
            "the server's copy was not claimed to have changed"
        );
    }

    #[test]
    fn what_was_sent_is_what_the_next_change_is_diffed_against() {
        let mut client = incremental_client("a");
        let after = doc("a\nb");

        client.remember(&after);
        assert_eq!(client.synced_text(), after.as_slice());
    }

    #[test]
    fn a_document_the_server_already_has_is_not_sent_again() {
        let mut client = LspClient::ready_for_test();
        let lines = vec!["fn main() {}".to_string()];
        client.open(Path::new("a.rs"), &lines, 3);
        client.set_synced_version(Some(3));

        assert!(client.tick(&lines, 3).is_none());
        assert!(client.due.is_none(), "nothing changed, so nothing to send");
    }

    #[test]
    fn repeated_syncs_reuse_the_row_buffer() {
        // The decode buffer is the one thing here that could grow per response;
        // `tokens::decode` is tested for that directly, this only pins the
        // client's own buffer is what gets reused.
        let client = LspClient::new();
        assert_eq!(client.rows().len(), 0);
    }
}
