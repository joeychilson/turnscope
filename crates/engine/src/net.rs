//! Requests to the network, made by `/usr/bin/curl`.
//!
//! The engine asks the network for two things: models.dev's catalog, and
//! limits from their providers: each subscription's, and the one an
//! OpenRouter API key carries, from OpenRouter. Going through the system's curl
//! keeps an HTTP client and TLS stack out of the binary and trusts what the
//! system trusts. `-q`, which must come first, keeps a `~/.curlrc` from adding
//! options such as a trace that would record what is sent.
//!
//! Every header goes to curl on its standard input, never its arguments or
//! its environment, so a credential among them appears in no process
//! listing. The body comes back on curl's standard output, and the status and
//! entity tag on its standard error, so nothing is written to disk.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The system's curl.
const CURL: &str = "/usr/bin/curl";

/// What curl writes to standard error once a request is done, on a line of
/// its own after anything it said: the status, and the entity tag, empty
/// when there is none. `%header{…}` needs curl 7.84; macOS 26, the oldest the
/// app runs on, has 8.7.
const WRITTEN_OUT: &str = "%{stderr}\n%{http_code} %header{etag}";

/// Where a request goes: one of the two kinds of destination the engine
/// asks, each asked in its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    /// models.dev's catalog: about five megabytes, asked without a
    /// credential, and followed wherever it moves over HTTPS.
    Catalog,
    /// A provider's usage endpoint: a small answer, asked with a credential,
    /// and never followed elsewhere, since curl would carry the headers
    /// beside the credential, such as an account's id, to wherever a
    /// redirect points.
    Usage,
}

impl Destination {
    /// The longest a request may take, in seconds.
    fn timeout(self) -> &'static str {
        match self {
            Destination::Catalog => "60",
            Destination::Usage => "20",
        }
    }

    /// The largest body accepted, in bytes: well beyond what the answer is.
    fn largest(self) -> &'static str {
        match self {
            Destination::Catalog => "67108864",
            Destination::Usage => "1048576",
        }
    }
}

/// What a request answered.
#[derive(Debug)]
pub(crate) struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The entity tag the answer carries, to ask next time whether it changed,
    /// when it is one that can be sent back.
    pub etag: Option<String>,
    /// The body.
    pub body: Vec<u8>,
}

/// Why a request got no answer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NetError {
    /// curl could not be run.
    #[error("could not run curl: {0}")]
    Spawn(std::io::Error),
    /// A header held a line break, which would let it add headers of its
    /// own, so nothing was sent.
    #[error("a header holds a line break, so nothing was sent")]
    Header,
    /// curl ran and failed, as it does when offline.
    #[error("curl failed ({code}): {message}")]
    Failed {
        /// Its exit code.
        code: i32,
        /// What it said.
        message: String,
    },
    /// The request or its answer could not be made sense of.
    #[error("could not read the answer: {0}")]
    Answer(String),
}

/// Ask `url`, a `destination`, over HTTPS with `headers`, a credential among
/// them or not, and give back what it answered.
///
/// A header holding a line break, which would let it add headers of its own,
/// is refused before anything is sent.
pub(crate) fn get(
    destination: Destination,
    url: &str,
    headers: &[String],
) -> Result<Answer, NetError> {
    if headers.iter().any(|header| header.contains(['\r', '\n'])) {
        return Err(NetError::Header);
    }
    let mut command = Command::new(CURL);
    command
        .arg("-q")
        .args(["--silent", "--show-error", "--compressed"])
        .args(["--proto", "=https"])
        .args(["--max-time", destination.timeout()])
        .args(["--max-filesize", destination.largest()])
        .args(["--user-agent", "Turnscope"])
        .args(["--header", "Accept: application/json"])
        .args(["--header", "@-"])
        .args(["--write-out", WRITTEN_OUT]);
    if destination == Destination::Catalog {
        command.args(["--location", "--proto-redir", "=https"]);
    }
    let mut curl = command
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(NetError::Spawn)?;
    let request: String = headers.iter().map(|header| format!("{header}\n")).collect();
    // curl reads the headers before it connects, and they are far smaller
    // than a pipe holds, so writing them all first can't wait on its output.
    let sent = curl
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(request.as_bytes()).is_ok());
    let output = curl.wait_with_output().map_err(NetError::Spawn)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (said, written) = written_out(&stderr);
    if !sent || !output.status.success() {
        return Err(NetError::Failed {
            code: output.status.code().unwrap_or(-1),
            message: said.to_owned(),
        });
    }
    let (status, etag) = written;
    Ok(Answer {
        status: status
            .parse()
            .map_err(|error| NetError::Answer(format!("status {status:?}: {error}")))?,
        etag: Some(etag).filter(|etag| safe(etag)).map(str::to_owned),
        body: output.stdout,
    })
}

/// What curl said on standard error before what [`WRITTEN_OUT`] has it
/// write, and that: the status and the entity tag, as text.
fn written_out(stderr: &str) -> (&str, (&str, &str)) {
    let (said, last) = stderr.rsplit_once('\n').unwrap_or(("", stderr));
    let (status, etag) = last.split_once(' ').unwrap_or((last, ""));
    (said.trim(), (status.trim(), etag.trim()))
}

/// Whether `etag` can be sent back in a header: printable ASCII only, so it
/// cannot end the header and start another.
fn safe(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 256 && etag.bytes().all(|byte| (0x20..0x7f).contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::{Destination, NetError, get, safe, written_out};

    #[test]
    fn the_status_and_entity_tag_follow_what_curl_says() {
        // As models.dev answered on 2026-09-26, and then as curl fails
        // offline, when the status it writes is 000.
        assert_eq!(
            written_out("\n200 W/\"a8fdff8480a40bc94c91a1fa9705ce98\""),
            ("", ("200", "W/\"a8fdff8480a40bc94c91a1fa9705ce98\""))
        );
        assert_eq!(written_out("\n304 "), ("", ("304", "")));
        assert_eq!(
            written_out("curl: (6) Could not resolve host: models.dev\n\n000 "),
            ("curl: (6) Could not resolve host: models.dev", ("000", ""))
        );
    }

    #[test]
    fn an_entity_tag_that_could_start_another_header_is_not_kept() {
        assert!(safe("\"1737e0753061fd719ca157d575ca25e0\""));
        assert!(!safe("\"x\"\r\nX-Injected: yes"));
        assert!(!safe(""));
    }

    #[test]
    fn a_header_that_could_add_another_is_refused_before_anything_is_sent() {
        let refused = get(
            Destination::Usage,
            "https://example.invalid/usage",
            &["Authorization: Bearer x\r\nX-Injected: yes".to_owned()],
        );
        assert!(matches!(refused, Err(NetError::Header)));
    }
}
