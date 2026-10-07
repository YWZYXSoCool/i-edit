//! The stdio transport: a subprocess, a reader thread and a writer thread.
//!
//! Both threads are mandatory, and the reason is not symmetry:
//!
//! - **The reader** must drain the server's stdout continuously. A server whose
//!   output pipe fills up stops writing, and a server that has stopped writing
//!   is not reading our requests either — that is a deadlock, and it is the one
//!   failure in this design that hangs the editor rather than degrading it.
//! - **The writer** exists so a large `didChange` payload never blocks the UI.
//!   Writing to a pipe can block; doing it on the main thread would freeze
//!   input for as long as the server takes to drain. On its own thread the
//!   block is invisible.
//!
//! Nothing here knows what a message means. Framing and moving bytes is all it
//! does; [`crate::lsp::protocol`] builds them and the client interprets them.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::thread::JoinHandle;

use log::warn;
use serde_json::Value;

/// How many messages may queue for the writer before the sender blocks.
/// Requests are small and infrequent (one per 150 ms of typing at most), so a
/// small bound is plenty and keeps a wedged server from accumulating work.
const OUTBOUND_QUEUE: usize = 32;

/// A message that came back from the server.
#[derive(Debug)]
pub enum Inbound {
    /// A decoded JSON-RPC message.
    Json(Value),
    /// The server closed its stdout, died, or the reader gave up. The client
    /// treats this as "no more colors, ever" for this session.
    Eof,
}

/// Wraps one LSP body in its `Content-Length` header.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 32);
    out.extend_from_slice(b"Content-Length: ");
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\n\r\n");
    out.extend_from_slice(body);
    out
}

/// Reads `Content-Length` out of one header line.
///
/// The name is compared case-insensitively because the spec writes it
/// `Content-Length` but servers have been seen to lowercase it.
fn content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse().ok()
}

/// Reads whole LSP bodies from any byte stream.
///
/// Generic over `Read` so the frame handling can be tested against a byte
/// slice — including the two cases that only show up on a real pipe: several
/// messages arriving in one read, and one message arriving in several.
struct FrameReader<R: Read> {
    reader: BufReader<R>,
}

impl<R: Read> FrameReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }

    /// `Ok(None)` is a clean end of stream between messages.
    fn read_body(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let mut length: Option<usize> = None;

        loop {
            let mut line = Vec::new();
            if self.reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(None);
            }

            let text = String::from_utf8_lossy(&line);
            match text.trim() {
                "" => break, // blank line: headers are over
                text => {
                    if let Some(value) = content_length(text) {
                        length = Some(value);
                    }
                }
            }
        }

        // A message with no Content-Length cannot be read; skipping it is
        // better than stopping, since the stream stays in sync.
        let Some(length) = length else {
            return Ok(None);
        };

        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body)?;
        Ok(Some(body))
    }
}

fn read_loop(mut frames: FrameReader<ChildStdout>, out: Sender<Inbound>) {
    loop {
        match frames.read_body() {
            Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                Ok(value) => {
                    if out.send(Inbound::Json(value)).is_err() {
                        return; // the client is gone
                    }
                }
                Err(err) => {
                    // A malformed message is dropped, never propagated: the
                    // server stays in sync because the frame was framed, and
                    // one bad message must not cost the whole session.
                    warn!("lsp: unreadable message, dropping it: {err}");
                }
            },
            Ok(None) => {
                let _ = out.send(Inbound::Eof);
                return;
            }
            Err(err) => {
                warn!("lsp: stopped reading stdout: {err}");
                let _ = out.send(Inbound::Eof);
                return;
            }
        }
    }
}

fn write_loop(mut stdin: ChildStdin, incoming: Receiver<Vec<u8>>) {
    while let Ok(bytes) = incoming.recv() {
        // `flush` is not optional: without it a request can sit in the
        // BufWriter until the next one pushes it out, and the server appears
        // to ignore us.
        if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
            return;
        }
    }
}

/// A running language server, seen purely as a byte pipe.
pub struct Transport {
    child: Option<Child>,
    outbound: Option<SyncSender<Vec<u8>>>,
    inbound: Receiver<Inbound>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Transport {
    /// Starts `program` as a language server.
    ///
    /// The child's stderr is discarded: servers write diagnostics and progress
    /// there, and we have no place to show it.
    pub fn spawn(program: &str) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(std::io::Error::other("server did not open stdio"));
        };

        let (in_tx, in_rx) = channel::<Inbound>();
        let (out_tx, out_rx) = sync_channel::<Vec<u8>>(OUTBOUND_QUEUE);

        let reader = match std::thread::Builder::new()
            .name("lsp-read".to_string())
            .spawn(move || read_loop(FrameReader::new(stdout), in_tx))
        {
            Ok(handle) => handle,
            Err(err) => {
                let _ = child.kill();
                return Err(err);
            }
        };

        let writer = match std::thread::Builder::new()
            .name("lsp-write".to_string())
            .spawn(move || write_loop(stdin, out_rx))
        {
            Ok(handle) => handle,
            Err(err) => {
                let _ = child.kill();
                return Err(err);
            }
        };

        Ok(Self {
            child: Some(child),
            outbound: Some(out_tx),
            inbound: in_rx,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    /// Serializes and queues one message. `false` means the pipe is gone; the
    /// caller shuts the client down rather than retrying.
    pub fn send(&self, message: &Value) -> bool {
        let Some(outbound) = self.outbound.as_ref() else {
            return false;
        };
        let Ok(body) = serde_json::to_vec(message) else {
            return false;
        };
        outbound.send(frame(&body)).is_ok()
    }

    /// The next message, if one has arrived. Never blocks: this is called once
    /// per frame from the UI loop.
    pub fn try_recv(&self) -> Option<Inbound> {
        match self.inbound.try_recv() {
            Ok(message) => Some(message),
            Err(TryRecvError::Empty) => None,
            // The reader thread ended without saying goodbye.
            Err(TryRecvError::Disconnected) => Some(Inbound::Eof),
        }
    }

    /// Stops the server and joins both threads.
    ///
    /// Order matters: the sender is dropped first (which ends the writer), the
    /// child is killed (which ends the reader on EOF), and only then are the
    /// threads joined. Joining before that could wait forever.
    pub fn shutdown(&mut self) {
        self.outbound = None;

        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        for handle in [self.reader.take(), self.writer.take()]
            .into_iter()
            .flatten()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameReader, content_length, frame};
    use serde_json::json;
    use std::io::{ErrorKind, Read};

    fn read_all(bytes: &[u8]) -> Vec<serde_json::Value> {
        let mut reader = FrameReader::new(bytes);
        let mut out = Vec::new();
        while let Some(body) = reader.read_body().expect("read") {
            out.push(serde_json::from_slice(&body).expect("json"));
        }
        out
    }

    #[test]
    fn frame_writes_the_exact_byte_count() {
        let body = br#"{"jsonrpc":"2.0"}"#;
        let framed = frame(body);
        let text = String::from_utf8(framed.clone()).expect("utf8");

        let (header, rest) = text.split_once("\r\n\r\n").expect("header");
        assert_eq!(header, format!("Content-Length: {}", body.len()));
        assert_eq!(rest.as_bytes(), body);
        assert_eq!(framed.len(), header.len() + 4 + body.len());
    }

    #[test]
    fn frame_round_trips_through_the_reader() {
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "x" });
        let bytes = frame(&serde_json::to_vec(&message).unwrap());
        assert_eq!(read_all(&bytes), vec![message]);
    }

    #[test]
    fn two_messages_in_one_read_are_split_apart() {
        // The common case on a pipe: the server wrote twice, we read once.
        let a = json!({ "id": 1 });
        let b = json!({ "id": 2 });
        let mut bytes = frame(&serde_json::to_vec(&a).unwrap());
        bytes.extend_from_slice(&frame(&serde_json::to_vec(&b).unwrap()));

        assert_eq!(read_all(&bytes), vec![a, b]);
    }

    #[test]
    fn one_message_spread_over_many_reads_reassembles() {
        // The other case: a large body dribbles in. `Chunked` hands out a
        // single byte per call, which is the worst the pipe can do.
        struct Chunked<'a> {
            data: &'a [u8],
            pos: usize,
        }
        impl Read for Chunked<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.data.len() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.data[self.pos];
                self.pos += 1;
                Ok(1)
            }
        }

        let message = json!({ "result": { "data": [1, 2, 3, 4, 5] } });
        let bytes = frame(&serde_json::to_vec(&message).unwrap());

        let mut reader = FrameReader::new(Chunked {
            data: &bytes,
            pos: 0,
        });
        let body = reader.read_body().expect("read").expect("body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            message
        );
    }

    #[test]
    fn content_length_is_case_insensitive_and_tolerates_spacing() {
        assert_eq!(content_length("Content-Length: 42"), Some(42));
        assert_eq!(content_length("content-length: 42"), Some(42));
        assert_eq!(content_length("CONTENT-LENGTH:  42 "), Some(42));
        // Other headers are not ours to read and are simply skipped.
        assert_eq!(content_length("Content-Type: application/json"), None);
        assert_eq!(content_length("Content-Length: not-a-number"), None);
    }

    #[test]
    fn unknown_headers_still_deliver_the_body() {
        // `Content-Type` is skipped; only `Content-Length` is read (7 bytes).
        let bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
                      Content-Length: 7\r\n\
                      \r\n\
                      {\"a\":1}";

        assert_eq!(read_all(bytes), vec![json!({ "a": 1 })]);
    }

    #[test]
    fn a_truncated_stream_stops_cleanly() {
        // Headers promise 100 bytes, only 2 arrive: an error, but a bounded
        // one — the reader must not loop or panic.
        let bytes = b"Content-Length: 100\r\n\r\n{}";
        let mut reader = FrameReader::new(&bytes[..]);
        let err = reader.read_body().expect_err("should fail");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn an_empty_stream_is_not_an_error() {
        assert!(read_all(b"").is_empty());
    }
}
