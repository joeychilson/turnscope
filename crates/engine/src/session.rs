//! Sessions: what a conversation is, and how conversations relate.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::agent::Agent;
use crate::time::Instant;

/// The longest title kept, in characters.
const LONGEST_TITLE: usize = 200;

/// Which session something belongs to.
///
/// The agent's own id for the session, qualified by the agent. It does not
/// change when the ledger is rebuilt, so what the feed and MCP clients
/// remember stays valid; it is written `claude-code:0f6e3f6a-…`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    agent: Agent,
    native: String,
}

impl SessionKey {
    /// The session `native` names within `agent`.
    pub fn new(agent: Agent, native: impl Into<String>) -> SessionKey {
        SessionKey {
            agent,
            native: native.into(),
        }
    }

    /// The agent the session belongs to.
    pub fn agent(&self) -> Agent {
        self.agent
    }

    /// The agent's own id for the session.
    pub fn native(&self) -> &str {
        &self.native
    }

    /// The session a key written as [`SessionKey`] displays it names:
    /// `claude-code:0f6e3f6a-…`. `None` when it names no agent.
    pub fn parse(text: &str) -> Option<SessionKey> {
        let (agent, native) = text.split_once(':')?;
        Some(SessionKey::new(Agent::from_key(agent)?, native))
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.agent.key(), self.native)
    }
}

/// How one session came from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkKind {
    /// Run by the parent to do part of its work, within the parent's process.
    /// A subagent's usage is part of its parent's totals.
    Subagent,
    /// Run by the agent to review what the parent was about to do, as Codex
    /// reviews a request for approval.
    Review,
    /// Branched from the parent's conversation into a conversation of its own.
    Fork,
    /// Carries on where the parent left off, as a new session.
    Continuation,
}

impl LinkKind {
    /// The kind with `key`.
    pub(crate) fn from_key(key: &str) -> Option<LinkKind> {
        [
            LinkKind::Subagent,
            LinkKind::Review,
            LinkKind::Fork,
            LinkKind::Continuation,
        ]
        .into_iter()
        .find(|kind| kind.key() == key)
    }

    /// Whether a child of this kind runs within its parent, so its usage is
    /// part of the parent's work: a subagent or a review, not a fork or a
    /// continuation.
    pub(crate) fn within(self) -> bool {
        matches!(self, LinkKind::Subagent | LinkKind::Review)
    }

    /// The kind as the ledger stores it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            LinkKind::Subagent => "subagent",
            LinkKind::Review => "review",
            LinkKind::Fork => "fork",
            LinkKind::Continuation => "continuation",
        }
    }
}

/// A session, the session it came from, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionLink {
    /// The session that came from the other.
    pub child: SessionKey,
    /// The session it came from.
    pub parent: SessionKey,
    /// How.
    pub kind: LinkKind,
    /// For a subagent, what the call in its parent's conversation that
    /// started it is known by, as its agent records it on both sides: the
    /// key [`crate::transcript::Builder::launched`] marks that call with.
    pub launch: Option<String>,
}

/// Where a session's title came from, from least to most authoritative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TitleSource {
    /// The start of the first thing the user asked.
    Prompt,
    /// What the session was started to do, as a subagent's task is described.
    Description,
    /// A title the agent generated.
    Generated,
    /// A name the user gave the session.
    Named,
}

impl TitleSource {
    /// The source with `rank`.
    pub(crate) fn from_rank(rank: i64) -> Option<TitleSource> {
        [
            TitleSource::Prompt,
            TitleSource::Description,
            TitleSource::Generated,
            TitleSource::Named,
        ]
        .into_iter()
        .find(|source| source.rank() == rank)
    }

    /// The source's rank; a higher rank replaces a lower.
    pub(crate) fn rank(self) -> i64 {
        match self {
            TitleSource::Prompt => 1,
            TitleSource::Description => 2,
            TitleSource::Generated => 3,
            TitleSource::Named => 4,
        }
    }
}

/// A session's title, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Title {
    /// Where the title came from.
    pub source: TitleSource,
    /// The title: one line, at most 200 characters.
    pub text: String,
}

impl Title {
    /// A title from `text`, reduced to its first non-blank line with runs of
    /// whitespace made single spaces, and cut to 200 characters. `None` when
    /// nothing is left.
    pub(crate) fn new(source: TitleSource, text: &str) -> Option<Title> {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        let mut kept = String::with_capacity(line.len().min(LONGEST_TITLE * 4));
        for word in line.split_whitespace() {
            if !kept.is_empty() {
                kept.push(' ');
            }
            kept.push_str(word);
        }
        if let Some((cut, _)) = kept.char_indices().nth(LONGEST_TITLE) {
            kept.truncate(cut);
        }
        Some(Title { source, text: kept })
    }
}

/// What one artifact says about one session.
///
/// An artifact is read in parts as it grows. The ledger combines what each
/// part says: the earliest start and latest activity, the first directory,
/// the latest branch and version, the first origin, and the most
/// authoritative title, the latest among equals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionFacts {
    /// The earliest instant seen.
    pub started: Option<Instant>,
    /// The latest instant seen.
    pub last: Option<Instant>,
    /// The directory the session started in.
    pub cwd: Option<String>,
    /// The git branch most recently recorded.
    pub branch: Option<String>,
    /// The agent's version most recently recorded.
    pub version: Option<String>,
    /// How the session was started, such as `cli` or `Codex Desktop`.
    pub origin: Option<String>,
    /// The most authoritative title seen, the latest among equals.
    pub title: Option<Title>,
}

impl SessionFacts {
    /// Note activity at `at`.
    pub(crate) fn saw(&mut self, at: Instant) {
        self.started = Some(self.started.map_or(at, |started| started.min(at)));
        self.last = Some(self.last.map_or(at, |last| last.max(at)));
    }

    /// Fold in facts from an artifact read after the one these came from.
    pub(crate) fn absorb(&mut self, later: SessionFacts) {
        if let Some(at) = later.started {
            self.saw(at);
        }
        if let Some(at) = later.last {
            self.saw(at);
        }
        if self.cwd.is_none() {
            self.cwd = later.cwd;
        }
        if later.branch.is_some() {
            self.branch = later.branch;
        }
        if later.version.is_some() {
            self.version = later.version;
        }
        if self.origin.is_none() {
            self.origin = later.origin;
        }
        self.offer_title(later.title);
    }

    /// Offer a title, which replaces the current one unless that came from a
    /// more authoritative source.
    pub(crate) fn offer_title(&mut self, title: Option<Title>) {
        if let Some(title) = title
            && self
                .title
                .as_ref()
                .is_none_or(|current| title.source >= current.source)
        {
            self.title = Some(title);
        }
    }
}

/// `word` as one word of a shell command: as it is when it holds nothing a
/// shell reads specially, or in single quotes.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./:@%+=,".contains(character));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// How sessions run within one another: subagents and reviews under the
/// sessions that ran them.
pub(crate) struct Tree {
    parents: HashMap<SessionKey, SessionKey>,
    children: HashMap<SessionKey, Vec<SessionKey>>,
}

impl Tree {
    /// The tree `parents`, where every session that came from another came
    /// from, makes: those run within their parents, subagents and reviews.
    pub(crate) fn of(parents: &HashMap<SessionKey, (SessionKey, LinkKind)>) -> Tree {
        let mut tree = Tree {
            parents: HashMap::new(),
            children: HashMap::new(),
        };
        // In key order, so each parent lists its children the same way
        // whatever order the map holds them in.
        let mut within: Vec<(&SessionKey, &SessionKey)> = parents
            .iter()
            .filter(|(_, (_, kind))| kind.within())
            .map(|(child, (parent, _))| (child, parent))
            .collect();
        within.sort();
        for (child, parent) in within {
            tree.parents.insert(child.clone(), parent.clone());
            tree.children
                .entry(parent.clone())
                .or_default()
                .push(child.clone());
        }
        tree
    }

    /// Every session above `key`, nearest first, each once.
    pub(crate) fn ancestors(&self, key: &SessionKey) -> Vec<SessionKey> {
        let mut seen = HashSet::new();
        let mut above = Vec::new();
        let mut current = key;
        while let Some(parent) = self.parents.get(current) {
            if !seen.insert(parent.clone()) {
                break;
            }
            above.push(parent.clone());
            current = parent;
        }
        above
    }

    /// The session at the top of `key`'s tree.
    pub(crate) fn root(&self, key: &SessionKey) -> SessionKey {
        self.ancestors(key).pop().unwrap_or_else(|| key.clone())
    }

    /// `key` and every session under it, each once.
    pub(crate) fn below(&self, key: &SessionKey) -> Vec<SessionKey> {
        let mut seen = HashSet::from([key.clone()]);
        let mut all = vec![key.clone()];
        let mut next = 0;
        while let Some(current) = all.get(next).cloned() {
            next += 1;
            for child in self.children.get(&current).into_iter().flatten() {
                if seen.insert(child.clone()) {
                    all.push(child.clone());
                }
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{LinkKind, SessionFacts, SessionKey, Title, TitleSource, Tree};
    use crate::agent::Agent;

    #[test]
    fn a_tree_holds_each_session_once_even_when_links_loop() {
        let key = |native: &str| SessionKey::new(Agent::ClaudeCode, native);
        let parents = HashMap::from([
            (key("2"), (key("1"), LinkKind::Subagent)),
            (key("3"), (key("1"), LinkKind::Subagent)),
            (key("4"), (key("2"), LinkKind::Subagent)),
            (key("1"), (key("4"), LinkKind::Subagent)),
            // A fork is a session of its own, not part of its parent's work.
            (key("5"), (key("1"), LinkKind::Fork)),
        ]);
        let mut below = Tree::of(&parents).below(&key("1"));
        below.sort();
        assert_eq!(below, [key("1"), key("2"), key("3"), key("4")]);
    }

    #[test]
    fn a_title_is_its_first_line_tidied_and_cut_on_a_character_boundary() {
        let title = Title::new(TitleSource::Prompt, "\n  Fix   the\tbuild  \nthen deploy").unwrap();
        assert_eq!(title.text, "Fix the build");
        assert_eq!(Title::new(TitleSource::Prompt, " \n\t "), None);
        // 250 two-byte characters, cut to 200 of them.
        let long = Title::new(TitleSource::Prompt, &"é".repeat(250)).unwrap();
        assert_eq!(long.text, "é".repeat(200));
    }

    #[test]
    fn a_title_gives_way_only_to_one_at_least_as_authoritative() {
        let mut facts = SessionFacts::default();
        facts.offer_title(Title::new(TitleSource::Generated, "Generated"));
        facts.offer_title(Title::new(TitleSource::Prompt, "First prompt"));
        assert_eq!(facts.title.as_ref().unwrap().text, "Generated");
        facts.offer_title(Title::new(TitleSource::Generated, "Regenerated"));
        assert_eq!(facts.title.as_ref().unwrap().text, "Regenerated");
    }

    #[test]
    fn a_shell_word_is_quoted_only_when_it_must_be() {
        use super::shell_word;
        assert_eq!(shell_word("0f6e3f6a-713c"), "0f6e3f6a-713c");
        assert_eq!(shell_word("/Users/me/work/ledger"), "/Users/me/work/ledger");
        assert_eq!(shell_word("/Users/me/My Apps"), "'/Users/me/My Apps'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word("019a; rm -rf ~"), "'019a; rm -rf ~'");
        assert_eq!(shell_word(""), "''");
    }
}
