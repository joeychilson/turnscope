//! Reading JSON Lines files that are only ever appended to, from where the last
//! read stopped.
//!
//! Each line is copied out of the read buffer before it is handed on. Handing
//! out slices of the buffer instead was measured and turned down (2026-09-29,
//! release builds, this Mac's 1,805 logs, 7.52 GB): copying took 3.6 s, and
//! slices 4.7 s where each line's end was found with the standard library's
//! slice search, or 3.5 s with the `memchr` crate's, too little to add a
//! dependency for. The search for each line's end and the reads from the
//! system are what take the time, not the copy.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::error::{Error, Result};

/// How much of a file is read at a time.
const BUFFER: usize = 1 << 20;

/// How many of a log's first bytes are kept to tell whether it was rewritten.
const OPENING: u64 = 256;

/// How many of the bytes before where a read of a log stopped are kept, to
/// tell whether what was read was rewritten since.
const CLOSING: u64 = 256;

/// Up to the first 256 bytes of `path`, which a log that is only appended to
/// keeps however it grows.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be opened or read.
pub(crate) fn opening(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(OPENING).read_to_end(&mut bytes))
        .map_err(|error| Error::io(path, error))?;
    Ok(bytes)
}

/// Up to 256 bytes of `path` ending at byte `offset`, where a read stopped:
/// fewer when the file is now shorter than `offset`, which then differ from
/// what was kept.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be opened or read.
pub(crate) fn closing(path: &Path, offset: u64) -> Result<Vec<u8>> {
    let fail = |source| Error::io(path, source);
    let start = offset.saturating_sub(CLOSING);
    let mut file = File::open(path).map_err(fail)?;
    file.seek(SeekFrom::Start(start)).map_err(fail)?;
    let mut bytes = Vec::new();
    file.take(offset - start)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    Ok(bytes)
}

/// The longest line read, 64 MiB: over six times the longest any agent
/// here has written, 9.75 MB of a Codex log (2026-09-27). A longer one is
/// passed over, never held whole, and said.
pub(crate) const LONGEST_LINE: u64 = 64 << 20;

/// What a pass over a log's lines came to.
pub(crate) struct Lines {
    /// The offset just past the last complete line, where the next pass
    /// starts.
    pub end: u64,
    /// Each line longer than [`LONGEST_LINE`], by the offset it starts at.
    pub overlong: Vec<u64>,
}

/// Pass each complete line of `path` from byte `from` to `visit`, with the
/// byte offset it starts at, and say where the last complete line ended and
/// which lines were too long to read.
///
/// A line is complete once its newline is written. A last line without one is
/// still being written, so it is left for the next read, which starts at the
/// returned offset. Blank lines are passed over, and so are lines longer than
/// [`LONGEST_LINE`], which are read past without being kept. `from` must be
/// the start of a line; [`appended`] checks that a saved offset still is.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be opened or read.
pub(crate) fn each_line(path: &Path, from: u64, visit: impl FnMut(&[u8], u64)) -> Result<Lines> {
    lines_up_to(path, from, LONGEST_LINE, visit)
}

/// [`each_line`], with lines longer than `longest` passed over.
fn lines_up_to(
    path: &Path,
    from: u64,
    longest: u64,
    mut visit: impl FnMut(&[u8], u64),
) -> Result<Lines> {
    let fail = |source| Error::io(path, source);
    let mut file = File::open(path).map_err(fail)?;
    file.seek(SeekFrom::Start(from)).map_err(fail)?;
    let mut reader = BufReader::with_capacity(BUFFER, file);
    let mut line = Vec::new();
    let mut offset = from;
    let mut overlong = Vec::new();
    loop {
        line.clear();
        // At most the longest line, so a line with no end in sight can't
        // take all there is. Each read is at most that, so it fits a `u64`.
        let read = (&mut reader)
            .take(longest)
            .read_until(b'\n', &mut line)
            .map_err(fail)? as u64;
        if read == 0 {
            return Ok(Lines {
                end: offset,
                overlong,
            });
        }
        if line.last() == Some(&b'\n') {
            let start = offset;
            offset = offset.saturating_add(read);
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            if !text.iter().all(u8::is_ascii_whitespace) {
                visit(text, start);
            }
            continue;
        }
        if read < longest {
            // The last line, still being written.
            return Ok(Lines {
                end: offset,
                overlong,
            });
        }
        // Longer than the longest: read on to its end, keeping none of it.
        let mut length = read;
        loop {
            line.clear();
            let more = (&mut reader)
                .take(BUFFER as u64)
                .read_until(b'\n', &mut line)
                .map_err(fail)? as u64;
            if more == 0 {
                // Still being written: the next read looks at it again.
                return Ok(Lines {
                    end: offset,
                    overlong,
                });
            }
            length = length.saturating_add(more);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        overlong.push(offset);
        offset = offset.saturating_add(length);
        // Let go of what the long line took.
        line = Vec::new();
    }
}

/// The whole of the JSON document at `path`, or `None` when it is longer
/// than [`LONGEST_LINE`], as no record is: it is not held to find out.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be opened or read.
pub(crate) fn document(path: &Path) -> Result<Option<Vec<u8>>> {
    document_up_to(path, LONGEST_LINE)
}

/// [`document`], with documents longer than `longest` passed over.
fn document_up_to(path: &Path, longest: u64) -> Result<Option<Vec<u8>>> {
    let fail = |source| Error::io(path, source);
    let mut bytes = Vec::new();
    // One byte more than the longest, to tell a document just that long
    // from a longer one, however it grows meanwhile.
    File::open(path)
        .and_then(|file| file.take(longest.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(fail)?;
    Ok((bytes.len() as u64 <= longest).then_some(bytes))
}

/// Whether the log at `path`, `size` bytes long now and longer than it was,
/// has only been appended to since it opened with `opening` and was read to
/// `offset`, which `closing` ended: it still opens with those bytes, `offset`
/// still falls at the start of a line, and the bytes before it are still
/// `closing`, when they were kept.
///
/// Both ends of what was read are checked, not all of it, which would read
/// the whole log again for each line added: a log cut short and written past
/// where it ended, or rewritten at its start or its end, is seen; one rewritten
/// only in between, and grown as well, is not. A log that changed without
/// growing was rewritten, which the caller, knowing its size before, tells.
///
/// # Errors
///
/// Returns [`Error::Io`] when the log cannot be read.
pub(crate) fn appended(
    path: &Path,
    opening: &[u8],
    closing: Option<&[u8]>,
    size: u64,
    offset: u64,
) -> Result<bool> {
    Ok(self::opening(path)?.starts_with(opening)
        && size >= offset
        && starts_line(path, offset)?
        && match closing {
            Some(closing) => self::closing(path, offset)? == closing,
            None => true,
        })
}

/// Whether `offset` is the start of a line of `path`: the start of the file,
/// or just after a newline.
///
/// A saved offset that no longer is one means the file was rewritten, and it
/// must be read again from the start.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be opened or read.
fn starts_line(path: &Path, offset: u64) -> Result<bool> {
    let Some(before) = offset.checked_sub(1) else {
        return Ok(true);
    };
    let fail = |source| Error::io(path, source);
    let mut file = File::open(path).map_err(fail)?;
    file.seek(SeekFrom::Start(before)).map_err(fail)?;
    let mut byte = [0u8; 1];
    match file.read_exact(&mut byte) {
        Ok(()) => Ok(byte[0] == b'\n'),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(fail(error)),
    }
}

/// Folds of logs read in order into a state, the last few kept to read on
/// from: a running session's conversation grows a line at a time, and reading
/// it whole again for each line read everything again. Measured 2026-09-24,
/// reading on took 1 ms where reading whole took about 100 for 3,575 entries
/// of Claude Code's, and 9 ms where it took 1 s for a 336 MB Codex thread.
/// Each reader's tests check that reading on comes to what reading whole
/// does.
pub(crate) struct Folds<S> {
    kept: Mutex<Vec<Fold<S>>>,
}

/// How many folds are kept: the conversation being read, and one more, as
/// the session a subagent was opened from.
const FOLDS_KEPT: usize = 2;

/// A fold of logs, as far as it read them.
struct Fold<S> {
    logs: Vec<Log>,
    state: S,
}

/// One log as a fold last read it.
struct Log {
    path: PathBuf,
    /// Its device and inode, which change when it is replaced.
    file: (u64, u64),
    /// Its first bytes, which change when it is rewritten.
    opening: Vec<u8>,
    /// The bytes before `offset`, which change when what was read is
    /// rewritten.
    closing: Vec<u8>,
    /// How long it was.
    size: u64,
    /// When it was last modified, which changes with any write.
    modified: Option<SystemTime>,
    /// Where its last complete line ended.
    offset: u64,
}

impl<S> Default for Folds<S> {
    fn default() -> Self {
        Folds {
            kept: Mutex::new(Vec::new()),
        }
    }
}

impl<S: Default> Folds<S> {
    /// Fold every complete line of `paths`, in order, into a state, calling
    /// `begin` before a log is read from its start and `line` for each line,
    /// and give what `finish` makes of the state.
    ///
    /// A fold kept of the same logs reads on from where it stopped when they
    /// have only grown since: each log it read is the same file, opening as
    /// it did, those before the last unchanged, and the last grown if changed
    /// at all, with where it stopped still the start of a line and ending as
    /// it did (see [`appended`]); any others come after them. What reading on
    /// comes to is what folding anew would.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when a log cannot be read; nothing is kept then.
    pub(crate) fn fold<T>(
        &self,
        paths: &[&Path],
        mut begin: impl FnMut(&mut S),
        mut line: impl FnMut(&mut S, &[u8]),
        finish: impl FnOnce(&S) -> T,
    ) -> Result<T> {
        let Some(first) = paths.first() else {
            return Ok(finish(&S::default()));
        };
        let earlier = {
            let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            let at = kept
                .iter()
                .position(|fold| fold.logs.first().is_some_and(|log| log.path == *first));
            at.map(|at| kept.remove(at))
        };
        let mut fold = match earlier {
            Some(fold) if fold.grown(paths)? => fold,
            _ => Fold {
                logs: Vec::new(),
                state: S::default(),
            },
        };
        // The last log read reads on; any after it are read whole.
        let reading_on = fold.logs.len().saturating_sub(1);
        for (index, path) in paths.iter().enumerate().skip(reading_on) {
            let file = std::fs::metadata(path).map_err(|error| Error::io(path, error))?;
            let opening = opening(path)?;
            let from = match fold.logs.get(index) {
                Some(log) => log.offset,
                None => {
                    begin(&mut fold.state);
                    0
                }
            };
            let state = &mut fold.state;
            let offset = each_line(path, from, |text, _| line(state, text))?.end;
            let log = Log {
                path: path.to_path_buf(),
                file: (file.dev(), file.ino()),
                opening,
                closing: closing(path, offset)?,
                size: file.len(),
                modified: file.modified().ok(),
                offset,
            };
            match fold.logs.get_mut(index) {
                Some(kept) => *kept = log,
                None => fold.logs.push(log),
            }
        }
        let made = finish(&fold.state);
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.insert(0, fold);
        kept.truncate(FOLDS_KEPT);
        Ok(made)
    }
}

impl<S> Fold<S> {
    /// Whether `paths` are the logs it read, grown at most, and perhaps
    /// others after them.
    fn grown(&self, paths: &[&Path]) -> Result<bool> {
        if self.logs.is_empty() || self.logs.len() > paths.len() {
            return Ok(false);
        }
        let last = self.logs.len() - 1;
        for (index, log) in self.logs.iter().enumerate() {
            let path = paths[index];
            if path != log.path {
                return Ok(false);
            }
            let file = match std::fs::metadata(path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(Error::io(path, error)),
            };
            let unchanged = file.len() == log.size && file.modified().ok() == log.modified;
            let same = (file.dev(), file.ino()) == log.file
                && if index == last {
                    // Changed without growing, it was rewritten.
                    (unchanged || file.len() != log.size)
                        && appended(
                            path,
                            &log.opening,
                            Some(&log.closing),
                            file.len(),
                            log.offset,
                        )?
                } else {
                    unchanged && opening(path)?.starts_with(&log.opening)
                };
            if !same {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{document_up_to, each_line, lines_up_to, starts_line};

    fn lines_from(path: &std::path::Path, from: u64) -> (Vec<(String, u64)>, u64) {
        let mut seen = Vec::new();
        let end = each_line(path, from, |line, start| {
            seen.push((String::from_utf8(line.to_vec()).unwrap(), start));
        })
        .unwrap()
        .end;
        (seen, end)
    }

    #[test]
    fn a_line_still_being_written_is_left_for_the_next_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(&path, "{\"a\":1}\n\n{\"b\":2}\r\n{\"c\":").unwrap();

        let (seen, end) = lines_from(&path, 0);
        // `{"a":1}\n` is 8 bytes, the blank line 1, and `{"b":2}\r\n` 9.
        assert_eq!(seen, [("{\"a\":1}".into(), 0), ("{\"b\":2}".into(), 9)]);
        assert_eq!(end, 18);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"3}\n").unwrap();
        let (seen, end) = lines_from(&path, end);
        assert_eq!(seen, [("{\"c\":3}".into(), 18)]);
        assert_eq!(end, 26);
    }

    #[test]
    fn an_offset_inside_a_line_is_not_a_line_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        assert!(starts_line(&path, 0).unwrap());
        assert!(starts_line(&path, 8).unwrap());
        assert!(!starts_line(&path, 5).unwrap());
        // Past the end of a file that has since shrunk.
        assert!(!starts_line(&path, 100).unwrap());
    }

    #[test]
    fn a_line_longer_than_the_longest_is_passed_over_and_said() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.jsonl");
        // "ab\n" is 3 bytes; forty x and a newline, 41, end at 44; "cd\n"
        // ends at 47; "yy" is still being written.
        let mut text = String::from("ab\n");
        text.push_str(&"x".repeat(40));
        text.push_str("\ncd\nyy");
        std::fs::write(&path, &text).unwrap();
        let mut seen = Vec::new();
        let lines = lines_up_to(&path, 0, 16, |line, start| {
            seen.push((String::from_utf8(line.to_vec()).unwrap(), start));
        })
        .unwrap();
        assert_eq!(seen, [("ab".to_owned(), 0), ("cd".to_owned(), 44)]);
        // Passed over whole: "cd" starts past its 41 bytes.
        assert_eq!(lines.overlong, [3]);
        assert_eq!(lines.end, 47);

        // One too long and not yet ended waits, with nothing said of it.
        std::fs::write(&path, format!("ab\n{}", "x".repeat(40))).unwrap();
        let lines = lines_up_to(&path, 0, 16, |_, _| {}).unwrap();
        assert_eq!((lines.end, lines.overlong.len()), (3, 0));
    }

    #[test]
    fn a_document_longer_than_the_longest_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.json");
        std::fs::write(&path, "{\"a\": 1}").unwrap();
        // Eight bytes, { " a " : space 1 }: within ten and within eight, not
        // within seven.
        assert_eq!(
            document_up_to(&path, 10).unwrap().as_deref(),
            Some(&b"{\"a\": 1}"[..])
        );
        assert_eq!(
            document_up_to(&path, 8).unwrap().map(|bytes| bytes.len()),
            Some(8)
        );
        assert_eq!(document_up_to(&path, 7).unwrap(), None);
    }
}
