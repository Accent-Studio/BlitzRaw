//! Logging that cannot stall the application.
//!
//! # Why
//!
//! The logger chained `std::io::stderr()` directly, so every `log::info!` wrote
//! to stderr on whatever thread called it. Under `npm run tauri dev` stderr is
//! a pipe to the Node process that launched the app. A pipe whose reader stops
//! draining fills, and a write to a full pipe **blocks**.
//!
//! So a thread that logged could stop, holding whatever it held, and fern
//! chains its outputs in order, which means the log file stopped getting
//! records too. The visible result was an application that froze partway
//! through: previews stopped arriving, scopes stopped updating, and the window
//! would not close, with a log that ended a few seconds after startup and gave
//! no hint why. Three sessions were spent diagnosing symptoms of this.
//!
//! # What this does
//!
//! Log records go to a bounded channel and one writer thread drains it. The
//! calling thread never touches stderr or the file, so it cannot block on
//! either. If the channel fills, records are dropped and counted rather than
//! made to wait: losing lines from a log is a much smaller problem than
//! stopping the work that was producing them.
//!
//! The file is written before stderr, so a stalled console costs the console
//! and not the record of what happened.

use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};

/// How many records may be waiting before new ones are dropped.
///
/// Large enough to ride out a burst, small enough that a reader which has gone
/// away for good does not hold megabytes of text nobody will read.
const QUEUE_DEPTH: usize = 4096;

/// Records dropped because the queue was full, reported once on the way out.
static DROPPED: AtomicUsize = AtomicUsize::new(0);

/// The `Write` fern is handed. Everything it receives is queued, never written.
pub struct NonBlockingSink {
    tx: SyncSender<Vec<u8>>,
    pending: Vec<u8>,
}

impl NonBlockingSink {
    /// Starts the writer thread and returns the sink to give to fern.
    ///
    /// `file` is optional: a run that could not open one still gets a console.
    pub fn start(file: Option<File>) -> Self {
        let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);

        std::thread::Builder::new()
            .name("log-writer".to_string())
            .spawn(move || {
                let mut file = file;
                while let Ok(line) = rx.recv() {
                    // The file first. A console that has stopped reading is
                    // then only a lost console, not a lost record.
                    if let Some(handle) = file.as_mut() {
                        let _ = handle.write_all(&line);
                        let _ = handle.flush();
                    }
                    let _ = io::stderr().write_all(&line);
                }
            })
            .ok();

        Self {
            tx,
            pending: Vec::with_capacity(256),
        }
    }

    fn queue(&mut self, line: Vec<u8>) {
        if self.tx.try_send(line).is_err() {
            // Said once, on the first drop, and with eprintln rather than the
            // logger: logging about the logger being behind is how a slow log
            // becomes an infinite one.
            if DROPPED.fetch_add(1, Ordering::Relaxed) == 0 {
                eprintln!(
                    "Log records are being dropped: the writer is behind by more than {QUEUE_DEPTH}.                      The application is not waiting for it."
                );
            }
        }
    }
}

impl Write for NonBlockingSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(data);

        // Sent a line at a time, so a record never arrives split across two
        // writes and interleaved with another thread's.
        while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            self.queue(line);
        }

        // Always the whole slice: this cannot fail, and telling the caller it
        // wrote less would make it try again forever.
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.queue(line);
        }
        Ok(())
    }
}

/// How many records have been dropped for being behind. Zero on a healthy run.
pub fn dropped_records() -> usize {
    DROPPED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink whose reader never drains still returns, which is the whole
    /// point: the application thread must not wait on the console.
    #[test]
    fn writing_more_than_the_queue_holds_does_not_block() {
        let (tx, rx) = sync_channel::<Vec<u8>>(4);
        let mut sink = NonBlockingSink {
            tx,
            pending: Vec::new(),
        };

        // Nothing is reading rx. Past four records this would wait forever if
        // the send were not a try_send.
        let before = dropped_records();
        for index in 0..50 {
            writeln!(sink, "record {index}").expect("write must not fail");
        }
        assert!(
            dropped_records() > before,
            "records past the queue depth should be dropped, not waited on"
        );

        // And the ones that fit are intact and whole lines.
        let first = rx.try_recv().expect("the first record is queued");
        assert_eq!(first, b"record 0\n");
    }

    /// fern writes a record and a newline separately, so a line has to be
    /// assembled before it is sent or two threads interleave mid-record.
    #[test]
    fn a_record_split_across_writes_arrives_as_one_line() {
        let (tx, rx) = sync_channel::<Vec<u8>>(8);
        let mut sink = NonBlockingSink {
            tx,
            pending: Vec::new(),
        };

        sink.write_all(b"half a ").expect("write");
        assert!(
            rx.try_recv().is_err(),
            "nothing goes until the line is whole"
        );

        sink.write_all(b"record\n").expect("write");
        assert_eq!(rx.try_recv().expect("queued"), b"half a record\n");
    }

    /// Several lines in one write are several records, not one blob.
    #[test]
    fn a_burst_is_split_back_into_lines() {
        let (tx, rx) = sync_channel::<Vec<u8>>(8);
        let mut sink = NonBlockingSink {
            tx,
            pending: Vec::new(),
        };

        sink.write_all(b"one\ntwo\nthree\n").expect("write");
        assert_eq!(rx.try_recv().expect("queued"), b"one\n");
        assert_eq!(rx.try_recv().expect("queued"), b"two\n");
        assert_eq!(rx.try_recv().expect("queued"), b"three\n");
    }

    /// A record with no trailing newline is not held forever.
    #[test]
    fn a_flush_ships_what_is_left() {
        let (tx, rx) = sync_channel::<Vec<u8>>(8);
        let mut sink = NonBlockingSink {
            tx,
            pending: Vec::new(),
        };

        sink.write_all(b"no newline").expect("write");
        assert!(rx.try_recv().is_err());
        sink.flush().expect("flush");
        assert_eq!(rx.try_recv().expect("queued"), b"no newline");
    }
}
