//! The providers whose accounts' limits Turnscope reads. Each is one module
//! implementing [`Provider`], listed in [`ALL`]. This is Turnscope's only way
//! to the network: `/usr/bin/curl`.

mod anthropic;
mod google;
mod openai;
mod opencode_go;
mod openrouter;
mod xai;

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::Error;
use crate::agents::{Credential, Secret};
use crate::time::{DAY, HOUR};

/// Every provider, in the order they're listed.
pub static ALL: &[&dyn Provider] = &[
    &anthropic::Anthropic,
    &openai::OpenAi,
    &xai::Xai,
    &google::Google,
    &opencode_go::OpenCodeGo,
    &openrouter::OpenRouter,
];

pub fn by_id(id: &str) -> Option<&'static dyn Provider> {
    ALL.iter()
        .copied()
        .find(|provider| provider.info().id == id)
}

/// A provider of models and the accounts that pay for them.
pub trait Provider: Sync {
    fn info(&self) -> &'static Info;
    /// The account `credential` is to, from the credential alone.
    fn account(&self, credential: &Credential) -> Option<Account>;
    /// The limits `credential` reads, asked by `by`: a sign-in's are its
    /// account's, and a key's its own. Never given a key where
    /// `Info::key_limits` is false.
    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem>;
    /// For an API-key account, the limits of the account itself, which
    /// every key reads alike, as credits are: asked once a read, with one
    /// key after another until one isn't refused, as a key may have no
    /// access to them.
    fn account_limits(
        &self,
        _credential: &Credential,
        _now: i64,
        _by: Instant,
    ) -> Result<Vec<Reading>, Problem> {
        Ok(Vec::new())
    }
    /// How long the window of its limit `key` is, when that window rolls:
    /// the use that counts is that of the last so long.
    fn rolls(&self, _key: &str) -> Option<i64> {
        None
    }
    /// For a provider whose keys don't say which plan they're to, what
    /// tells the plan `limits` were read from apart from any other. Accounts
    /// whose limits give the same mark are one plan.
    fn plan_mark(&self, _limits: &Limits) -> Option<i64> {
        None
    }
}

/// What Turnscope knows of a provider.
pub struct Info {
    /// As stored, as agents' logins name it, and as models.dev does:
    /// `anthropic`.
    pub id: &'static str,
    /// As people know it: `Anthropic`.
    pub name: &'static str,
    /// What its plans are called, before the plan: `Claude`. `None` for a
    /// provider that sells none.
    pub subscription: Option<&'static str>,
    /// Whether an API key an agent holds reads any limit. Where it doesn't,
    /// an API-key account is listed with none, and no key is sent.
    pub key_limits: bool,
}

pub struct Account {
    /// `<provider>:<the provider's id for it>`.
    pub id: String,
    pub kind: Kind,
    /// What the person knows it by, such as an email address.
    pub label: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// A plan with limits that reset.
    Subscription,
    /// Paid as used: its limits, where it has any, are money.
    ApiKey,
}

#[derive(Default)]
pub struct Limits {
    /// The plan, as the provider reports it: `max`, `plus`.
    pub plan: Option<String>,
    pub readings: Vec<Reading>,
}

pub struct Reading {
    pub key: String,
    pub name: String,
    /// What it covers, when not everything: a model, such as `Opus`,
    /// several joined with "and", as `Claude and GPT`, or use no agent here
    /// makes, as `Other apps`, which names no model.
    pub scope: Option<String>,
    /// Percent used.
    pub used: f64,
    /// Dollars, for a limit of money. For one that never resets, all that
    /// was ever bought, so it grows when more is: an alert tells it used up
    /// once per size.
    pub size: Option<f64>,
    pub starts: Option<i64>,
    pub resets: Option<i64>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Problem {
    /// The provider refused the login: sign in again in the agent.
    SignIn,
    /// Out of reach, busy, or an answer not understood. `why` says which, in
    /// words that are the same each time it fails the same way, for the
    /// log; `retry_after` is how long the provider asked to be left, in
    /// milliseconds, if it did. `answered` unless no answer came, as when
    /// the Mac is offline: only a provider that answered is backed off
    /// (`limits::backoff`).
    Unavailable {
        why: String,
        retry_after: Option<i64>,
        answered: bool,
    },
    /// Every login to it has expired, so none was sent: its agent renews one
    /// when next used. Set by `limits`, never by a provider.
    Expired,
}

impl Problem {
    /// An answer that can't be used, or a read that failed some other way.
    pub fn unavailable(why: impl Into<String>) -> Problem {
        Problem::Unavailable {
            why: why.into(),
            retry_after: None,
            answered: true,
        }
    }

    /// No answer from the provider.
    pub fn unanswered(why: impl Into<String>) -> Problem {
        Problem::Unavailable {
            why: why.into(),
            retry_after: None,
            answered: false,
        }
    }

    /// An answer from `url` that isn't what it should be.
    fn not_understood(url: &str) -> Problem {
        Problem::unavailable(format!("{url}: an answer not understood"))
    }
}

/// The account every API key of `provider` draws on.
fn api_account(provider: &str) -> Account {
    Account {
        id: format!("{provider}:api"),
        kind: Kind::ApiKey,
        label: None,
    }
}

/// The account `credential` is to: for an API key, the one every key of
/// `provider` draws on; for a sign-in, the plan whose id and label
/// `identity` gives.
fn account(
    provider: &str,
    credential: &Credential,
    identity: impl FnOnce() -> Option<(String, Option<String>)>,
) -> Option<Account> {
    if credential.key {
        return Some(api_account(provider));
    }
    let (id, label) = identity()?;
    Some(Account {
        id: format!("{provider}:{id}"),
        kind: Kind::Subscription,
        label,
    })
}

/// Ask `url` with `credential`'s token and `headers`, for JSON, by `by`.
fn ask(
    url: &str,
    credential: &Credential,
    headers: &[&str],
    by: Instant,
) -> Result<Value, Problem> {
    asked(url, &credential.secret, headers, None, &[401, 403], by)
}

/// As [`ask`], with `token` and `refused` the statuses that are a refused
/// login. With a `body`, a POST of it as JSON. A body goes in curl's
/// arguments, so it must hold no secret. Past `by`, nothing is asked.
fn asked(
    url: &str,
    token: &Secret,
    headers: &[&str],
    body: Option<&Value>,
    refused: &[u16],
    by: Instant,
) -> Result<Value, Problem> {
    let left = time_left(url, by)?;
    let authorization = format!("Authorization: Bearer {}", token.expose());
    let mut lines = vec![authorization.as_str()];
    if body.is_some() {
        lines.push("Content-Type: application/json");
    }
    lines.extend(headers);
    let body = body.map(Value::to_string);
    let answered = requested(url, &lines, body.as_deref(), left)
        .map_err(|why| Problem::unanswered(format!("{url}: {why}")))?;
    answer(url, &answered, refused)
}

/// The time left until `by` to ask `url` in, or why it isn't asked: under a
/// millisecond, curl's `--max-time` would be 0.000, no limit.
fn time_left(url: &str, by: Instant) -> Result<Duration, Problem> {
    let left = by.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(1) {
        return Err(Problem::unanswered(format!(
            "{url}: not asked, as the read's time was spent"
        )));
    }
    Ok(left)
}

/// POST `fields` to `url` as a form, by `by`. The form goes to curl on
/// standard input, as headers do, so a secret among its fields is in no
/// process listing.
fn post_form(url: &str, fields: &[(&str, &str)], by: Instant) -> Result<Answered, Problem> {
    let left = time_left(url, by)?;
    let form = form_encode(fields);
    let args = [
        "--header",
        "Content-Type: application/x-www-form-urlencoded",
        "--data-binary",
        "@-",
    ];
    curl(url, &args, form.as_bytes(), left)
        .map_err(|why| Problem::unanswered(format!("{url}: {why}")))
}

/// `fields` as `application/x-www-form-urlencoded`.
fn form_encode(fields: &[(&str, &str)]) -> String {
    let encoded = |text: &str| -> String {
        text.bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    char::from(byte).to_string()
                }
                _ => format!("%{byte:02X}"),
            })
            .collect()
    };
    let fields: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{}={}", encoded(key), encoded(value)))
        .collect();
    fields.join("&")
}

/// The JSON `url` answered, or why there's none. A refusal is a refused
/// login unless its body is an HTML page, such as a bot check's 403, which
/// signing in again wouldn't get past. Chosen over counting only a JSON
/// body, as no provider's refusal body has been measured: one in plain
/// text or empty would never say "sign in again".
fn answer(url: &str, answered: &Answered, refused: &[u16]) -> Result<Value, Problem> {
    let body = answered.body.as_slice();
    match answered.status {
        200..=299 => serde_json::from_slice(body)
            .map_err(|_| Problem::unavailable(format!("{url}: an answer that isn't JSON"))),
        status if refused.contains(&status) && !body.trim_ascii_start().starts_with(b"<") => {
            Err(Problem::SignIn)
        }
        status => Err(Problem::Unavailable {
            why: format!("{url}: answered {status}"),
            retry_after: answered.retry_after,
            answered: true,
        }),
    }
}

/// What a provider answered.
struct Answered {
    status: u16,
    /// How long it asked to be left, in milliseconds: its `Retry-After`.
    retry_after: Option<i64>,
    body: Vec<u8>,
}

/// Ask `url` over HTTPS with `headers`, giving the status and body, or
/// giving up after `seconds`: a GET, or a POST of `body`.
///
/// Headers go to curl on standard input, so a credential among them is in
/// no process listing; `-q` keeps a `~/.curlrc` from adding a trace. A
/// header with a line break could add headers of its own, so none is sent.
/// A connection not made in 5 s won't be: on a network that drops what's
/// sent, curl otherwise waits out all of `seconds` (measured 2026-10-12 at
/// an address that drops everything: 5.0 s with `--connect-timeout 5`, all
/// of `--max-time` without).
///
/// A body is an argument, so it shows in a process listing.
pub fn request(
    url: &str,
    headers: &[&str],
    body: Option<&str>,
    seconds: u32,
) -> crate::Result<(u16, Vec<u8>)> {
    requested(url, headers, body, Duration::from_secs(seconds.into()))
        .map(|answered| (answered.status, answered.body))
        .map_err(|why| Error::Failed(format!("asking {url}: {why}")))
}

/// As [`request`], giving up after `time`, or why there's no answer.
fn requested(
    url: &str,
    headers: &[&str],
    body: Option<&str>,
    time: Duration,
) -> std::result::Result<Answered, String> {
    if headers.iter().any(|header| header.contains(['\r', '\n'])) {
        return Err("a header holds a line break, so nothing was sent".to_owned());
    }
    let mut args = vec!["--header", "@-"];
    if let Some(body) = body {
        args.extend(["--data-raw", body]);
    }
    let request: String = headers.iter().map(|header| format!("{header}\n")).collect();
    curl(url, &args, request.as_bytes(), time)
}

/// Run curl for `url` with `args`, writing `input` to its standard input,
/// giving up after `time`.
fn curl(
    url: &str,
    args: &[&str],
    input: &[u8],
    time: Duration,
) -> std::result::Result<Answered, String> {
    let mut curl = Command::new("/usr/bin/curl")
        .args(["-q", "--silent", "--show-error", "--compressed"])
        .args(["--proto", "=https"])
        .args(["--connect-timeout", "5"])
        .args(["--max-time", &format!("{:.3}", time.as_secs_f64())])
        .args(["--max-filesize", "67108864", "--user-agent", "Turnscope"])
        .args(["--write-out", "%{stderr}%{http_code} %header{retry-after}"])
        .args(args)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    // Headers and a form are far smaller than a pipe holds, so writing them
    // first can't wait on curl's output. Still reap curl and prefer its
    // error when it exited before reading them.
    let write = curl
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(input));
    let output = curl.wait_with_output().map_err(|error| error.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(unanswered(output.status.code(), &stderr));
    }
    write.map_err(|error| error.to_string())?;
    // What curl writes last on standard error is the status and the
    // `Retry-After`, if any.
    let last = stderr.trim().rsplit('\n').next().unwrap_or_default();
    let (status, after) = last.split_once(' ').unwrap_or((last, ""));
    Ok(Answered {
        status: status.parse().map_err(|_| format!("status {status:?}"))?,
        retry_after: retry_after(after, crate::now()),
        body: output.stdout,
    })
}

/// Why curl got no answer, from its exit code where that says, in words
/// that are the same each time: its own message names how many
/// milliseconds it waited.
fn unanswered(code: Option<i32>, said: &str) -> String {
    match code {
        Some(6) => "host not found",
        Some(7) => "couldn't connect",
        Some(28) => "no answer in time",
        Some(35 | 60) => "no secure connection",
        Some(52) => "an empty answer",
        Some(56) => "the connection broke",
        _ => said.lines().next().unwrap_or("curl failed").trim(),
    }
    .to_owned()
}

/// A `Retry-After` as a wait in milliseconds from `now`: seconds, or an
/// HTTP date.
fn retry_after(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    let wait = match value.parse::<i64>() {
        Ok(seconds) => seconds.saturating_mul(1000),
        Err(_) => {
            jiff::fmt::rfc2822::parse(value)
                .ok()?
                .timestamp()
                .as_millisecond()
                - now
        }
    };
    Some(wait.max(0))
}

/// A reset as providers write one, null or a time: `Some(None)` for null,
/// and `None` for one that doesn't read as a time.
fn reset(value: &Value) -> Option<Option<i64>> {
    match value {
        Value::Null => Some(None),
        at => crate::time::millis(at.as_str()?).map(Some),
    }
}

/// Whether `window` is no window: null, or every value null, as providers
/// write one a plan doesn't have.
fn absent(window: &Value) -> bool {
    match window {
        Value::Null => true,
        Value::Object(fields) => fields.values().all(Value::is_null),
        _ => false,
    }
}

/// A provider's key as words, each capitalized: `opus` is Opus,
/// `super_heavy` Super Heavy.
pub fn words(key: &str) -> String {
    key.split(['_', '-', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut letters = word.chars();
            letters
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .chain(letters)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A limit `used` percent of a window `length` ms long that resets at
/// `resets`, named for its length: keyed `key`, or `key:scope` for one on
/// some models. Of one answer or an agent's history (`agents::InBand`), the
/// key is the same.
pub fn window(
    key: &str,
    scope: Option<&str>,
    used: f64,
    length: i64,
    resets: Option<i64>,
) -> Reading {
    Reading {
        key: scope.map_or(key.to_owned(), |scope| format!("{key}:{scope}")),
        name: window_name(length),
        scope: scope.map(str::to_owned),
        used,
        size: None,
        starts: resets.map(|resets| resets - length),
        resets,
    }
}

/// What a window `length` ms long is called: `5 hours`, `Weekly`.
fn window_name(length: i64) -> String {
    let hours = (length + HOUR / 2) / HOUR;
    // Whole days, as a window across a clock change is an hour off.
    let days = (length + DAY / 2) / DAY;
    match (hours, days) {
        (0 | 1, _) => "Hourly".to_owned(),
        (hours, _) if hours < 23 => format!("{hours} hours"),
        (_, 1) => "Daily".to_owned(),
        (_, 7) => "Weekly".to_owned(),
        (_, 28..=31) => "Monthly".to_owned(),
        (_, days) => format!("{days} days"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::Problem;
    use crate::time::{DAY, HOUR, MINUTE};

    /// What `status` and `body` from a provider come to: the JSON, or
    /// `signIn`, or the reason it's unavailable.
    fn answered(status: u16, body: &[u8], refused: &[u16]) -> Result<Value, String> {
        let answered = super::Answered {
            status,
            retry_after: None,
            body: body.to_vec(),
        };
        super::answer("https://x", &answered, refused).map_err(|problem| match problem {
            Problem::Unavailable { why, .. } => why,
            other => format!("{other:?}"),
        })
    }

    #[test]
    fn a_refused_status_is_a_refused_login_unless_its_a_page() {
        let json: &[u8] = br#"{"error": "unauthorized"}"#;
        let page: &[u8] = b"<!DOCTYPE html><title>Just a moment...</title>";
        assert_eq!(
            answered(200, json, &[401, 403]),
            Ok(json!({"error": "unauthorized"}))
        );
        for (status, body, problem) in [
            (200, page, "https://x: an answer that isn't JSON"),
            (401, json, "SignIn"),
            (403, page, "https://x: answered 403"),
            (429, json, "https://x: answered 429"),
            // An empty or plain refusal is a refused login.
            (401, b"", "SignIn"),
            (401, b"Unauthorized", "SignIn"),
            (403, b"\n  <html></html>", "https://x: answered 403"),
        ] {
            assert_eq!(answered(status, body, &[401, 403]), Err(problem.to_owned()));
        }
        assert_eq!(
            answered(403, json, &[401]),
            Err("https://x: answered 403".to_owned())
        );
    }

    #[test]
    fn a_retry_after_is_seconds_or_a_date() {
        let now = crate::time::millis("2015-10-21T07:27:00Z").unwrap();
        assert_eq!(super::retry_after("120", now), Some(120_000));
        assert_eq!(
            super::retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(MINUTE)
        );
        assert_eq!(super::retry_after("", now), None);
        assert_eq!(super::retry_after("soon", now), None);
    }

    /// `guard` waits on an account's read in an agent's hook: once its time
    /// is spent, as on a provider's second request, no more is asked.
    #[test]
    fn nothing_is_asked_past_the_reads_time() {
        let credential = crate::agents::Credential {
            provider: "made-up",
            secret: crate::agents::Secret::new("made-up"),
            key: true,
            expires: None,
            identity: None,
            plan: None,
            refresh: None,
        };
        let spent = std::time::Instant::now();
        // Not HTTPS, so curl, were it run, would send nothing either.
        let asked = super::ask("http://example.invalid/", &credential, &[], spent);
        assert_eq!(
            asked,
            Err(Problem::unanswered(
                "http://example.invalid/: not asked, as the read's time was spent".to_owned()
            ))
        );
    }

    #[test]
    fn a_form_escapes_what_isnt_a_letter_digit_or_mark() {
        assert_eq!(
            super::form_encode(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", "1//a+b=c&d e~_.-"),
            ]),
            "grant_type=refresh_token&refresh_token=1%2F%2Fa%2Bb%3Dc%26d%20e~_.-"
        );
    }

    #[test]
    fn a_window_is_named_by_its_length_rounded() {
        assert_eq!(super::window_name(HOUR + 29 * MINUTE), "Hourly");
        assert_eq!(super::window_name(HOUR + 30 * MINUTE), "2 hours");
        assert_eq!(super::window_name(22 * HOUR + 29 * MINUTE), "22 hours");
        assert_eq!(super::window_name(22 * HOUR + 30 * MINUTE), "Daily");
        // A week across a clock change is an hour off.
        assert_eq!(super::window_name(7 * DAY - HOUR), "Weekly");
        assert_eq!(super::window_name(7 * DAY + HOUR), "Weekly");
        assert_eq!(super::window_name(14 * DAY), "14 days");
        assert_eq!(super::window_name(28 * DAY), "Monthly");
        assert_eq!(super::window_name(31 * DAY), "Monthly");
        assert_eq!(super::window_name(32 * DAY), "32 days");
    }

    #[test]
    fn a_curl_that_exits_before_reading_headers_reports_curls_error() {
        // Bigger than the pipe buffer, so `write_all` fails if curl exits
        // before reading `--header @-`.
        let big = format!("X-Test: {}", "a".repeat(256 * 1024));
        let error = super::requested(
            "https://example.invalid/",
            &[&big],
            None,
            std::time::Duration::MAX,
        )
        .err()
        .unwrap();
        assert!(error.contains("--max-time"), "{error}");
    }
}
