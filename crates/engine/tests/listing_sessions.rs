//! A list of sessions holds every session with usage that answers the
//! question, its own or its subagents', and pages through them without
//! losing or repeating one.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod subagents;
}

use serde_json::{Value, json};
use turnscope_engine::{Error, Filter, Instant, ModelKey, SessionOrder, SessionQuery, Span};

use history::claude_code::{self, response};
use history::home::{Home, write};
use history::subagents;

const PARENT: &str = "9e5f7084-d983-49d9-b587-0cf04a7b4b98";
const EXPLORE: &str = "ac9cd968c38c96ac1";

/// A response's usage: 32 tokens in, `cache_read` read from the cache and
/// `output` out.
fn usage(cache_read: u64, output: u64) -> Value {
    json!({"input_tokens": 32, "cache_read_input_tokens": cache_read, "output_tokens": output})
}

/// Every session of all time, most recently active first.
fn every_session() -> SessionQuery {
    SessionQuery {
        empty: true,
        limit: 10,
        ..SessionQuery::default()
    }
}

fn at(text: &str) -> Instant {
    Instant::parse(text).unwrap()
}

#[test]
fn a_session_whose_subagent_alone_matches_is_listed_as_facts_count_it() {
    let home = Home::new();
    // The parent asks Claude Opus 5 at 12:00, and its Explore subagent runs
    // Claude Haiku 5 at 12:20.
    write(
        &claude_code::session(home.path(), PARENT),
        &[response(
            PARENT,
            None,
            "2026-09-14T12:00:10.000Z",
            "1",
            "claude-opus-5",
            usage(1_000, 100),
        )],
    );
    write(
        &subagents::log(home.path(), PARENT, EXPLORE),
        &[response(
            PARENT,
            Some(EXPLORE),
            "2026-09-14T12:20:50.000Z",
            "2",
            "claude-haiku-5",
            usage(500, 5),
        )],
    );
    subagents::describe(
        home.path(),
        PARENT,
        EXPLORE,
        json!({"agentType": "Explore", "description": "Find the ledger's tests"}),
    );
    let engine = home.scanned();

    let haiku = Filter {
        models: vec![ModelKey::of("claude-haiku-5")],
        ..Filter::default()
    };
    // From 12:15 to 12:30, only the subagent's response.
    let explored = Span {
        from: Some(at("2026-09-14T12:15:00Z")),
        until: Some(at("2026-09-14T12:30:00Z")),
    };
    for (span, filter) in [
        (Span::default(), haiku.clone()),
        (explored, Filter::default()),
    ] {
        let listed = engine
            .sessions(&SessionQuery {
                span,
                filter,
                ..every_session()
            })
            .unwrap();
        let rows: Vec<(String, u64, u64)> = listed
            .items
            .iter()
            .map(|row| {
                (
                    row.key.to_string(),
                    row.totals.tokens.total(),
                    row.with_subagents.tokens.total(),
                )
            })
            .collect();
        // The parent used none of it itself; with its subagent, 32 in, 500
        // read from the cache and 5 out.
        assert_eq!(rows, [(format!("claude-code:{PARENT}"), 0, 537)]);
    }
}

#[test]
fn pages_follow_on_without_losing_or_repeating_a_session_among_equal_usage() {
    let home = Home::new();
    // Five sessions of Claude Opus 5, the second to fourth with the same
    // usage.
    let sessions = [
        ("10000000-0000-4000-8000-000000000001", 3_000),
        ("10000000-0000-4000-8000-000000000002", 2_000),
        ("10000000-0000-4000-8000-000000000003", 2_000),
        ("10000000-0000-4000-8000-000000000004", 2_000),
        ("10000000-0000-4000-8000-000000000005", 1_000),
    ];
    for (index, (session, output)) in sessions.iter().enumerate() {
        write(
            &claude_code::session(home.path(), session),
            &[response(
                session,
                None,
                &format!("2026-09-14T12:0{index}:00.000Z"),
                &index.to_string(),
                "claude-opus-5",
                usage(0, *output),
            )],
        );
    }
    let engine = home.scanned();

    let largest = |after: Option<String>, limit: usize| {
        let page = engine
            .sessions(&SessionQuery {
                order: SessionOrder::Tokens,
                limit,
                after,
                ..every_session()
            })
            .unwrap();
        let keys: Vec<String> = page.items.iter().map(|row| row.key.to_string()).collect();
        (keys, page.next)
    };
    // Most tokens first, and of equal usage the last key first.
    let key = |index: usize| format!("claude-code:{}", sessions[index].0);
    let (whole, next) = largest(None, 10);
    assert_eq!(whole, [key(0), key(3), key(2), key(1), key(4)]);
    assert_eq!(next, None);

    // Two at a time, the first page ends among the equal usage.
    let mut paged = Vec::new();
    let mut pages = 0;
    let mut after = None;
    loop {
        let (keys, next) = largest(after, 2);
        pages += 1;
        paged.extend(keys);
        match next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!((paged, pages), (whole, 3));

    // A cursor no page gave is the asker's mistake.
    let astray = engine.sessions(&SessionQuery {
        after: Some("yesterday".to_owned()),
        ..every_session()
    });
    assert!(matches!(astray, Err(Error::Cursor(_))), "{astray:?}");
}

/// A response's usage: 10 tokens in and `output` out.
fn small(output: u64) -> Value {
    json!({"input_tokens": 10, "output_tokens": output})
}

/// A session whose Explore subagent `EXPLORE` starts a subagent of its own,
/// `NESTED`, as Claude Code records it: the nested one's description names
/// the subagent that started it. The session uses 11 tokens (10 in, 1 out),
/// the subagent 12 (10 in, 2 out) of Claude Opus 5, and the nested subagent
/// 13 (10 in, 3 out) of Claude Haiku 5.
fn three_deep(home: &Home) {
    two_deep(home);
    nested(home);
}

/// The session and its Explore subagent of [`three_deep`].
fn two_deep(home: &Home) {
    write(
        &claude_code::session(home.path(), PARENT),
        &[response(
            PARENT,
            None,
            "2026-09-14T12:00:10.000Z",
            "1",
            "claude-opus-5",
            small(1),
        )],
    );
    write(
        &subagents::log(home.path(), PARENT, EXPLORE),
        &[response(
            PARENT,
            Some(EXPLORE),
            "2026-09-14T12:01:10.000Z",
            "2",
            "claude-opus-5",
            small(2),
        )],
    );
    subagents::describe(
        home.path(),
        PARENT,
        EXPLORE,
        json!({"agentType": "Explore", "description": "Find the ledger's tests"}),
    );
}

/// The subagent the Explore subagent of [`three_deep`] starts.
fn nested(home: &Home) {
    write(
        &subagents::log(home.path(), PARENT, NESTED),
        &[response(
            PARENT,
            Some(NESTED),
            "2026-09-14T12:02:10.000Z",
            "3",
            "claude-haiku-5",
            small(3),
        )],
    );
    subagents::describe(
        home.path(),
        PARENT,
        NESTED,
        json!({"agentType": "Explore", "description": "Read the merge rules",
               "parentAgentId": EXPLORE, "spawnDepth": 2}),
    );
}

const NESTED: &str = "a5f0c2e19b7d4a3c8";

/// Each listed session's key, own tokens and tokens with its subagents',
/// with the number of subagents it ran itself.
fn tree_rows(
    engine: &turnscope_engine::Engine,
    question: SessionQuery,
) -> Vec<(String, u64, u64, u32)> {
    let mut rows: Vec<_> = engine
        .sessions(&question)
        .unwrap()
        .items
        .iter()
        .map(|row| {
            (
                row.key.to_string(),
                row.totals.tokens.total(),
                row.with_subagents.tokens.total(),
                row.subagents,
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn every_session_counts_its_subagents_however_deep_they_run() {
    let home = Home::new();
    three_deep(&home);
    let engine = home.scanned();

    let parent = format!("claude-code:{PARENT}");
    let explore = format!("claude-code:{EXPLORE}");
    let nested = format!("claude-code:{NESTED}");
    // The session: 11 + 12 + 13 = 36 with both below it. Its subagent:
    // 12 + 13 = 25 with the one it started. The nested one: its own 13.
    assert_eq!(
        tree_rows(
            &engine,
            SessionQuery {
                subagents: true,
                ..every_session()
            }
        ),
        [
            (parent.clone(), 11, 36, 1),
            (nested.clone(), 13, 13, 0),
            (explore.clone(), 12, 25, 1),
        ]
    );

    // Of Claude Haiku 5, only the nested subagent's 13 tokens: each session
    // above it is listed with those 13 beneath it, though none of its own.
    let haiku = Filter {
        models: vec![ModelKey::of("claude-haiku-5")],
        ..Filter::default()
    };
    assert_eq!(
        tree_rows(
            &engine,
            SessionQuery {
                subagents: true,
                filter: haiku.clone(),
                ..every_session()
            }
        ),
        [
            (parent.clone(), 0, 13, 1),
            (nested, 13, 13, 0),
            (explore.clone(), 0, 13, 1),
        ]
    );
    // And the session's own subagents, as a session's page lists them.
    let parent_key = engine
        .sessions(&every_session())
        .unwrap()
        .items
        .remove(0)
        .key;
    assert_eq!(
        tree_rows(
            &engine,
            SessionQuery {
                subagents_of: Some(parent_key),
                filter: haiku,
                ..every_session()
            }
        ),
        [(explore, 0, 13, 1)]
    );
}

#[test]
fn a_subagent_found_later_counts_in_every_session_above_it() {
    let home = Home::new();
    two_deep(&home);
    let engine = home.scanned();
    let parent = format!("claude-code:{PARENT}");
    let explore = format!("claude-code:{EXPLORE}");
    let nested_key = format!("claude-code:{NESTED}");
    let all = || SessionQuery {
        subagents: true,
        ..every_session()
    };
    // 11 + 12 = 23 for the session; the subagent has none below it yet.
    assert_eq!(
        tree_rows(&engine, all()),
        [(parent.clone(), 11, 23, 1), (explore.clone(), 12, 12, 0)]
    );

    // The nested subagent's 13 arrive as the cache catches up: 36 and 25.
    nested(&home);
    engine.scan().unwrap();
    let caught_up = tree_rows(&engine, all());
    assert_eq!(
        caught_up,
        [
            (parent, 11, 36, 1),
            (nested_key, 13, 13, 0),
            (explore, 12, 25, 1),
        ]
    );

    // And a cache built at once from all of it says the same.
    let data = tempfile::tempdir().unwrap();
    let fresh = turnscope_engine::Engine::open(data.path(), home.path()).unwrap();
    fresh.scan().unwrap();
    assert_eq!(tree_rows(&fresh, all()), caught_up);
}

#[test]
fn a_session_whose_usage_is_all_its_subagents_is_listed() {
    let home = Home::new();
    // The session itself holds only what Claude Code injected: no title, no
    // response of its own. Its subagent used 12 tokens.
    write(
        &claude_code::session(home.path(), PARENT),
        &[
            json!({"type": "system", "subtype": "informational", "sessionId": PARENT,
                 "timestamp": "2026-09-14T12:00:00.000Z", "cwd": "/work/ledger",
                 "content": "Context restored"}),
        ],
    );
    write(
        &subagents::log(home.path(), PARENT, EXPLORE),
        &[response(
            PARENT,
            Some(EXPLORE),
            "2026-09-14T12:01:10.000Z",
            "2",
            "claude-opus-5",
            small(2),
        )],
    );
    subagents::describe(
        home.path(),
        PARENT,
        EXPLORE,
        json!({"agentType": "Explore", "description": "Find the ledger's tests"}),
    );
    let engine = home.scanned();
    let listed = tree_rows(
        &engine,
        SessionQuery {
            empty: false,
            ..every_session()
        },
    );
    assert_eq!(listed, [(format!("claude-code:{PARENT}"), 0, 12, 1)]);
}

const OTHER: &str = "3c1d0a2e-other";

/// Each listed session's key, and when it or anything run within it was
/// last active.
fn activity(
    engine: &turnscope_engine::Engine,
    question: &SessionQuery,
) -> Vec<(String, Option<Instant>)> {
    engine
        .sessions(question)
        .unwrap()
        .items
        .into_iter()
        .map(|row| (row.key.to_string(), row.active))
        .collect()
}

/// Another session, answered once at `time`.
fn other(home: &Home, time: &str) {
    write(
        &claude_code::session(home.path(), OTHER),
        &[response(OTHER, None, time, "o1", "claude-opus-5", small(1))],
    );
}

#[test]
fn a_session_is_as_recent_as_the_latest_work_run_within_it() {
    let home = Home::new();
    // The session answers at 12:00:10, the other session at 12:00:40, and
    // the session's subagent works until 12:01:10: the session is the more
    // recent, though its own last word came first.
    two_deep(&home);
    other(&home, "2026-09-14T12:00:40.000Z");
    let engine = home.scanned();
    let parent = format!("claude-code:{PARENT}");
    let other = format!("claude-code:{OTHER}");
    assert_eq!(
        activity(&engine, &every_session()),
        [
            (parent.clone(), Some(at("2026-09-14T12:01:10.000Z"))),
            (other.clone(), Some(at("2026-09-14T12:00:40.000Z"))),
        ]
    );

    // A subagent of the subagent, read later, works until 12:02:10: the
    // session two levels above it is as recent.
    nested(&home);
    engine.scan().unwrap();
    assert_eq!(
        activity(&engine, &every_session())[0],
        (parent, Some(at("2026-09-14T12:02:10.000Z")))
    );
}

#[test]
fn a_session_runs_while_anything_within_it_does() {
    let home = Home::new();
    two_deep(&home);
    other(&home, "2026-09-14T12:00:40.000Z");
    let engine = home.scanned();
    // Since 12:01: only the session whose subagent worked at 12:01:10, which
    // is listed as its session's work rather than on its own.
    let running = SessionQuery {
        active_since: Some(at("2026-09-14T12:01:00.000Z")),
        ..every_session()
    };
    let keys: Vec<String> = activity(&engine, &running)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(keys, [format!("claude-code:{PARENT}")]);
}

#[test]
fn in_a_span_a_session_stands_at_its_latest_activity_within_it() {
    let home = Home::new();
    // The session answers at 9:00:10 and again at 11:00:10; the other at
    // 9:30:00.
    write(
        &claude_code::session(home.path(), PARENT),
        &[
            response(
                PARENT,
                None,
                "2026-09-14T09:00:10.000Z",
                "1",
                "claude-opus-5",
                small(1),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T11:00:10.000Z",
                "2",
                "claude-opus-5",
                small(1),
            ),
        ],
    );
    other(&home, "2026-09-14T09:30:00.000Z");
    let engine = home.scanned();
    let span = |from: &str, until: &str| SessionQuery {
        span: Span {
            from: Some(at(from)),
            until: Some(at(until)),
        },
        ..every_session()
    };
    let parent = format!("claude-code:{PARENT}");
    let other = format!("claude-code:{OTHER}");
    // From 8:00 to 10:00, the session went on after the span, so it stands
    // at the end of its last quarter hour in it, 9:00 to 9:15; the other's
    // latest activity is in the span, to the millisecond.
    let actives = activity(
        &engine,
        &span("2026-09-14T08:00:00.000Z", "2026-09-14T10:00:00.000Z"),
    );
    assert_eq!(
        actives,
        [
            (other, Some(at("2026-09-14T09:30:00.000Z"))),
            (parent.clone(), Some(at("2026-09-14T09:14:59.999Z"))),
        ]
    );
    // From 8:00 to 12:00, its latest activity of all is in the span.
    assert_eq!(
        activity(
            &engine,
            &span("2026-09-14T08:00:00.000Z", "2026-09-14T12:00:00.000Z")
        )[0],
        (parent, Some(at("2026-09-14T11:00:10.000Z")))
    );
}

#[test]
fn a_page_goes_on_only_in_the_order_it_began_in() {
    let home = Home::new();
    two_deep(&home);
    other(&home, "2026-09-14T12:00:40.000Z");
    let engine = home.scanned();
    let first = engine
        .sessions(&SessionQuery {
            limit: 1,
            ..every_session()
        })
        .unwrap();
    let after = first.next.clone().unwrap();
    // The same order goes on from it.
    let next = engine
        .sessions(&SessionQuery {
            limit: 1,
            after: Some(after.clone()),
            ..every_session()
        })
        .unwrap();
    assert_eq!(next.items.len(), 1);
    // Another would page from a value it doesn't order by, so it refuses.
    let largest = engine.sessions(&SessionQuery {
        order: SessionOrder::Tokens,
        limit: 1,
        after: Some(after),
        ..every_session()
    });
    assert!(matches!(largest, Err(Error::Cursor(_))), "{largest:?}");
}
