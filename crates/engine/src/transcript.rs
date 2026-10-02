//! Conversations: what was said in a session, read from the agent's own files
//! when someone opens it. Nothing of a conversation is kept.
//!
//! Every agent's conversation becomes one list of entries: what the person
//! said, what the model said and thought, each tool call with its result
//! folded in, and what the agent itself injected, such as instructions or the
//! summary a compacted conversation carries on from.
//!
//! A call that started a subagent is marked by the key its agent records for
//! the start on both sides ([`Builder::launched`]), and names the subagent
//! once the engine finds whose link keeps that key ([`Transcript::resolve`]).
//!
//! Beside the entries, a reader records the work its agent's records say was
//! done, the commands run, files changed and plans kept, as the agent's own
//! tools describe it ([`Builder::worked`]), for a handoff to add up
//! ([`crate::handoff`]).

use std::collections::HashMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::handoff::Work;
use crate::session::{SessionKey, Title, TitleSource};
use crate::time::Instant;

/// Who an entry is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// The person.
    User,
    /// The model's reply.
    Assistant,
    /// The model's thinking, as far as the agent recorded it readably.
    Reasoning,
    /// A tool call and its result.
    Tool,
    /// Something the agent injected: instructions, context, a compaction's
    /// summary, a command's output.
    System,
}

/// A tool call, with its result once the result is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCall {
    /// The tool's name, as the agent recorded it.
    pub name: String,
    /// Its arguments, as JSON text or as the agent gave them.
    pub input: String,
    /// What it returned, when recorded.
    pub output: Option<String>,
    /// Whether it reported failure.
    pub failed: bool,
    /// The subagent the call started, when it started one and its agent
    /// records which: exactly, never guessed from times or wording.
    pub subagent: Option<SessionKey>,
}

impl ToolCall {
    /// The start of its result: as much as a reader looks at to tell what
    /// the call did, enough for what a result opens with, such as `Exit code
    /// 2`, or `Task #3 created successfully: ` and the task's subject.
    pub(crate) fn head(&self) -> String {
        const HEAD: usize = 400;
        self.output
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(HEAD)
            .collect()
    }
}

/// One entry of a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its place in the conversation, from zero.
    pub index: u32,
    /// Who it is from.
    pub speaker: Speaker,
    /// When, where the agent recorded it.
    pub at: Option<Instant>,
    /// The model, for what a model said, thought or called.
    pub model: Option<String>,
    /// The text; empty for a tool call.
    pub text: String,
    /// The call, for a tool call.
    pub tool: Option<ToolCall>,
}

/// A conversation as a reader gives it: its entries, and the calls among
/// them that started a subagent, each by what the agent calls the start in
/// the subagent's own link ([`crate::session::SessionLink::launch`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct Transcript {
    pub(crate) entries: Vec<Entry>,
    /// Each call that started a subagent: its entry's index, and its key.
    pub(crate) launches: Vec<(u32, String)>,
    /// The work its agent recorded, in order, each when it was done where
    /// the agent recorded that.
    pub(crate) work: Vec<(Option<Instant>, Work)>,
}

impl Transcript {
    /// Make each call that started a subagent name it, as `children` says
    /// which subagent each key started.
    pub(crate) fn resolve(&mut self, children: &HashMap<String, SessionKey>) {
        for (index, key) in &self.launches {
            let call = usize::try_from(*index)
                .ok()
                .and_then(|index| self.entries.get_mut(index))
                .and_then(|entry| entry.tool.as_mut());
            if let (Some(call), Some(child)) = (call, children.get(key)) {
                call.subagent = Some(child.clone());
            }
        }
    }
}

/// A conversation being put together, in order.
#[derive(Debug, Default)]
pub(crate) struct Builder {
    entries: Vec<Entry>,
    /// Calls not yet answered, by id, at their place in `entries`, which is
    /// always a call's: only [`Builder::call`] adds one, and entries are never
    /// taken away.
    awaiting: HashMap<String, usize>,
    /// Calls that started a subagent, as [`Transcript::launches`].
    launches: Vec<(u32, String)>,
    /// The work recorded, as [`Transcript::work`].
    work: Vec<(Option<Instant>, Work)>,
}

impl Builder {
    /// Add something said, unless there is nothing in it to read.
    pub(crate) fn say(
        &mut self,
        speaker: Speaker,
        at: Option<Instant>,
        model: Option<&str>,
        text: impl Into<String>,
    ) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.push(speaker, at, model, text, None);
        }
    }

    /// Add a message sent as the person's, divided as [`prompt_parts`]
    /// divides it.
    pub(crate) fn prompt(&mut self, at: Option<Instant>, text: &str) {
        for (speaker, part) in prompt_parts(text) {
            self.say(speaker, at, None, part);
        }
    }

    /// Add a call of the tool `name`, or of a tool the agent did not name,
    /// with its arguments, `input`. A later result answers it when it has an
    /// id.
    pub(crate) fn call(
        &mut self,
        id: Option<&str>,
        at: Option<Instant>,
        model: Option<&str>,
        name: Option<&str>,
        input: String,
    ) {
        if let Some(id) = id {
            self.awaiting.insert(id.to_owned(), self.entries.len());
        }
        let tool = ToolCall {
            name: name.unwrap_or("tool").to_owned(),
            input,
            ..ToolCall::default()
        };
        self.push(Speaker::Tool, at, model, String::new(), Some(tool));
    }

    /// Mark the call just added as the start of the subagent whose link
    /// records `key`. An empty key names nothing and is passed over.
    pub(crate) fn launched(&mut self, key: &str) {
        match self.entries.last() {
            Some(entry) if entry.tool.is_some() && !key.is_empty() => {
                self.launches.push((entry.index, key.to_owned()));
            }
            _ => {}
        }
    }

    /// Fold a result into the call it answers, and give back that call. A
    /// result answering no call is kept as what the agent injected, so
    /// nothing said is lost.
    pub(crate) fn answer(&mut self, id: &str, output: String, failed: bool) -> Option<&ToolCall> {
        let Some(position) = self.awaiting.remove(id) else {
            self.say(Speaker::System, None, None, output);
            return None;
        };
        let tool = self.entries.get_mut(position)?.tool.as_mut()?;
        tool.output = Some(output);
        tool.failed = failed;
        Some(tool)
    }

    /// Record work the agent did, at `at` where it recorded when.
    pub(crate) fn worked(&mut self, at: Option<Instant>, work: Work) {
        self.work.push((at, work));
    }

    /// How many entries there are so far.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The conversation.
    pub(crate) fn finish(self) -> Transcript {
        Transcript {
            entries: self.entries,
            launches: self.launches,
            work: self.work,
        }
    }

    /// The conversation so far, as it would finish.
    pub(crate) fn transcript(&self) -> Transcript {
        Transcript {
            entries: self.entries.clone(),
            launches: self.launches.clone(),
            work: self.work.clone(),
        }
    }

    /// The calls so far that started a subagent.
    pub(crate) fn launches(&self) -> &[(u32, String)] {
        &self.launches
    }

    /// The conversation so far.
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn push(
        &mut self,
        speaker: Speaker,
        at: Option<Instant>,
        model: Option<&str>,
        text: String,
        tool: Option<ToolCall>,
    ) {
        self.entries.push(Entry {
            // A conversation of four billion entries does not fit in memory.
            index: u32::try_from(self.entries.len()).unwrap_or(u32::MAX),
            speaker,
            at,
            model: model.map(str::to_owned),
            text,
            tool,
        });
    }
}

/// The pieces of text in a value that is a string or a list of blocks carrying
/// text.
pub(crate) fn pieces(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => vec![text],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str().or_else(|| block.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The text of a value that is a string or a list of blocks carrying text, a
/// piece to a line; anything else as compact JSON.
pub(crate) fn text_of(value: &Value) -> String {
    match value {
        Value::Array(_) => pieces(value).join("\n"),
        other => compact(other),
    }
}

/// The text field `name` of a call's arguments, `input`, when they are a JSON
/// object that has one.
pub(crate) fn argument(input: &str, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(input)
        .ok()?
        .get(name)?
        .as_str()
        .map(str::to_owned)
}

/// A value as text: a string as it is, null as nothing, and anything else, such
/// as a tool's arguments, as compact JSON.
pub(crate) fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The tags agents wrap what they inject in, which a message sent as the
/// person's opens with when it is the agent's (besides those
/// [`prompt_parts`] takes apart): Claude Code's reminders, notices of work
/// done in the background, a fork's instructions, and a shell command run
/// with `!` and its output; Codex's context, instructions and plugins,
/// requests of its own, markers around an image, a skill's instructions,
/// the person's answer to a question it asked, as JSON, word of a turn
/// stopped, and of its apps page opened; and Grok Build's context and the
/// images it saved.
///
/// Every tag that opened such a message in this Mac's history of Claude
/// Code, Codex, OpenCode, Pi and Grok Build but one, `<pasted_content>`,
/// which Claude Code puts around what the person pasted, before what they
/// typed (measured 2026-09-27; Codex's apps page, 7 times in 936 rollouts,
/// 2026-09-30). A message opening with any other tag, or with `<` alone,
/// is the person's.
const INJECTED: &[&str] = &[
    "bash-input",
    "bash-stderr",
    "bash-stdout",
    "codex_internal_context",
    "environment_context",
    "external_codex_apps_open_page",
    "fork-boilerplate",
    "image",
    "image_files",
    "in-app-browser-context",
    "recommended_plugins",
    "send_user_message_question_reply",
    "skill",
    "system-reminder",
    "task-notification",
    "turn_aborted",
    "user_info",
];

/// A message sent as the person's, divided into what they said and what their
/// agent put in beside it.
///
/// Agents wrap what they inject in tags: context such as `<system-reminder>`
/// ([`INJECTED`]), a slash command as `<command-name>`, its output as
/// `<local-command-stdout>`, and, for some, the person's own words as
/// `<user_query>` inside the context, which may open with a line of the
/// agent's own, such as Grok Build's note that the person wrote while it
/// worked. Two openings are the agent's though they carry no tag: Claude
/// Code's `[Request interrupted`, and Codex's `# AGENTS.md instructions`. A
/// slash command reads as the person's command; its output and all other
/// injected text as the agent's; a caveat Claude Code adds before commands is
/// left out. A message opening with a tag no agent is known to inject is the
/// person's, as one pasted from a web page may be.
pub(crate) fn prompt_parts(text: &str) -> Vec<(Speaker, String)> {
    let text = text.trim();
    if text.starts_with("<local-command-caveat>") {
        return Vec::new();
    }
    if text.starts_with("<command-name>") {
        let name = element(text, "command-name").map_or("", |(_, name)| name);
        let args = element(text, "command-args").map_or("", |(_, args)| args);
        return vec![(
            Speaker::User,
            format!("{name} {args}").trim_end().to_owned(),
        )];
    }
    if text.starts_with("<local-command-stdout>") {
        let output = element(text, "local-command-stdout").map_or(text, |(_, output)| output);
        return vec![(Speaker::System, output.to_owned())];
    }
    if let Some((span, query)) = element(text, "user_query") {
        let context = format!("{}{}", &text[..span.start], &text[span.end..]);
        return vec![
            (Speaker::System, context.trim().to_owned()),
            (Speaker::User, query.to_owned()),
        ];
    }
    let injected = opening_tag(text).is_some_and(|tag| INJECTED.contains(&tag))
        || text.starts_with("[Request interrupted")
        || text.starts_with("# AGENTS.md instructions");
    vec![(
        if injected {
            Speaker::System
        } else {
            Speaker::User
        },
        text.to_owned(),
    )]
}

/// How far what the person typed has titled their session, by one rule for
/// every agent: the first thing they said, as [`prompt_parts`] divides a
/// message sent as theirs. A slash command, such as `/goal Ship the scanner`,
/// says more of what they did than of what the session is about, so it titles
/// the session only until something that is not one does, and a command with
/// nothing after it titles nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Titled {
    /// Not yet.
    #[default]
    No,
    /// By a slash command.
    ByCommand,
    /// By what the person said.
    Yes,
}

impl Titled {
    /// The title a message sent as the person's, `text`, gives their
    /// session, when it replaces the one it has.
    pub(crate) fn offer(&mut self, text: &str) -> Option<Title> {
        if *self == Titled::Yes {
            return None;
        }
        let (_, said) = prompt_parts(text)
            .into_iter()
            .find(|(speaker, _)| *speaker == Speaker::User)?;
        let mut words = said.split_whitespace();
        let command = words
            .next()
            .and_then(|word| word.strip_prefix('/'))
            .is_some_and(|name| !name.contains('/'));
        // A command alone, such as `/clear`, says nothing of the session.
        if command && (*self == Titled::ByCommand || words.next().is_none()) {
            return None;
        }
        let title = Title::new(TitleSource::Prompt, &said)?;
        *self = if command {
            Titled::ByCommand
        } else {
            Titled::Yes
        };
        Some(title)
    }
}

/// The name of the tag `text` opens with, opening or closing, as `image` of
/// `<image name=[Image #1]>` or of `</image>`: `None` when it opens with
/// none.
fn opening_tag(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('<')?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name = rest
        .split(|character: char| character.is_whitespace() || matches!(character, '>' | '/'))
        .next()?;
    (!name.is_empty()).then_some(name)
}

/// Where the first element named `tag` sits in `text`, and what it holds.
fn element<'a>(text: &'a str, tag: &str) -> Option<(Range<usize>, &'a str)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let inner = start + open.len();
    let end = inner + text[inner..].find(&close)?;
    Some((start..end + close.len(), text[inner..end].trim()))
}

#[cfg(test)]
mod tests {
    use super::{Builder, Speaker, Titled, prompt_parts};

    #[test]
    fn a_result_folds_into_the_call_it_answers() {
        let mut builder = Builder::default();
        builder.call(
            Some("toolu_1"),
            None,
            Some("claude-opus-5"),
            Some("Read"),
            "{}".into(),
        );
        builder.say(
            Speaker::Assistant,
            None,
            Some("claude-opus-5"),
            "Reading it.",
        );
        builder.answer("toolu_1", "fn main() {}".into(), false);
        builder.answer("toolu_unknown", "stray output".into(), false);
        let entries = builder.finish().entries;
        assert_eq!(
            entries[0].tool.as_ref().unwrap().output.as_deref(),
            Some("fn main() {}")
        );
        assert_eq!(
            (entries[1].speaker, entries[1].index),
            (Speaker::Assistant, 1)
        );
        assert_eq!(
            (entries[2].speaker, entries[2].text.as_str()),
            (Speaker::System, "stray output")
        );
    }

    #[test]
    fn injected_context_is_told_apart_from_what_the_person_typed() {
        assert_eq!(
            prompt_parts("Fix the build"),
            [(Speaker::User, "Fix the build".to_owned())]
        );
        assert_eq!(
            prompt_parts("<command-name>/model</command-name>\n<command-args>opus</command-args>"),
            [(Speaker::User, "/model opus".to_owned())]
        );
        assert_eq!(
            prompt_parts("<environment>cwd: /work</environment>\n<user_query>Why?</user_query>"),
            [
                (
                    Speaker::System,
                    "<environment>cwd: /work</environment>".to_owned()
                ),
                (Speaker::User, "Why?".to_owned())
            ]
        );
        // Grok Build's note of a message sent while it worked, with the files
        // attached after the person's words.
        assert_eq!(
            prompt_parts(
                "The user sent a message while you were working:\n<user_query>Stop there</user_query>\n<attached_files>src/lib.rs</attached_files>"
            ),
            [
                (
                    Speaker::System,
                    "The user sent a message while you were working:\n\n<attached_files>src/lib.rs</attached_files>"
                        .to_owned()
                ),
                (Speaker::User, "Stop there".to_owned())
            ]
        );
        assert!(prompt_parts("<local-command-caveat>ignore</local-command-caveat>").is_empty());
        // What agents inject, opening with a tag of their own: Claude Code's,
        // Codex's around an attached image, before and after it, and Grok
        // Build's.
        for injected in [
            "<system-reminder>x</system-reminder>",
            "<task-notification>\n<summary>Goal check-in</summary>\n</task-notification>",
            "<image name=[Image #1] path=\"/Users/joey/Desktop/shot.png\">",
            "</image>",
            "<environment_context>\n  <cwd>/work</cwd>\n</environment_context>",
            "<user_info>\nOS Version: macos\n</user_info>",
        ] {
            assert_eq!(prompt_parts(injected)[0].0, Speaker::System, "{injected}");
        }
        // What the person sent that opens with `<` but no tag an agent
        // injects: a paste Claude Code marks, markup, and a heart.
        for typed in [
            "<pasted_content id=\"fe94\">\nhttps://example.com\n</pasted_content id=\"fe94\">\n\nLike this",
            "<div class=\"row\"> won't center",
            "<3 thanks",
        ] {
            assert_eq!(
                prompt_parts(typed),
                [(Speaker::User, typed.to_owned())],
                "{typed}"
            );
        }
    }

    #[test]
    fn a_title_is_the_first_thing_the_person_said_and_a_command_only_until_then() {
        // The titles a session's messages give, in turn.
        let titles = |messages: &[&str]| -> Vec<Option<String>> {
            let mut titled = Titled::default();
            messages
                .iter()
                .map(|text| titled.offer(text).map(|title| title.text))
                .collect()
        };
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(
            titles(&["Fix the build\nIt fails on main", "And the tests"]),
            [some("Fix the build"), None]
        );
        assert_eq!(
            titles(&[
                "<system-reminder>Be brief</system-reminder>",
                "# AGENTS.md instructions for /work",
                "<environment>cwd: /work</environment>\n<user_query>Why?</user_query>"
            ]),
            [None, None, some("Why?")]
        );
        // A command titles a session until the first thing said that is not
        // one, a command alone titles nothing, and a path is not one.
        assert_eq!(
            titles(&[
                "<command-name>/clear</command-name>\n<command-args></command-args>",
                "<command-name>/model</command-name>\n<command-args>opus</command-args>",
                "/review the parser",
                "/Users/joey/notes.md is out of date",
                "Thanks"
            ]),
            [
                None,
                some("/model opus"),
                None,
                some("/Users/joey/notes.md is out of date"),
                None
            ]
        );
    }
}
