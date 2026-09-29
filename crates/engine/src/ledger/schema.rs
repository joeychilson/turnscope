//! The ledger's schema: its migrations, in order, and running those a
//! ledger has not had. It only ever grows: a migration that has shipped is
//! never edited, and a ledger from a newer build is refused rather than
//! altered.

use rusqlite::Connection;

use crate::error::{Error, Result};

/// The migrations, in order. The ledger's `user_version` is how many have run.
pub(crate) const MIGRATIONS: &[&str] = &[
    r"
CREATE TABLE artifact (
    id INTEGER PRIMARY KEY,
    agent TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    device INTEGER NOT NULL,
    inode INTEGER NOT NULL,
    size INTEGER NOT NULL,
    modified INTEGER NOT NULL,
    fingerprint BLOB NOT NULL,
    read_to INTEGER NOT NULL,
    state BLOB NOT NULL,
    reader_version INTEGER NOT NULL,
    present INTEGER NOT NULL,
    read_at INTEGER NOT NULL
) STRICT;

CREATE TABLE session (
    id INTEGER PRIMARY KEY,
    agent TEXT NOT NULL,
    native TEXT NOT NULL,
    UNIQUE (agent, native)
) STRICT;

CREATE TABLE session_fact (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    session_id INTEGER NOT NULL REFERENCES session (id),
    started INTEGER,
    last INTEGER,
    cwd TEXT,
    branch TEXT,
    version TEXT,
    origin TEXT,
    title TEXT,
    title_rank INTEGER,
    PRIMARY KEY (artifact_id, session_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX session_fact_session ON session_fact (session_id);

CREATE TABLE session_link (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    child_id INTEGER NOT NULL REFERENCES session (id),
    parent_id INTEGER NOT NULL REFERENCES session (id),
    kind TEXT NOT NULL,
    PRIMARY KEY (artifact_id, child_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX session_link_parent ON session_link (parent_id);

CREATE TABLE observation (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    agent TEXT NOT NULL,
    response TEXT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES session (id),
    copy INTEGER NOT NULL,
    at INTEGER NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    output INTEGER NOT NULL,
    reasoning INTEGER NOT NULL,
    prompt INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    cost INTEGER,
    priority INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, response)
) STRICT, WITHOUT ROWID;
CREATE INDEX observation_response ON observation (agent, response);
CREATE INDEX observation_session ON observation (session_id);

CREATE TABLE report (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    session_id INTEGER NOT NULL REFERENCES session (id),
    scope TEXT NOT NULL,
    at INTEGER,
    model TEXT NOT NULL,
    input INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    output INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    cost INTEGER,
    PRIMARY KEY (artifact_id, session_id, model)
) STRICT, WITHOUT ROWID;
-- A report's model is empty when its totals cover every model the session used.

CREATE TABLE diagnostic (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    count INTEGER NOT NULL,
    first INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, kind, detail)
) STRICT, WITHOUT ROWID;

CREATE TABLE said (
    id INTEGER PRIMARY KEY,
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    session_id INTEGER NOT NULL REFERENCES session (id),
    at INTEGER,
    message TEXT
) STRICT;
CREATE INDEX said_artifact ON said (artifact_id, message);
-- Something the person or a model said. `message` names the message it is
-- part of when the artifact can give that message again as it changes.

CREATE VIRTUAL TABLE said_text USING fts5(
    text,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);
-- What the person and the models said, indexed for search by the row of
-- `said` it came from. The index keeps terms and their positions, not the
-- text, which is read from the agent's files when a match is shown. It has no
-- prefix indexes: they tripled the cost of writing it, and a search of one or
-- two letters is quick enough without them.

CREATE TABLE directory (
    path TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    name TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- The project a working directory belonged to when it was first seen, so a
-- repository moved or deleted later does not regroup its history.

CREATE TABLE identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    value INTEGER NOT NULL
) STRICT;
INSERT INTO identity (id, value) VALUES (1, random());
-- A number drawn when the ledger was made. The cache records the ledger it
-- was built from by it, and is built again for any other, as for a ledger
-- deleted and made anew while the cache was kept.

CREATE TABLE revision (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    value INTEGER NOT NULL
) STRICT;
INSERT INTO revision (id, value) VALUES (1, 0);

CREATE TABLE touched (
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL,
    agent TEXT NOT NULL,
    key TEXT NOT NULL
) STRICT;
CREATE INDEX touched_revision ON touched (revision);
-- What each write changed, for the cache to catch up on: a response whose
-- reports changed, a session whose facts, links, reports or presence changed,
-- or, with kind 'prices', every price. Rows the cache has caught up on are
-- removed.

CREATE TABLE price (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    since INTEGER NOT NULL,
    prices TEXT,
    PRIMARY KEY (provider, model, since)
) STRICT, WITHOUT ROWID;
-- A model's prices from `since` on; the earliest also stand for usage before
-- it. `prices` is NULL while the catalog listed the model unpriced.

CREATE TABLE catalog (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    as_of INTEGER NOT NULL,
    source TEXT NOT NULL,
    etag TEXT,
    body BLOB NOT NULL
) STRICT;
-- The catalog last taken in, for what it says of models besides their prices.

CREATE TABLE catalog_check (
    at INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL
) STRICT;

CREATE TABLE account (
    id TEXT PRIMARY KEY,
    subscription TEXT NOT NULL,
    label TEXT,
    plan TEXT,
    via TEXT NOT NULL,
    read_at INTEGER,
    problem TEXT,
    checked_at INTEGER NOT NULL,
    signed_in INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
-- A subscription account found signed in: the agents signed into it, by
-- their keys as a JSON list, when its limits were last read, why the latest
-- read failed, and whether an agent was signed into it when last looked for.
-- One signed in nowhere keeps its last readings until it is signed in again
-- or forgotten.

CREATE TABLE limit_reading (
    account TEXT NOT NULL,
    key TEXT NOT NULL,
    at INTEGER NOT NULL,
    name TEXT NOT NULL,
    scope TEXT,
    used REAL NOT NULL,
    starts INTEGER,
    resets INTEGER,
    PRIMARY KEY (account, key, at)
) STRICT, WITHOUT ROWID;

CREATE TABLE alert (
    account TEXT NOT NULL,
    key TEXT NOT NULL,
    kind TEXT NOT NULL,
    window INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (account, key, kind, window)
) STRICT, WITHOUT ROWID;
-- An alert sent, once per limit, kind and window, the window named by when it
-- resets.
",
    r"
ALTER TABLE artifact ADD COLUMN closing BLOB;
-- The bytes before where a log was last read to, to tell whether what was
-- read was rewritten since; NULL when not kept.
",
    r"
CREATE TABLE look (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    at INTEGER NOT NULL,
    failed TEXT NOT NULL
) STRICT;
-- The last look through every agent's history to finish: when, and what it
-- could not read, as a JSON list of [path, why], so an answer can say what
-- it may be missing.
",
    r"
CREATE TABLE session_absent (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    session_id INTEGER NOT NULL REFERENCES session (id),
    PRIMARY KEY (artifact_id, session_id)
) STRICT, WITHOUT ROWID;
-- A session an artifact speaks of without holding its transcript: one its
-- agent deleted from a database that holds many, which keeps what it said
-- of the session, or one whose conversation is kept in another of the
-- agent's files. Such an artifact doesn't make the session's transcript
-- one that can be read.
",
    r"
ALTER TABLE said ADD COLUMN ordinal INTEGER;
-- Where what was said sits in its session's numbered history, for an agent
-- that numbers it and can take it back from a point, as Codex does.

CREATE TABLE resumption (
    artifact_id INTEGER NOT NULL REFERENCES artifact (id),
    session_id INTEGER NOT NULL REFERENCES session (id),
    at INTEGER NOT NULL,
    base INTEGER,
    PRIMARY KEY (artifact_id, session_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX resumption_session ON resumption (session_id);
-- Where an artifact took up its session's numbered history, at `at`: from
-- the ordinal `base`, so that what the session said before `at`, numbered
-- `base` or later, was taken back; or, with no base, numbered afresh, so
-- that no later resumption takes back what was said before it.
",
    "
-- What the call that started a subagent is known by in its parent's
-- conversation, as the subagent's agent records it on both sides: a call's
-- id, a task's name, a description. The parent's conversation marks the
-- same key on the call (`Builder::launched`), which is how a call names the
-- subagent it started without anyone guessing.
ALTER TABLE session_link ADD COLUMN launch TEXT;
CREATE INDEX session_link_child ON session_link (child_id);
",
    "
CREATE TABLE folder (
    agent TEXT NOT NULL,
    path TEXT NOT NULL,
    read INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (agent, path)
) STRICT, WITHOUT ROWID;
-- A folder of an agent's the person chose to read, `read` 1, as another
-- account's folder, or chose not to, `read` 0, as one found or the agent's
-- own, and when. The person's choice, which stands over what is found.
",
    "
CREATE TABLE sign_in (
    agent TEXT NOT NULL,
    folder TEXT NOT NULL,
    provider TEXT NOT NULL,
    held TEXT NOT NULL,
    account TEXT,
    first INTEGER NOT NULL,
    last INTEGER NOT NULL
) STRICT;
CREATE INDEX sign_in_place ON sign_in (agent, folder, provider, last);
-- What one place an agent keeps a sign-in held, from the first look that
-- found it so to the last: the place its folder and the provider its usage
-- names when it uses the sign-in; what it held `account`, a sign-in to the
-- account named, or `nothing`. A look that finds what the latest span of a
-- place holds stretches it; one that finds anything else begins another.
",
    "
CREATE TABLE account_setting (
    account TEXT PRIMARY KEY,
    name TEXT,
    hidden INTEGER NOT NULL DEFAULT 0,
    budget INTEGER
) STRICT, WITHOUT ROWID;
-- What the person set for an account: a name for it, as `work`, whether it
-- is hidden, and for an API-key account a budget for each calendar month,
-- in nano-dollars. The person's choices, which nothing read can make again.
--
-- A place in `sign_in` can also have held `key`, an API key, or `other`, a
-- sign-in to something Turnscope doesn't read.
",
    "
DROP TABLE folder;
ALTER TABLE account_setting DROP COLUMN name;
ALTER TABLE account_setting DROP COLUMN budget;
-- The person no longer adds or removes folders, names accounts, or gives
-- an API-key account a budget: of what they set, only whether an account
-- is hidden stays.
",
];

/// Run the migrations `connection` has not had.
pub(super) fn migrate(connection: &mut Connection) -> Result<()> {
    let known = i64::try_from(MIGRATIONS.len())
        .map_err(|_| Error::corrupt("migration count", "out of range"))?;
    let found: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > known {
        return Err(Error::NewerLedger { found, known });
    }
    let done = usize::try_from(found)
        .map_err(|_| Error::corrupt("ledger schema version", format!("{found} is negative")))?;
    for (version, migration) in (1_i64..).zip(MIGRATIONS).skip(done) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
}
