//! The providers whose accounts' limits Turnscope reads. Each is one module
//! implementing [`Provider`], listed in [`ALL`]. This is Turnscope's only way
//! to the network: `/usr/bin/curl`.

mod anthropic;
mod openai;
mod opencode_go;
mod openrouter;
mod xai;

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::agents::Credential;
use crate::{Error, Result};

/// Every provider, in the order they're listed.
pub static ALL: &[&dyn Provider] = &[
    &anthropic::Anthropic,
    &openai::OpenAi,
    &xai::Xai,
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
    /// The account's limits now.
    fn limits(&self, credential: &Credential, now: i64) -> std::result::Result<Limits, Problem>;
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

pub struct Limits {
    /// The plan, as the provider reports it: `max`, `plus`.
    pub plan: Option<String>,
    pub readings: Vec<Reading>,
}

pub struct Reading {
    pub key: String,
    pub name: String,
    /// What it covers, when not everything: a model, such as `Opus`.
    pub scope: Option<String>,
    /// Percent used.
    pub used: f64,
    /// Dollars, for a limit of money.
    pub size: Option<f64>,
    pub starts: Option<i64>,
    pub resets: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Problem {
    /// The provider refused the login: sign in again in the agent.
    SignIn,
    /// Out of reach, busy, or an answer not understood.
    Unavailable,
    /// Every login to it has expired, so none was sent: its agent renews one
    /// when next used. Set by `limits`, never by a provider.
    Expired,
}

/// The account every API key of `provider` draws on.
fn api_account(provider: &str) -> Account {
    Account {
        id: format!("{provider}:api"),
        kind: Kind::ApiKey,
        label: None,
    }
}

/// Ask `url` with `credential`'s token and `headers`, for JSON. A 401 or 403
/// is a refused login.
fn ask(
    url: &str,
    credential: &Credential,
    headers: &[&str],
) -> std::result::Result<Value, Problem> {
    ask_refused_by(url, credential, headers, &[401, 403])
}

/// As [`ask`], with `refused` the statuses that are a refused login.
fn ask_refused_by(
    url: &str,
    credential: &Credential,
    headers: &[&str],
    refused: &[u16],
) -> std::result::Result<Value, Problem> {
    let mut lines = vec![format!(
        "Authorization: Bearer {}",
        credential.secret.expose()
    )];
    lines.extend(headers.iter().map(|header| (*header).to_owned()));
    match get(url, &lines) {
        Ok((200..=299, body)) => serde_json::from_slice(&body).map_err(|_| Problem::Unavailable),
        Ok((status, _)) if refused.contains(&status) => Err(Problem::SignIn),
        _ => Err(Problem::Unavailable),
    }
}

/// GET `url` over HTTPS with `headers`, giving the status and body.
///
/// Headers go to curl on standard input, so a credential among them is in
/// no process listing; `-q` keeps a `~/.curlrc` from adding a trace. A
/// header with a line break could add headers of its own, so none is sent.
pub fn get(url: &str, headers: &[String]) -> Result<(u16, Vec<u8>)> {
    if headers.iter().any(|header| header.contains(['\r', '\n'])) {
        return Err(Error::Failed(
            "a header holds a line break, so nothing was sent".to_owned(),
        ));
    }
    let failed = |why: String| Error::Failed(format!("asking {url}: {why}"));
    let mut curl = Command::new("/usr/bin/curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--compressed",
            "--proto",
            "=https",
        ])
        .args([
            "--max-time",
            "60",
            "--max-filesize",
            "67108864",
            "--user-agent",
            "Turnscope",
        ])
        .args(["--header", "@-", "--write-out", "%{stderr}%{http_code}"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failed(error.to_string()))?;
    let request: String = headers.iter().map(|header| format!("{header}\n")).collect();
    // The headers are far smaller than a pipe holds, so writing them first
    // can't wait on curl's output.
    if let Some(mut stdin) = curl.stdin.take() {
        stdin
            .write_all(request.as_bytes())
            .map_err(|error| failed(error.to_string()))?;
    }
    let output = curl
        .wait_with_output()
        .map_err(|error| failed(error.to_string()))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(failed(stderr.trim().to_owned()));
    }
    // What curl writes last on standard error is the status.
    let status = stderr.trim().rsplit(['\n', ' ']).next().unwrap_or_default();
    let status = status
        .parse()
        .map_err(|_| failed(format!("status {status:?}")))?;
    Ok((status, output.stdout))
}

/// A time an answer gives as RFC 3339 text, or Unix seconds or milliseconds.
fn time(value: &Value) -> Option<i64> {
    match value {
        Value::String(text) => crate::time::millis(text),
        Value::Number(number) => {
            let number = number.as_i64()?;
            // Seconds until the year 5138, milliseconds after.
            Some(if number < 100_000_000_000 {
                number * 1000
            } else {
                number
            })
        }
        _ => None,
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

/// What a window `seconds` long is called: `5 hours`, `Weekly`.
fn window_name(seconds: i64) -> String {
    let hours = (seconds + 1800) / 3600;
    // Whole days, as a window across a clock change is an hour off.
    let days = (seconds + 43_200) / 86_400;
    match (hours, days) {
        (0 | 1, _) => "Hourly".to_owned(),
        (hours, _) if hours < 23 => format!("{hours} hours"),
        (_, 1) => "Daily".to_owned(),
        (_, 7) => "Weekly".to_owned(),
        (_, 28..=31) => "Monthly".to_owned(),
        (_, days) => format!("{days} days"),
    }
}
