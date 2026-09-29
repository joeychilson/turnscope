//! Searching what was said.
//!
//! The ledger indexes what the person typed and what models replied, as the
//! entries of a conversation show them, and nothing an agent injected or a tool
//! returned. A search's words match in any order, ignoring case and accents,
//! and all of them within one entry: each as typed, as the start of a longer
//! word, or, for an English word, as another of its forms, so "notarizing"
//! finds "notarization". The same rules find the excerpt shown for a session,
//! so an excerpt shows why it matched while the conversation still holds it;
//! a conversation rewound past the match no longer does.
//!
//! Left out of the index besides tools and injected text are a compacted
//! conversation's summary, which Claude Code sends as the person's; a Codex
//! review's requests, which hand the reviewing model the whole transcript
//! under review; and a fork's or subagent's copy of its parent's history.
//!
//! **The index** is a contentless FTS5 table with `contentless_delete`: it
//! stores terms and positions, not the text, which stays in the agents' own
//! files. A file read again from the start forgets what it said, and a
//! message read again as it changes replaces its words, even with none, so
//! nothing is counted twice or kept after it is taken back. Hits are grouped
//! by session, the most recent match first, with how many entries matched;
//! Grok Build records no time for what was said, so a session whose matches
//! have none takes its place by when it was last active.
//!
//! **Excerpts** read the conversation now and match it in an in-memory FTS5
//! table of the same text with the same tokenizer, so they agree with the
//! index.
//!
//! **Other forms of a word.** A word of the letters A to Z also matches the
//! index's words with the same stem by Snowball's English stemmer (Porter2):
//! "notarizing" finds "notarize" and "notarization", and "tests" finds
//! "testing". The index is not stemmed. A search looks its words up in the
//! index's own list of words ([`TERMS`]) and asks for the forms it finds
//! there, so it finds them in all the history the ledger keeps, what files
//! since deleted said included, and the stemmer can change without reading
//! anything again. A word with a stem under [`STEM`] letters has no other
//! forms looked for: they would be looked for among every word that starts
//! with its first letter. Stems join a few words that aren't related, as
//! "organization" and "organic", but only ever add to what matches as typed,
//! and words in other languages match as typed alone.
//!
//! Every word starts with its stem but for the stem's last letter, so a
//! word's forms are looked for among the index's words that start so, and,
//! for a stem ending in "ie", among those starting with the stem less that
//! and a "y". Measured (2026-09-27): of the 235,974 words of macOS's word
//! list and the 24,769 words of the letters A to Z in this Mac's index, only
//! "dying", "lying" and "tying", which Snowball stems to "die", "lie" and
//! "tie", don't start with the stem less its last letter; stemming took 0.2
//! to 0.3 µs a word. Looking a word's forms up among the 32,487 words of
//! this Mac's index took 0.04 to 0.8 ms, the most for a stem of three
//! letters, whose forms are looked for among the 100 to 350 words that share
//! its first two. What a search takes besides follows the sessions it finds:
//! "searched" found 337 sessions in place of 24, in 16 ms in place of 7, as
//! "search" finds 349 in 16; "testing" 877 in place of 209, in 35 ms in
//! place of 14, as "tests" finds them. Each is the fastest of 75 searches in
//! a release build, the machine being busy.
//!
//! A stemmed index beside this one was turned down. It is contentless, so it
//! could be filled only from the files still there, and what deleted files
//! said would never be found by another form; it would also fix a stemmer in
//! the ledger, and SQLite has only Porter's first. A stemmed index in place
//! of this one would also lose half-typed words: it keeps "notar" for
//! "notarization", which "notariz" isn't the start of.
//!
//! **Measured on this Mac** (2026-09-23): 27,452 entries made an 8 MB index,
//! and writing it added nothing measurable to the 3 to 5 seconds a full read
//! of 5.5 GB took. A search took 0.3 to 3 ms for ordinary words, and up to
//! 28 ms for one or two letters; an excerpt 19 ms on average. For 30 words,
//! all 1,436 hits had an excerpt. For every Grok Build, OpenCode and Pi
//! session, the entries indexed equalled the entries shown, and so for
//! Claude Code, but for images, which it then showed as a placeholder with
//! no words, and sessions written during the check. Two choices were measured and turned down: prefix
//! indexes (`prefix = '2 3'`) tripled the cost of writing the index and made
//! short searches slower, not faster; and `detail = none` made the index a
//! third the size but can't match phrases, which words the tokenizer divides
//! further, such as Devanagari with its vowel signs, become.

use std::cmp::Reverse;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params};
use rust_stemmers::{Algorithm, Stemmer};

use crate::error::{Error, Result};
use crate::ledger::Ledger;
use crate::query::{self, Filter, Page};
use crate::session::SessionKey;
use crate::time::{Instant, Span};
use crate::transcript::{Entry, Speaker};

/// How the index and excerpts divide text into words: at anything but letters
/// and digits, with case and accents folded away. The ledger's index was
/// created with the same, and changing it takes a migration that rebuilds it.
pub(crate) const TOKENIZE: &str = "unicode61 remove_diacritics 2";

/// The table listing an index's words, one to a row, that a search looks its
/// words' other forms up in: an `fts5vocab` table over the ledger's index,
/// made in each of the ledger's connections' `temp` schema, where it writes
/// nothing to the ledger, and over an index in memory beside it.
pub(crate) const TERMS: &str = "said_terms";

/// The fewest letters a stem has for other forms of its word to be looked
/// for.
const STEM: usize = 3;

/// A search of what was said, in the sessions the rest of the question
/// admits, as a list of sessions admits them.
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    /// The words to look for, all of them in one entry: each as typed, as the
    /// start of a longer word, or, for an English word, as another of its
    /// forms.
    pub words: String,
    /// Only sessions with usage in this time, their own or their subagents',
    /// as [`crate::SessionQuery::span`] keeps them.
    pub span: Span,
    /// Only sessions it keeps, as [`crate::SessionQuery::filter`] does.
    pub filter: Filter,
    /// At most this many to a page, from 1 to 1,000.
    pub limit: usize,
    /// Where the last page ended, as its `next` said: the page after it in
    /// the same order.
    pub after: Option<String>,
}

/// A session in which something said matched a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// The session.
    pub session: SessionKey,
    /// How many entries of its conversation matched.
    pub matches: u64,
    /// When the last of them was said, where known.
    pub last: Option<Instant>,
}

/// A line of a conversation showing why it matched a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Excerpt {
    /// The matching entry's index in the conversation.
    pub index: u32,
    /// About a dozen words around the first match, on one line, with an
    /// ellipsis where text was left out.
    pub text: String,
}

/// The page of sessions `question` finds, as [`crate::Engine::search`] says,
/// from the ledger's index and the cache's sessions, each read in one view.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the ledger or the cache cannot be
/// read, and [`crate::Error::Cursor`] when `after` is not where a page ended.
pub(crate) fn sessions(
    ledger: &Ledger,
    cache: &Connection,
    question: &SearchQuery,
) -> Result<Page<SearchHit>> {
    let mut found: Vec<(SearchHit, Option<i64>)> = ledger.search(&question.words)?;
    // Those the rest of the question admits, as a list of sessions would,
    // found before paging, so a page is never short of what the filter
    // leaves out.
    let keys: Vec<SessionKey> = found.iter().map(|(hit, _)| hit.session.clone()).collect();
    let admitted = query::admitted(cache, &keys, &question.span, &question.filter)?;
    found.retain(|(hit, _)| admitted.contains(&hit.session));
    // The page after the cursor, `placed|key`, in the order found.
    let from = match question.after.as_deref() {
        None => 0,
        Some(after) => {
            let (placed, key) = after
                .split_once('|')
                .and_then(|(placed, key)| {
                    let placed = match placed {
                        "" => None,
                        placed => Some(placed.parse::<i64>().ok()?),
                    };
                    Some((placed, SessionKey::parse(key)?))
                })
                .ok_or_else(|| Error::Cursor(after.to_owned()))?;
            let cursor = placing(placed, &key);
            found.partition_point(|(hit, placed)| placing(*placed, &hit.session) <= cursor)
        }
    };
    let rest = &found[from..];
    let page = &rest[..rest.len().min(question.limit.clamp(1, 1_000))];
    let next = page
        .last()
        .filter(|_| rest.len() > page.len())
        .map(|(hit, placed)| {
            let placed = placed.map(|placed| placed.to_string()).unwrap_or_default();
            format!("{placed}|{}", hit.session)
        });
    Ok(Page {
        items: page.iter().map(|(hit, _)| hit.clone()).collect(),
        next,
    })
}

/// Where a hit placed at `placed` in `session` falls in the order the
/// ledger finds them: the latest placed first, those with no place last,
/// and then by agent and id, as the ledger orders them. As one text, an
/// agent whose key starts with another's would sort apart from its place.
fn placing(placed: Option<i64>, session: &SessionKey) -> (bool, Reverse<i64>, &str, &str) {
    (
        placed.is_none(),
        Reverse(placed.unwrap_or_default()),
        session.agent().key(),
        session.native(),
    )
}

/// The words of a search: each run of letters and digits in `text`.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// Whether `text` holds a word to search for.
pub(crate) fn has_words(text: &str) -> bool {
    words(text).next().is_some()
}

/// The FTS5 query for `words` over the index whose words [`TERMS`] lists in
/// `connection`: each word as a quoted prefix term, so nothing typed reads as
/// query syntax, or as any of its other forms that the index holds, and all
/// the words in one entry. `None` when `words` holds no word.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the index's words can't be read.
pub(crate) fn expression(connection: &Connection, words: &str) -> Result<Option<String>> {
    let stemmer = Stemmer::create(Algorithm::English);
    let mut terms = Vec::new();
    for word in self::words(words) {
        let forms = forms(connection, &stemmer, word)?;
        terms.push(if forms.is_empty() {
            format!("\"{word}\"*")
        } else {
            let forms: Vec<String> = forms.iter().map(|form| format!("\"{form}\"")).collect();
            format!("(\"{word}\"* OR {})", forms.join(" OR "))
        });
    }
    Ok((!terms.is_empty()).then(|| terms.join(" AND ")))
}

/// The words of the index, but those `word` is the start of, that are other
/// forms of it: those with its stem. None for a word not all of the letters
/// A to Z, or with a stem under [`STEM`] letters.
fn forms(connection: &Connection, stemmer: &Stemmer, word: &str) -> Result<Vec<String>> {
    let english = |word: &str| word.bytes().all(|byte| byte.is_ascii_lowercase());
    let word = word.to_ascii_lowercase();
    if !english(&word) {
        return Ok(Vec::new());
    }
    let stem = stemmer.stem(&word);
    if stem.len() < STEM || !english(&stem) {
        return Ok(Vec::new());
    }
    // Where the words with this stem start, as the module's documentation
    // measures it.
    let mut starts = vec![stem[..stem.len() - 1].to_owned()];
    if let Some(root) = stem.strip_suffix("ie") {
        starts.push(format!("{root}y"));
    }
    // The words from `start` up to its end among the letters A to Z, '{'
    // being the character after 'z'.
    let mut statement = connection.prepare_cached(&format!(
        "SELECT term FROM {TERMS} WHERE term >= ?1 AND term < ?1 || '{{'"
    ))?;
    let mut forms = Vec::new();
    for start in starts {
        let mut rows = statement.query(params![start])?;
        while let Some(row) = rows.next()? {
            let ValueRef::Text(term) = row.get_ref(0)? else {
                continue;
            };
            // Every word of the letters A to Z is UTF-8, so one that isn't
            // is no form of the word.
            let Ok(term) = std::str::from_utf8(term) else {
                continue;
            };
            if english(term) && !term.starts_with(&word) && stemmer.stem(term) == stem {
                forms.push(term.to_owned());
            }
        }
    }
    Ok(forms)
}

/// The first of `entries` the person or a model said that matches `words`,
/// with the words around the match. `None` when none does.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when SQLite cannot build the index in
/// memory.
pub(crate) fn excerpt(entries: &[Entry], words: &str) -> Result<Option<Excerpt>> {
    let connection = said(entries)?;
    let Some(expression) = expression(&connection, words)? else {
        return Ok(None);
    };
    let found: Option<(i64, String)> = connection
        .query_row(
            "SELECT rowid, snippet(said, 0, '', '', '…', 16) FROM said
             WHERE said MATCH ?1 ORDER BY rowid LIMIT 1",
            [expression],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(found.and_then(|(index, snippet)| {
        Some(Excerpt {
            index: u32::try_from(index).ok()?,
            text: snippet.split_whitespace().collect::<Vec<_>>().join(" "),
        })
    }))
}

/// An index in memory, `said`, of what the person and the models said in
/// `entries`, each by its index in the conversation, as the ledger's index
/// reads it, with its words listed in [`TERMS`].
fn said(entries: &[Entry]) -> Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    connection.execute_batch(&format!(
        "CREATE VIRTUAL TABLE said USING fts5(text, tokenize = '{TOKENIZE}');
         CREATE VIRTUAL TABLE {TERMS} USING fts5vocab(said, row);"
    ))?;
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction.prepare("INSERT INTO said (rowid, text) VALUES (?1, ?2)")?;
        for entry in entries {
            if matches!(entry.speaker, Speaker::User | Speaker::Assistant) {
                insert.execute((i64::from(entry.index), &entry.text))?;
            }
        }
    }
    transaction.commit()?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::{TOKENIZE, excerpt, expression, said};
    use crate::transcript::{Entry, Speaker};

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
    fn excerpts_divide_words_as_the_ledger_index_does() {
        let index = format!("tokenize = '{TOKENIZE}'");
        assert!(
            crate::ledger::MIGRATIONS
                .iter()
                .any(|migration| migration.contains(&index))
        );
    }

    /// The FTS5 query for `words` over what `entries` said.
    fn query(entries: &[Entry], words: &str) -> Option<String> {
        expression(&said(entries).unwrap(), words).unwrap()
    }

    #[test]
    fn typed_words_become_quoted_prefix_terms() {
        assert_eq!(
            query(&[], "\"fix\" the NEAR(build*").as_deref(),
            Some("\"fix\"* AND \"the\"* AND \"NEAR\"* AND \"build\"*")
        );
        assert_eq!(query(&[], "  -*()  "), None);
    }

    #[test]
    fn a_word_also_matches_its_other_forms_the_index_holds() {
        let entries = [
            entry(0, Speaker::User, "Notarization failed; notarizing again."),
            entry(1, Speaker::Assistant, "Notarized. The notary tool agrees."),
            entry(2, Speaker::Tool, "notarize: done"),
        ];
        // Snowball stems "notarizing", "notarization", "notarize" and
        // "notarized" to "notar", and "notary" to "notari". Of the words
        // said, those starting "notar" and stemmed to "notar", less
        // "notarizing", which the prefix finds already; "notarize", only a
        // tool's, is no word of the index.
        assert_eq!(
            query(&entries, "Notarizing").as_deref(),
            Some("(\"Notarizing\"* OR \"notarization\" OR \"notarized\")")
        );
        // Half typed, a word's stem is itself: nothing but the prefix.
        assert_eq!(query(&entries, "notariz").as_deref(), Some("\"notariz\"*"));
    }

    #[test]
    fn only_words_of_the_letters_a_to_z_with_stems_of_three_have_other_forms() {
        let entries = [entry(
            0,
            Speaker::User,
            "Is it the cafés' cafe? GPT5 gpt tested; as asked.",
        )];
        // "is" and "as" stem to themselves, under three letters; "gpt5" has a
        // digit; "cafés" an accent, though the index keeps it as "cafes".
        assert_eq!(
            query(&entries, "is gpt5 cafés as").as_deref(),
            Some("\"is\"* AND \"gpt5\"* AND \"cafés\"* AND \"as\"*")
        );
        // "tests" stems to "test", as "tested" does; "cafe" to "cafe", as
        // "cafes" does, which "cafe" is the start of already.
        assert_eq!(
            query(&entries, "tests cafe").as_deref(),
            Some("(\"tests\"* OR \"tested\") AND \"cafe\"*")
        );
    }

    #[test]
    fn a_stem_ending_ie_finds_the_words_with_y_it_stands_for() {
        let entries = [
            entry(0, Speaker::User, "The worker died at noon."),
            entry(1, Speaker::Assistant, "It was dying slowly."),
        ];
        // Snowball stems "die", "died" and "dying" to "die", and "dying"
        // alone doesn't start "di".
        assert_eq!(
            query(&entries, "die").as_deref(),
            Some("(\"die\"* OR \"dying\")")
        );
        let found = excerpt(&entries[1..], "die").unwrap().unwrap();
        assert_eq!(found.index, 1);
    }

    #[test]
    fn an_excerpt_is_the_first_entry_said_that_holds_every_word_on_one_line() {
        let entries = [
            entry(0, Speaker::System, "Migrations run at startup."),
            entry(1, Speaker::User, "Add a watcher"),
            entry(
                2,
                Speaker::Assistant,
                "The café's\nmigration runs   before the watcher starts.",
            ),
            entry(3, Speaker::Tool, "the café's watcher"),
        ];
        // Another form, the start of a word, and a word without its
        // accent: each matches.
        let found = excerpt(&entries, "cafe MIGR running").unwrap().unwrap();
        assert_eq!(found.index, 2);
        assert_eq!(
            found.text,
            "The café's migration runs before the watcher starts."
        );
        // What the agent injected or a tool returned was never said, and
        // words said apart match in no entry.
        assert_eq!(excerpt(&entries, "startup").unwrap(), None);
        assert_eq!(excerpt(&entries, "add migration").unwrap(), None);
    }

    #[test]
    fn a_word_the_index_divides_further_is_found_as_a_phrase() {
        // Devanagari vowel signs are letters to Rust and marks, which divide
        // words, to the index.
        let entries = [entry(0, Speaker::User, "Translate the हिन्दी strings")];
        let found = excerpt(&entries, "हिन्दी").unwrap().unwrap();
        assert_eq!(found.text, "Translate the हिन्दी strings");
    }

    #[test]
    fn a_long_message_is_cut_around_the_match() {
        let text = format!("{} needle {}", "hay ".repeat(40), "straw ".repeat(40));
        let found = excerpt(&[entry(0, Speaker::User, &text)], "needle")
            .unwrap()
            .unwrap();
        assert!(found.text.starts_with('…') && found.text.ends_with('…'));
        assert!(found.text.contains(" needle "), "{}", found.text);
        assert!(found.text.split(' ').count() <= 17, "{}", found.text);
    }
}
