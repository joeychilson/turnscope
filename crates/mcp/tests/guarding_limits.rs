//! A hook guards a costly call with a limit: under the percent left, the
//! guard refuses it, and its hook exits 2 with why; otherwise, and whenever
//! how the limit stands isn't known, it lets it go on, exiting 0.

mod history {
    pub mod files;
    pub mod limits;
}

use turnscope_mcp::{Caller, Guard, guard};

use history::limits::{claude_code, server};

#[test]
fn a_limit_under_the_percent_left_refuses_the_call_with_why() {
    let (server, _, _dirs) = server(claude_code());
    // 20% of the 5 hours is left: under 50.
    let verdict = guard(&server, "5 hours", 50.0, None).unwrap();
    let Guard::Block(why) = &verdict else {
        panic!("{verdict:?}");
    };
    assert!(
        why.starts_with(
            "Claude Max \u{b7} joey@example.com, 5-hour limit: 20% left, under 50%. It resets "
        ),
        "{why}"
    );
    assert_eq!(verdict.exit_code(), 2);
    // 69% of the week is left: under 75, and not under 50.
    assert_eq!(guard(&server, "week", 75.0, None).unwrap().exit_code(), 2);
    let verdict = guard(&server, "week", 50.0, None).unwrap();
    assert_eq!(
        (verdict.clone(), verdict.exit_code()),
        (Guard::Pass(None), 0)
    );
}

#[test]
fn a_limit_whose_standing_isnt_known_lets_the_call_go_on_with_a_note() {
    let (server, _, _dirs) = server(claude_code());
    // Read an hour ago, at 70% left: nothing says it isn't under 50 now.
    let stale = guard(&server, "5 hours", 50.0, Some("other@example.com")).unwrap();
    assert_eq!(stale.exit_code(), 0);
    let Guard::Pass(Some(note)) = &stale else {
        panic!("{stale:?}");
    };
    assert!(
        note.contains("isn't known now, so the call goes on"),
        "{note}"
    );
    // A limit no account has, and an account no one has.
    for verdict in [
        guard(&server, "month", 50.0, None).unwrap(),
        guard(&server, "5 hours", 50.0, Some("gemini")).unwrap(),
    ] {
        assert!(matches!(&verdict, Guard::Pass(Some(_))), "{verdict:?}");
        assert_eq!(verdict.exit_code(), 0);
    }
    // A caller whose account can't be told.
    let (unknown, _, _dirs) = history::limits::server(Caller::default());
    let verdict = guard(&unknown, "5 hours", 50.0, None).unwrap();
    assert_eq!(
        verdict,
        Guard::Pass(Some(
            "Turnscope can't tell which agent started it, so which account is yours isn't known."
                .to_owned()
        ))
    );
    // A percent out of range is a mistake, not a verdict.
    assert!(guard(&unknown, "5 hours", 0.0, None).is_err());
    assert!(guard(&unknown, "5 hours", 150.0, None).is_err());
}
