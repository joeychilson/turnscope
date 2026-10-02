//! Usage adds up the same whether it is read from the rollup of quarter hours
//! or from each response, whatever quarter hours a span cuts, and into local
//! days as the clocks make them.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod subagents;
}

use serde_json::{Value, json};
use turnscope_engine::{
    Bucket, Dimension, Engine, Filter, Instant, ModelKey, SessionOrder, SessionQuery, Span,
    UsageQuery, Zone,
};

use history::claude_code::{self, response};
use history::home::{Home, write};
use history::subagents;

const PARENT: &str = "9e5f7084-d983-49d9-b587-0cf04a7b4b98";
const EXPLORE: &str = "ac9cd968c38c96ac1";
const OTHER: &str = "4b1d0c55-5e1f-4f3a-9a2e-7c0a1b2c3d4e";

/// A response's usage: `input` tokens in, `cache_read` read from the cache
/// and `output` out.
fn usage(input: u64, cache_read: u64, output: u64) -> Value {
    json!({"input_tokens": input, "cache_read_input_tokens": cache_read, "output_tokens": output})
}

fn at(text: &str) -> Instant {
    Instant::parse(text).unwrap()
}

/// An engine that has read `home`.
fn engine(home: &Home) -> Engine {
    let engine = home.open();
    let report = engine.scan().unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    engine
}

#[test]
fn the_rollup_answers_as_each_response_does() {
    let home = Home::new();
    let opus = "claude-opus-5";
    // Between 12:00 and 13:00 on 14 September: three responses in the first
    // quarter hour, one of a model the catalog doesn't price, and Claude
    // Code's own totals counting 1,000 more input than its transcripts show,
    // which is spread over the quarter hours as usage outside the
    // conversation; a subagent's two responses; and another session's two.
    write(
        &claude_code::session(home.path(), PARENT),
        &[
            response(
                PARENT,
                None,
                "2026-09-14T12:01:00.000Z",
                "p1",
                opus,
                usage(100, 1_000, 50),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T12:05:00.000Z",
                "p2",
                opus,
                usage(200, 2_000, 60),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T12:14:59.000Z",
                "p3",
                opus,
                usage(300, 3_000, 70),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T12:20:00.000Z",
                "p4",
                "claude-mystery-1",
                usage(400, 0, 80),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T12:40:00.000Z",
                "p5",
                opus,
                usage(500, 5_000, 90),
            ),
            // The transcripts' Claude Opus 5: 100 + 200 + 300 + 500 = 1,100
            // input, 11,000 read from the cache and 270 out.
            json!({"type": "cost-state", "sessionId": PARENT, "modelUsage": {opus: {
                "inputTokens": 2_100, "outputTokens": 270, "cacheReadInputTokens": 11_000,
                "cacheCreationInputTokens": 0, "webSearchRequests": 0, "costUSD": 1.0}}}),
        ],
    );
    write(
        &subagents::log(home.path(), PARENT, EXPLORE),
        &[
            response(
                PARENT,
                Some(EXPLORE),
                "2026-09-14T12:03:00.000Z",
                "e1",
                "claude-haiku-4-5",
                usage(10, 100, 5),
            ),
            response(
                PARENT,
                Some(EXPLORE),
                "2026-09-14T12:22:00.000Z",
                "e2",
                "claude-haiku-4-5",
                usage(20, 200, 6),
            ),
        ],
    );
    subagents::describe(
        home.path(),
        PARENT,
        EXPLORE,
        json!({"agentType": "Explore", "description": "Find the ledger's tests"}),
    );
    write(
        &claude_code::session(home.path(), OTHER),
        &[
            response(
                OTHER,
                None,
                "2026-09-14T12:07:00.000Z",
                "o1",
                opus,
                usage(50, 0, 7),
            ),
            response(
                OTHER,
                None,
                "2026-09-14T12:50:00.000Z",
                "o2",
                opus,
                usage(60, 0, 8),
            ),
        ],
    );
    let engine = engine(&home);
    let zone = Zone::named("America/New_York").unwrap();

    // The first span falls on quarter hours, so it is read from the rollup
    // alone; the second is a millisecond earlier at each end, with nothing
    // in either millisecond, so the quarter hours it cuts are read from each
    // response: 11:45 with nothing in it, and 12:45 with the response at
    // 12:50.
    let aligned = Span {
        from: Some(at("2026-09-14T12:00:00Z")),
        until: Some(at("2026-09-14T13:00:00Z")),
    };
    let off = Span {
        from: Some(at("2026-09-14T11:59:59.999Z")),
        until: Some(at("2026-09-14T12:59:59.999Z")),
    };
    let opus_only = Filter {
        models: vec![ModelKey::of(opus)],
        ..Filter::default()
    };
    for filter in [Filter::default(), opus_only] {
        for by in [
            None,
            Some(Dimension::Agent),
            Some(Dimension::Model),
            Some(Dimension::Project),
            Some(Dimension::Session),
        ] {
            for every in [None, Some(Bucket::Hour)] {
                let answer = |span: Span| {
                    let question = UsageQuery {
                        span,
                        filter: filter.clone(),
                        by,
                        every,
                    };
                    let table = engine.usage(&question, &zone).unwrap();
                    (table.rows, table.total)
                };
                assert_eq!(answer(aligned), answer(off), "{filter:?} {by:?} {every:?}");
            }
        }
        for subagents in [false, true] {
            let page = |span: Span| {
                let question = SessionQuery {
                    span,
                    filter: filter.clone(),
                    subagents,
                    order: SessionOrder::Tokens,
                    limit: 10,
                    ..SessionQuery::default()
                };
                engine.sessions(&question).unwrap()
            };
            assert_eq!(page(aligned), page(off), "{filter:?} {subagents}");
        }
    }

    // What the answers hold: nine responses, the 1,000 tokens outside the
    // conversation, and one response without a price.
    let everything = UsageQuery {
        span: aligned,
        filter: Filter::default(),
        by: None,
        every: None,
    };
    let total = engine.usage(&everything, &zone).unwrap().total;
    assert_eq!(
        (total.responses, total.outside, total.unpriced),
        (9, 1_000, 1)
    );
    assert!(total.approximate);

    // Spans that cut quarter hours hold just the responses within them. Each
    // is given as responses, then input, cache reads and output in the
    // conversation: the 1,000 input outside it lies at the starts of quarter
    // hours, and is left out by taking `outside` from the input.
    let within = |from: &str, until: Option<&str>| {
        let question = UsageQuery {
            span: Span {
                from: Some(at(from)),
                until: until.map(at),
            },
            filter: Filter::default(),
            by: None,
            every: None,
        };
        let total = engine.usage(&question, &zone).unwrap().total;
        (
            total.responses,
            total.tokens.input - total.outside,
            total.tokens.cache_read,
            total.tokens.output,
        )
    };
    // 12:04 to 12:42 cuts both ends: p2, p3 and o1 before 12:15, p4 and e2
    // in the whole quarter hour from 12:15, and p5 after 12:30. Input 200 +
    // 300 + 50 + 400 + 20 + 500 = 1,470; cache reads 2,000 + 3,000 + 0 + 0 +
    // 200 + 5,000 = 10,200; output 60 + 70 + 7 + 80 + 6 + 90 = 313.
    assert_eq!(
        within("2026-09-14T12:04:00Z", Some("2026-09-14T12:42:00Z")),
        (6, 1_470, 10_200, 313)
    );
    // 12:02 to 12:06, within one quarter hour: e1 at 12:03 and p2 at 12:05.
    // Input 10 + 200; cache reads 100 + 2,000; output 5 + 60.
    assert_eq!(
        within("2026-09-14T12:02:00Z", Some("2026-09-14T12:06:00Z")),
        (2, 210, 2_100, 65)
    );
    // 12:10 to 12:20 cuts two quarter hours with none whole between: p3 at
    // 12:14:59; p4 at 12:20 is its end, and left out.
    assert_eq!(
        within("2026-09-14T12:10:00Z", Some("2026-09-14T12:20:00Z")),
        (1, 300, 3_000, 70)
    );
    // From 12:21 on, cutting only its start: e2, p5 and o2. Input 20 + 500 +
    // 60; cache reads 200 + 5,000; output 6 + 90 + 8.
    assert_eq!(within("2026-09-14T12:21:00Z", None), (3, 580, 5_200, 104));
}

#[test]
fn a_local_day_holds_its_usage_across_a_change_of_the_clocks() {
    let home = Home::new();
    // New York's clocks went back at 2:00 on 1 November 2026, so that day
    // ran 25 hours, from 04:00 UTC to 05:00 UTC the next day. The responses
    // are at 23:30 on 31 October (EDT), 00:30 on 1 November (EDT), 23:30 on
    // 1 November (EST) and 00:30 on 2 November (EST), with 1, 2, 4 and 8
    // tokens out.
    let opus = "claude-opus-5";
    write(
        &claude_code::session(home.path(), PARENT),
        &[
            response(
                PARENT,
                None,
                "2026-11-01T03:30:00.000Z",
                "1",
                opus,
                usage(0, 0, 1),
            ),
            response(
                PARENT,
                None,
                "2026-11-01T04:30:00.000Z",
                "2",
                opus,
                usage(0, 0, 2),
            ),
            response(
                PARENT,
                None,
                "2026-11-02T04:30:00.000Z",
                "3",
                opus,
                usage(0, 0, 4),
            ),
            response(
                PARENT,
                None,
                "2026-11-02T05:30:00.000Z",
                "4",
                opus,
                usage(0, 0, 8),
            ),
        ],
    );
    let engine = engine(&home);
    let question = UsageQuery {
        span: Span {
            from: Some(at("2026-10-31T04:00:00Z")),
            until: Some(at("2026-11-03T05:00:00Z")),
        },
        filter: Filter::default(),
        by: None,
        every: Some(Bucket::Day),
    };
    let table = engine
        .usage(&question, &Zone::named("America/New_York").unwrap())
        .unwrap();
    let days = [
        at("2026-10-31T04:00:00Z"),
        at("2026-11-01T04:00:00Z"),
        at("2026-11-02T05:00:00Z"),
    ];
    let output: Vec<(Option<Instant>, u64)> = table
        .rows
        .iter()
        .map(|row| (row.start, row.totals.tokens.output))
        .collect();
    // 1 November holds its first hour and its last: 2 + 4.
    assert_eq!(
        output,
        [(Some(days[0]), 1), (Some(days[1]), 6), (Some(days[2]), 8)]
    );
}

#[test]
fn only_the_buckets_with_usage_are_given() {
    let home = Home::new();
    // Two responses 36 years apart: 322,000 hours or so.
    let opus = "claude-opus-5";
    write(
        &claude_code::session(home.path(), PARENT),
        &[
            response(
                PARENT,
                None,
                "1990-01-01T12:30:00.000Z",
                "1",
                opus,
                usage(0, 0, 1),
            ),
            response(
                PARENT,
                None,
                "2026-09-14T07:30:00.000Z",
                "2",
                opus,
                usage(0, 0, 2),
            ),
        ],
    );
    let engine = engine(&home);
    let zone = Zone::named("UTC").unwrap();
    let question = UsageQuery {
        span: Span::default(),
        filter: Filter::default(),
        by: None,
        every: Some(Bucket::Hour),
    };
    // Only the hours with usage, however far apart.
    let table = engine.usage(&question, &zone).unwrap();
    let output: Vec<(Option<Instant>, u64)> = table
        .rows
        .iter()
        .map(|row| (row.start, row.totals.tokens.output))
        .collect();
    assert_eq!(
        output,
        [
            (Some(at("1990-01-01T12:00:00Z")), 1),
            (Some(at("2026-09-14T07:00:00Z")), 2)
        ]
    );
}

#[test]
fn usage_before_1970_is_in_the_quarter_hour_it_was_made_in() {
    let home = Home::new();
    // One response a minute before 1970, at -60,000 ms: in the quarter hour
    // from 23:45, at -900,000 ms, not in the one from midnight.
    write(
        &claude_code::session(home.path(), PARENT),
        &[response(
            PARENT,
            None,
            "1969-12-31T23:59:00.000Z",
            "1",
            "claude-opus-5",
            usage(0, 0, 1),
        )],
    );
    let engine = engine(&home);
    let zone = Zone::named("UTC").unwrap();
    let output = |from: &str, until: &str| {
        let question = UsageQuery {
            span: Span {
                from: Some(at(from)),
                until: Some(at(until)),
            },
            filter: Filter::default(),
            by: None,
            every: None,
        };
        engine.usage(&question, &zone).unwrap().total.tokens.output
    };
    // On quarter hours, read from the rollup; a millisecond off at each
    // end, the quarter hours cut are read from each response.
    assert_eq!(output("1969-12-31T23:45:00Z", "1970-01-01T00:00:00Z"), 1);
    assert_eq!(
        output("1969-12-31T23:44:59.999Z", "1969-12-31T23:59:59.999Z"),
        1
    );
    assert_eq!(output("1970-01-01T00:00:00Z", "1970-01-01T00:15:00Z"), 0);
}
