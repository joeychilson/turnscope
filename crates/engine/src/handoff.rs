//! Handoffs: where a session stopped, for whoever picks it up.
//!
//! **What.** A handoff is what another agent, or the person, needs to carry
//! a session on: the first and the last thing the person asked, the model's
//! last reply, the plan the agent kept and how far it got, the files it
//! changed with the lines each gained and lost, and the commands it ran with
//! whether they failed.
//!
//! **How.** The requests and the reply are read from the conversation, as
//! [`crate::Engine::conversation`] reads it. The rest is work each agent
//! records its own way, with tools of its own names, so each reader turns
//! what its agent records into [`Work`] as it reads the conversation
//! ([`crate::transcript::Builder::worked`]), and this module adds the work
//! up without knowing any agent's tools. Each reader's module documentation
//! records which tools it reads, measured on this Mac's history.
//!
//! **Counting lines.** A change is counted from the most exact record its
//! agent keeps: a unified diff's hunks, as its headers bound them
//! ([`diff_lines`]); an `*** Begin Patch` envelope ([`envelope`]); a new
//! file's content; or, where only an edit's old and new text is kept, a
//! line diff of the two ([`replaced_lines`]). A file's lines are unknown,
//! never zero, once any change to it could not be counted, as a deleted
//! file's are when its agent records only that it was deleted.
//!
//! **Untrusted.** What a tool was given and what it returned is whatever a
//! model wrote. Nothing here panics on it: counts add without overflow, a
//! diff that doesn't read is unknown rather than guessed, a line diff is
//! only worked out below [`MOST_COMPARED`] pairs of lines, and a session
//! with more files or commands than a handoff keeps says how many it left
//! out. A known tool's call whose input or result isn't what its reader
//! expects is not skipped silently: it is counted in [`Handoff::unclear`]
//! by the tool's name.

use std::collections::HashMap;

use crate::time::Instant;
use crate::transcript::{Entry, Speaker};

/// The most pairs of lines [`replaced_lines`] compares: an edit of two
/// thousand lines for two thousand, which takes a few milliseconds. Beyond
/// it, what an edit changed is unknown.
pub(crate) const MOST_COMPARED: usize = 4_000_000;

/// The most files a handoff lists; changes to others are counted in
/// [`Handoff::files_left_out`].
const MOST_FILES: usize = 2_000;

/// The most commands a handoff keeps, the latest.
const LAST_COMMANDS: usize = 20;

/// Where a session stopped: what someone picking it up needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handoff {
    /// The first thing the person asked. A slash command with nothing after
    /// it, such as `/clear`, asks nothing, and is passed over.
    pub first_request: Option<Quote>,
    /// The last thing the person asked.
    pub last_request: Option<Quote>,
    /// The model's last reply.
    pub last_reply: Option<Quote>,
    /// The agent's plan as it last stood, in its order: empty when it kept
    /// none, or its agent records none Turnscope reads.
    pub plan: Vec<Step>,
    /// The goal the agent was set, where its agent keeps one, as Codex does.
    pub goal: Option<Goal>,
    /// Every file the session changed, in the order it first changed each,
    /// at most two thousand.
    pub files: Vec<FileChange>,
    /// How many more files it changed than `files` lists.
    pub files_left_out: u64,
    /// The last commands it ran, oldest first: at most twenty.
    pub commands: Vec<Command>,
    /// How many commands it ran in all.
    pub commands_run: u64,
    /// How many of them failed.
    pub commands_failed: u64,
    /// Calls of tools its reader knows, whose input or result wasn't what
    /// the reader expects, by the tool's name, with how many: what they did
    /// is missing from the rest.
    pub unclear: Vec<(String, u64)>,
}

/// Something said, as a handoff quotes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    /// Its entry in the conversation, as [`Entry::index`] numbers it.
    pub entry: u32,
    /// When, where its agent recorded it.
    pub at: Option<Instant>,
    /// What was said, whole.
    pub text: String,
}

/// One step of an agent's plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// What the step is.
    pub text: String,
    /// How far it got; `None` where its agent's word for it is one
    /// Turnscope doesn't know, which [`Handoff::unclear`] counts.
    pub status: Option<StepStatus>,
}

/// How far a step of a plan got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StepStatus {
    /// Not started.
    Pending,
    /// Under way.
    InProgress,
    /// Done.
    Completed,
    /// Given up.
    Cancelled,
}

impl StepStatus {
    /// Its stable key, as agents write it.
    pub fn key(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::InProgress => "in_progress",
            StepStatus::Completed => "completed",
            StepStatus::Cancelled => "cancelled",
        }
    }

    /// The status agents write as `word`: the four words every agent read
    /// here uses, exactly. `None` for any other.
    pub(crate) fn from_word(word: &str) -> Option<StepStatus> {
        [
            StepStatus::Pending,
            StepStatus::InProgress,
            StepStatus::Completed,
            StepStatus::Cancelled,
        ]
        .into_iter()
        .find(|status| status.key() == word)
    }
}

/// A goal an agent was set, as Codex keeps one for a thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goal {
    /// What it is to achieve.
    pub objective: String,
    /// Where it stands, in the agent's own word, such as `active` or
    /// `paused`.
    pub status: Option<String>,
}

/// A file a session changed, and by how much.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// The file, as its agent named it: absolute, or relative to where the
    /// session ran.
    pub path: String,
    /// Lines added over all its changes; `None` once one couldn't be
    /// counted.
    pub added: Option<u64>,
    /// Lines removed over all its changes; `None` once one couldn't be
    /// counted.
    pub removed: Option<u64>,
    /// Whether its first change created it.
    pub created: bool,
    /// Whether its last change deleted it.
    pub deleted: bool,
    /// Where its last change moved it, if one did.
    pub moved_to: Option<String>,
    /// How many changes were made to it.
    pub changes: u64,
}

/// A command a session ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// When, where its agent recorded it.
    pub at: Option<Instant>,
    /// The command, as it was run.
    pub command: String,
    /// Its exit code, where its agent records one.
    pub exit: Option<i64>,
    /// Whether it failed: `None` where its agent records no outcome, as for
    /// one left running in the background.
    pub failed: Option<bool>,
}

/// What an agent did, as its reader records it from the agent's own
/// records while reading a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Work {
    /// A command ran.
    Ran {
        command: String,
        exit: Option<i64>,
        failed: Option<bool>,
    },
    /// A file changed.
    Changed(Change),
    /// The whole plan, as it now stands.
    Planned(Vec<PlanItem>),
    /// Some steps of the plan, told apart by id: each changes the step with
    /// its id, or is added after the rest.
    Revised(Vec<PlanItem>),
    /// A goal set, or where it stands now.
    Goal(Goal),
    /// A call of the tool named, whose input or result wasn't understood.
    Unclear(String),
}

/// One change to one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) path: String,
    pub(crate) kind: ChangeKind,
    pub(crate) added: Option<u64>,
    pub(crate) removed: Option<u64>,
    /// Where the change moved the file to.
    pub(crate) moved_to: Option<String>,
}

impl Change {
    /// A change to `path` of `kind`, adding and removing as counted.
    pub(crate) fn new(path: &str, kind: ChangeKind, counted: Option<(u64, u64)>) -> Change {
        Change {
            path: path.to_owned(),
            kind,
            added: counted.map(|(added, _)| added),
            removed: counted.map(|(_, removed)| removed),
            moved_to: None,
        }
    }
}

/// What a change did to its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Created,
    Updated,
    Deleted,
}

/// A step of a plan as an agent wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanItem {
    /// The agent's id for it, where it gives one.
    pub(crate) id: Option<String>,
    /// What it is; `None` where a revision leaves it as it was.
    pub(crate) text: Option<String>,
    /// How far it got, where given.
    pub(crate) status: Option<StepStatus>,
    /// Whether a revision takes it out of the plan.
    pub(crate) removed: bool,
}

/// The handoff of a conversation of `entries` in which its agent did
/// `work`, in order.
pub(crate) fn handoff(entries: &[Entry], work: &[(Option<Instant>, Work)]) -> Handoff {
    let quote = |entry: &Entry| Quote {
        entry: entry.index,
        at: entry.at,
        text: entry.text.clone(),
    };
    let asked = |entry: &&Entry| {
        entry.speaker == Speaker::User && entry.tool.is_none() && !bare_command(&entry.text)
    };
    let replied = |entry: &&Entry| entry.speaker == Speaker::Assistant;
    let mut handoff = Handoff {
        first_request: entries.iter().find(asked).map(quote),
        last_request: entries.iter().rev().find(asked).map(quote),
        last_reply: entries.iter().rev().find(replied).map(quote),
        ..Handoff::default()
    };
    let mut plan: Vec<(Option<String>, Step)> = Vec::new();
    let mut files: HashMap<String, usize> = HashMap::new();
    let mut unclear: Vec<(String, u64)> = Vec::new();
    let mut commands: Vec<Command> = Vec::new();
    for (at, work) in work {
        match work {
            Work::Ran {
                command,
                exit,
                failed,
            } => {
                handoff.commands_run = handoff.commands_run.saturating_add(1);
                if *failed == Some(true) {
                    handoff.commands_failed = handoff.commands_failed.saturating_add(1);
                }
                if commands.len() == LAST_COMMANDS {
                    commands.remove(0);
                }
                commands.push(Command {
                    at: *at,
                    command: command.clone(),
                    exit: *exit,
                    failed: *failed,
                });
            }
            Work::Changed(change) => changed(&mut handoff, &mut files, change),
            Work::Planned(items) => {
                plan.clear();
                revise(&mut plan, items);
            }
            Work::Revised(items) => revise(&mut plan, items),
            Work::Goal(goal) => handoff.goal = Some(goal.clone()),
            Work::Unclear(tool) => match unclear.iter_mut().find(|(name, _)| name == tool) {
                Some((_, count)) => *count = count.saturating_add(1),
                None => unclear.push((tool.clone(), 1)),
            },
        }
    }
    handoff.plan = plan.into_iter().map(|(_, step)| step).collect();
    handoff.commands = commands;
    handoff.unclear = unclear;
    handoff
}

/// Whether `text` is a slash command with nothing after it, as `/clear`: one
/// word opening with `/` and holding no other, which a path would.
fn bare_command(text: &str) -> bool {
    let text = text.trim();
    text.strip_prefix('/').is_some_and(|name| {
        !name.is_empty() && !name.contains('/') && !name.contains(char::is_whitespace)
    })
}

/// Count `change` into the files `handoff` lists, placed by path in
/// `places`.
fn changed(handoff: &mut Handoff, places: &mut HashMap<String, usize>, change: &Change) {
    let place = match places.get(&change.path) {
        Some(place) => *place,
        None if handoff.files.len() >= MOST_FILES => {
            handoff.files_left_out = handoff.files_left_out.saturating_add(1);
            return;
        }
        None => {
            places.insert(change.path.clone(), handoff.files.len());
            handoff.files.push(FileChange {
                path: change.path.clone(),
                added: Some(0),
                removed: Some(0),
                created: change.kind == ChangeKind::Created,
                deleted: false,
                moved_to: None,
                changes: 0,
            });
            handoff.files.len() - 1
        }
    };
    let file = &mut handoff.files[place];
    file.added = file
        .added
        .zip(change.added)
        .map(|(a, b)| a.saturating_add(b));
    file.removed = file
        .removed
        .zip(change.removed)
        .map(|(a, b)| a.saturating_add(b));
    file.deleted = change.kind == ChangeKind::Deleted;
    if change.moved_to.is_some() {
        file.moved_to.clone_from(&change.moved_to);
    }
    file.changes = file.changes.saturating_add(1);
}

/// Apply `items` to `plan`: each changes the step with its id, or, without
/// one or with one no step has, is added after the rest.
fn revise(plan: &mut Vec<(Option<String>, Step)>, items: &[PlanItem]) {
    for item in items {
        let place = item.id.as_ref().and_then(|id| {
            plan.iter()
                .position(|(known, _)| known.as_ref() == Some(id))
        });
        match place {
            Some(place) if item.removed => {
                plan.remove(place);
            }
            Some(place) => {
                let (_, step) = &mut plan[place];
                if let Some(text) = &item.text {
                    step.text.clone_from(text);
                }
                if item.status.is_some() {
                    step.status = item.status;
                }
            }
            None if item.removed => {}
            None => plan.push((
                item.id.clone(),
                Step {
                    text: item.text.clone().unwrap_or_default(),
                    status: item.status,
                },
            )),
        }
    }
}

/// How many lines `text` holds, a last line without a newline counted.
pub(crate) fn lines(text: &str) -> u64 {
    u64::try_from(text.lines().count()).unwrap_or(u64::MAX)
}

/// The lines added and removed by a unified diff's hunks, each hunk as long
/// as its `@@ -a,b +c,d @@` header says, so what lies between hunks, such as
/// a file's `---` and `+++` header, is never counted. `None` when a hunk's
/// header doesn't read or a hunk runs short of what its header says.
pub(crate) fn diff_lines(diff: &str) -> Option<(u64, u64)> {
    let (mut added, mut removed) = (0u64, 0u64);
    // What of the hunk being read is still to come, old and new.
    let (mut old, mut new) = (0u64, 0u64);
    for line in diff.lines() {
        if old == 0 && new == 0 {
            if line.starts_with("@@") {
                (old, new) = hunk(line)?;
            }
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                new = new.checked_sub(1)?;
                added = added.saturating_add(1);
            }
            Some(b'-') => {
                old = old.checked_sub(1)?;
                removed = removed.saturating_add(1);
            }
            // A note that the file ends without a newline.
            Some(b'\\') => {}
            // Context, which some tools write without its leading space when
            // the line is empty.
            Some(b' ') | None => {
                old = old.checked_sub(1)?;
                new = new.checked_sub(1)?;
            }
            Some(_) => return None,
        }
    }
    (old == 0 && new == 0).then_some((added, removed))
}

/// How many old and new lines the hunk whose header is `line` spans, from
/// `@@ -a,b +c,d @@`, a count left out being one.
fn hunk(line: &str) -> Option<(u64, u64)> {
    let inner = line.strip_prefix("@@ ")?;
    let (ranges, _) = inner.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let count = |range: &str, sign: char| -> Option<u64> {
        let range = range.strip_prefix(sign)?;
        match range.split_once(',') {
            Some((start, count)) => {
                start.parse::<u64>().ok()?;
                count.parse().ok()
            }
            None => range.parse::<u64>().ok().map(|_| 1),
        }
    };
    Some((count(old, '-')?, count(new, '+')?))
}

/// The changes an `*** Begin Patch` envelope makes, as Codex's `apply_patch`
/// and OpenCode's `patch` write them: files added, with their lines; files
/// updated, with the lines their hunks add and remove, and where they move
/// to; and files deleted, whose lines the envelope doesn't say. `None` when
/// it isn't one, or a line of it doesn't read.
pub(crate) fn envelope(patch: &str) -> Option<Vec<Change>> {
    let mut lines = patch.trim().lines();
    if lines.next()?.trim_end() != "*** Begin Patch" {
        return None;
    }
    let mut changes: Vec<Change> = Vec::new();
    let mut ended = false;
    for line in lines {
        if ended {
            return None;
        }
        if let Some(header) = line.strip_prefix("*** ") {
            let header = header.trim_end();
            if header == "End Patch" {
                ended = true;
            } else if let Some(path) = header.strip_prefix("Add File: ") {
                changes.push(Change::new(path, ChangeKind::Created, Some((0, 0))));
            } else if let Some(path) = header.strip_prefix("Update File: ") {
                changes.push(Change::new(path, ChangeKind::Updated, Some((0, 0))));
            } else if let Some(path) = header.strip_prefix("Delete File: ") {
                changes.push(Change::new(path, ChangeKind::Deleted, None));
            } else if let Some(path) = header.strip_prefix("Move to: ") {
                changes.last_mut()?.moved_to = Some(path.to_owned());
            } else if header != "End of File" {
                return None;
            }
            continue;
        }
        let change = changes.last_mut()?;
        let (added, removed) = match (change.kind, line.as_bytes().first()) {
            (ChangeKind::Created, Some(b'+')) => (1, 0),
            (ChangeKind::Updated, Some(b'+')) => (1, 0),
            (ChangeKind::Updated, Some(b'-')) => (0, 1),
            (ChangeKind::Updated, Some(b' ') | None) => (0, 0),
            (ChangeKind::Updated, Some(b'@')) if line.starts_with("@@") => (0, 0),
            _ => return None,
        };
        change.added = change.added.map(|count| count.saturating_add(added));
        change.removed = change.removed.map(|count| count.saturating_add(removed));
    }
    (ended || !changes.is_empty()).then_some(changes)
}

/// The lines replacing `old` with `new` adds and removes, as a line diff
/// counts them: the lines of each that the longest run of lines common to
/// both, in order, leaves over. `None` when what differs between them spans
/// more than [`MOST_COMPARED`] pairs of lines.
pub(crate) fn replaced_lines(old: &str, new: &str) -> Option<(u64, u64)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    // What both start and end with is common, and not compared.
    let before = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let after = old[before..]
        .iter()
        .rev()
        .zip(new[before..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old = &old[before..old.len() - after];
    let new = &new[before..new.len() - after];
    if old.len().checked_mul(new.len())? > MOST_COMPARED {
        return None;
    }
    // The longest common subsequence, a row at a time.
    let mut previous = vec![0u32; new.len() + 1];
    let mut row = vec![0u32; new.len() + 1];
    for line in old {
        for (column, other) in new.iter().enumerate() {
            row[column + 1] = if line == other {
                previous[column] + 1
            } else {
                row[column].max(previous[column + 1])
            };
        }
        std::mem::swap(&mut previous, &mut row);
    }
    let common = u64::from(previous[new.len()]);
    let count = |lines: &[&str]| u64::try_from(lines.len()).unwrap_or(u64::MAX);
    Some((count(new) - common, count(old) - common))
}

/// The number `text` opens with after `prefix`, as `Exit code 2` gives 2
/// after `Exit code `. `None` when it doesn't open so.
pub(crate) fn leading_number(text: &str, prefix: &str) -> Option<i64> {
    let rest = text.strip_prefix(prefix)?;
    let end = rest
        .char_indices()
        .find(|(at, character)| !(character.is_ascii_digit() || (*at == 0 && *character == '-')))
        .map_or(rest.len(), |(at, _)| at);
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        Change, ChangeKind, Goal, PlanItem, StepStatus, Work, diff_lines, envelope, handoff,
        leading_number, lines, replaced_lines,
    };
    use crate::time::Instant;
    use crate::transcript::{Entry, Speaker, ToolCall};

    fn entry(index: u32, speaker: Speaker, text: &str) -> Entry {
        Entry {
            index,
            speaker,
            at: None,
            model: None,
            text: text.to_owned(),
            tool: None,
        }
    }

    #[test]
    fn a_diff_counts_only_what_its_hunks_hold() {
        // As OpenCode records an edit: a header, then one hunk of 3 old lines
        // and 4 new, 1 removed and 2 added among 2 of context. The "---" and
        // "+++" lines are the header, not a removal and an addition.
        let diff = "Index: a.rs\n===\n--- a.rs\n+++ a.rs\n@@ -1,3 +1,4 @@\n fn a() {\n-    1\n+    2\n+    3\n }\n";
        assert_eq!(diff_lines(diff), Some((2, 1)));
        // As Codex records one: hunks alone, a count of one left out, and a
        // removed line that is empty. Hunk by hunk, 1 + 1 + 2 + 0 = 4 added
        // and 0 + 1 + 2 + 1 = 4 removed.
        let codex = "@@ -4,2 +4,3 @@\n import (\n+\t\"cmp\"\n \t\"context\"\n@@ -78,3 +79,3 @@\n \tx\n-\ta\n+\tb\n \t})\n@@ -82,5 +83,5 @@\n \tif a {\n-\t\tone\n+\t\tuno\n \t\t}\n-\t\ttwo\n+\t\tdos\n \t})\n@@ -105,2 +106 @@\n var x\n-\n";
        assert_eq!(diff_lines(codex), Some((4, 4)));
        // A removed line that reads like a header, inside a hunk, is removed.
        assert_eq!(
            diff_lines("@@ -1 +0,0 @@\n--- not a header\n"),
            Some((0, 1))
        );
        // A hunk shorter than its header says, or a header that doesn't read,
        // is unknown.
        assert_eq!(diff_lines("@@ -1,3 +1,3 @@\n a\n"), None);
        assert_eq!(diff_lines("@@ -x +1 @@\n+a\n"), None);
        assert_eq!(diff_lines(""), Some((0, 0)));
    }

    #[test]
    fn an_envelope_counts_each_file_it_touches() {
        let patch = "*** Begin Patch\n*** Add File: src/new.rs\n+fn a() {}\n+\n*** Update File: src/old.rs\n*** Move to: src/moved.rs\n@@ fn b\n context\n-gone\n+here\n+also\n*** End of File\n*** Delete File: src/dead.rs\n*** End Patch\n";
        let changes = envelope(patch).unwrap();
        let mut moved = Change::new("src/old.rs", ChangeKind::Updated, Some((2, 1)));
        moved.moved_to = Some("src/moved.rs".into());
        assert_eq!(
            changes,
            [
                Change::new("src/new.rs", ChangeKind::Created, Some((2, 0))),
                moved,
                Change::new("src/dead.rs", ChangeKind::Deleted, None),
            ]
        );
        assert_eq!(envelope("diff --git a b"), None);
        // A line an added file can't hold.
        assert_eq!(
            envelope("*** Begin Patch\n*** Add File: a\n-no\n*** End Patch"),
            None
        );
        assert_eq!(
            envelope("*** Begin Patch\n*** Rename: a\n*** End Patch"),
            None
        );
    }

    #[test]
    fn an_edit_counts_the_lines_a_line_diff_finds() {
        // One line of three changed: 1 added, 1 removed.
        assert_eq!(replaced_lines("a\nb\nc", "a\nB\nc"), Some((1, 1)));
        // Two lines apart changed, the one between kept: 2 and 2, where
        // counting every line between the first and last change would say 3.
        assert_eq!(replaced_lines("a\nb\nc\nd", "A\nb\nC\nd"), Some((2, 2)));
        // Lines moved: "x" kept, "a" and "b" swapped around it: the longest
        // common run is 2 of the 3, so 1 and 1.
        assert_eq!(replaced_lines("a\nx\nb", "x\nb\na"), Some((1, 1)));
        assert_eq!(replaced_lines("", "one\ntwo"), Some((2, 0)));
        assert_eq!(replaced_lines("one line", "one line"), Some((0, 0)));
        // 2,001 lines for 2,001 different ones is past what is compared.
        let old = "o\n".repeat(2_001);
        let new = "n\n".repeat(2_001);
        assert_eq!(replaced_lines(&old, &new), None);
        assert_eq!(lines("a\nb\n"), 2);
        assert_eq!(lines("a\nb"), 2);
    }

    #[test]
    fn a_number_is_read_after_its_prefix() {
        assert_eq!(leading_number("Exit code 2\nerror", "Exit code "), Some(2));
        assert_eq!(leading_number("exit: 0\nok", "exit: "), Some(0));
        assert_eq!(leading_number("exit: -1", "exit: "), Some(-1));
        assert_eq!(leading_number("Exit code", "Exit code "), None);
        assert_eq!(leading_number("exit: 99999999999999999999", "exit: "), None);
    }

    #[test]
    fn a_handoff_adds_up_what_its_agent_did() {
        let mut call = entry(4, Speaker::Tool, "");
        call.tool = Some(ToolCall::default());
        // Slash commands alone ask nothing; a path the person gives does.
        let entries = [
            entry(0, Speaker::User, "/clear"),
            entry(1, Speaker::User, "Shade the hills"),
            entry(2, Speaker::Assistant, "Looking."),
            entry(3, Speaker::User, "/tmp/hills.png shows it"),
            call,
            entry(5, Speaker::Assistant, "Two tests still fail."),
            entry(6, Speaker::System, "Context"),
            entry(7, Speaker::User, "/compact"),
        ];
        let at = Instant::from_millis(1_000);
        let item = |id: &str, text: Option<&str>, status: Option<StepStatus>| PlanItem {
            id: Some(id.into()),
            text: text.map(str::to_owned),
            status,
            removed: false,
        };
        let work = [
            (
                at,
                Work::Planned(vec![
                    item("1", Some("Cache per tile"), Some(StepStatus::Pending)),
                    item("2", Some("Fix normals"), Some(StepStatus::Pending)),
                    item("3", Some("Snapshot test"), Some(StepStatus::Pending)),
                ]),
            ),
            (
                None,
                Work::Changed(Change::new("a.rs", ChangeKind::Created, Some((10, 0)))),
            ),
            (
                None,
                Work::Changed(Change::new("a.rs", ChangeKind::Updated, Some((3, 2)))),
            ),
            (
                None,
                Work::Changed(Change::new("b.rs", ChangeKind::Updated, Some((1, 1)))),
            ),
            (
                None,
                Work::Changed(Change::new("b.rs", ChangeKind::Deleted, None)),
            ),
            (
                None,
                Work::Revised(vec![
                    item("1", None, Some(StepStatus::Completed)),
                    PlanItem {
                        removed: true,
                        ..item("3", None, None)
                    },
                ]),
            ),
            (
                None,
                Work::Ran {
                    command: "cargo test".into(),
                    exit: Some(101),
                    failed: Some(true),
                },
            ),
            (
                None,
                Work::Goal(Goal {
                    objective: "Shade hills".into(),
                    status: Some("active".into()),
                }),
            ),
            (None, Work::Unclear("Edit".into())),
            (None, Work::Unclear("Edit".into())),
        ];
        let handoff = handoff(&entries, &work);
        assert_eq!(handoff.first_request.unwrap().text, "Shade the hills");
        assert_eq!(handoff.last_request.unwrap().entry, 3);
        assert_eq!(handoff.last_reply.unwrap().text, "Two tests still fail.");
        let plan: Vec<(&str, Option<StepStatus>)> = handoff
            .plan
            .iter()
            .map(|step| (step.text.as_str(), step.status))
            .collect();
        assert_eq!(
            plan,
            [
                ("Cache per tile", Some(StepStatus::Completed)),
                ("Fix normals", Some(StepStatus::Pending)),
            ]
        );
        // a.rs: created with 10 lines, then 3 added and 2 removed: 13 and 2.
        let a = &handoff.files[0];
        assert_eq!(
            (a.added, a.removed, a.created, a.deleted, a.changes),
            (Some(13), Some(2), true, false, 2)
        );
        // b.rs was deleted, which says nothing of its lines: unknown.
        let b = &handoff.files[1];
        assert_eq!((b.added, b.removed, b.deleted), (None, None, true));
        assert_eq!((handoff.commands_run, handoff.commands_failed), (1, 1));
        assert_eq!(handoff.commands[0].exit, Some(101));
        assert_eq!(handoff.goal.unwrap().objective, "Shade hills");
        assert_eq!(handoff.unclear, [("Edit".to_owned(), 2)]);
    }

    #[test]
    fn a_handoff_keeps_the_last_twenty_commands() {
        let work: Vec<_> = (0..25)
            .map(|n| {
                (
                    None,
                    Work::Ran {
                        command: format!("step {n}"),
                        exit: Some(0),
                        failed: Some(false),
                    },
                )
            })
            .collect();
        let handoff = handoff(&[], &work);
        assert_eq!(handoff.commands_run, 25);
        assert_eq!(handoff.commands.len(), 20);
        assert_eq!(handoff.commands[0].command, "step 5");
        assert_eq!(handoff.commands[19].command, "step 24");
    }
}
