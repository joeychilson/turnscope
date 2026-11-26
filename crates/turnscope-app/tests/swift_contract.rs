#![cfg(target_os = "macos")]

use std::path::Path;

use serde::Deserialize;
use turnscope::alerts::Notify;
use turnscope::status::Standing;
use turnscope_app::state::{Action, Effect, MenuShows};
use turnscope_app::view::*;
use turnscope_app::words::AlertNote;
use turnscope_app::{SnapshotConfig, StartConfig};

#[test]
fn swift_models_match_rust_wire_contract() {
    let full_row = AccountRowModel {
        id: "anthropic:a".into(),
        provider: "anthropic".into(),
        title: "Claude Max".into(),
        label: Some("me@example.com".into()),
        called: "Claude Max (me@example.com)".into(),
        standing: Standing::RunningOut,
        status: "Runs out in 50m".into(),
        figure: "50m".into(),
        figure_then: Some("· 12%".into()),
        figure_spoken: "50m left".into(),
        collapsed_spoken: "Claude Max, Runs out in 50m, 50m left".into(),
        openable: true,
        limits: vec![
            LimitLineModel {
                key: "five_hour".into(),
                name: "5 hours".into(),
                ahead: Some("Runs out in 50m".into()),
                window: Some("Resets 3:00 PM".into()),
                left: "12%".into(),
                size: None,
                spoken: "12% left".into(),
                left_percent: 12,
                time_left_fraction: Some(0.42),
                standing: Standing::RunningOut,
            },
            LimitLineModel {
                key: "credits".into(),
                name: "Credits".into(),
                ahead: None,
                window: None,
                left: "$0".into(),
                size: Some("of $30".into()),
                spoken: "$0 of $30 left".into(),
                left_percent: 0,
                time_left_fraction: None,
                standing: Standing::UsedUp,
            },
        ],
        note: Some("As of 2:00 PM".into()),
        used_most_heading: Some("Used most of 5 hours".into()),
        used_most: vec![UsedMostModel {
            id: "claude-code:0f6e-1234".into(),
            session_id: "0f6e-1234".into(),
            title: "Refactor bridge".into(),
            detail: "34% · turnscope".into(),
            project_path: Some("/tmp/turnscope".into()),
            exists: true,
        }],
    };

    let full_panel = PanelModel {
        version: "0.4.1".into(),
        stopped: false,
        can_refresh: true,
        waiting: Some(WaitingModel {
            reading: "Reading…".into(),
            problem: Some(WaitingProblem {
                headline: "Turnscope can't read its data".into(),
                detail: "database locked".into(),
                footnote: Some("It tries again on its own.".into()),
            }),
        }),
        verdict: Some(VerdictModel {
            headline: "Claude Max runs out in 50m".into(),
            detail: "Back 3:00 PM.".into(),
            standing: Standing::RunningOut,
        }),
        in_use: vec![full_row],
        recent: vec![],
        unused: vec![],
        hidden: vec![AccountModel {
            id: "openai:b".into(),
            provider: "openai".into(),
            title: "ChatGPT Pro".into(),
            label: Some("work@example.com".into()),
            via: Some("Codex".into()),
            called: "ChatGPT Pro".into(),
            shown: false,
        }],
        unused_fold_label: Some("1 hidden".into()),
        hidden_fold_label: Some("1 hidden".into()),
        connect_offer: Some(ConnectOfferModel {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            note: Some("Uses another Turnscope".into()),
            configured_help: Some("Configured to run /old/turnscope".into()),
            changing: false,
            verb: "Reconnect".into(),
            help: "Points Claude Code at this copy".into(),
        }),
        footer_note: Some(FooterNoteModel {
            text: "Not up to date · update Turnscope".into(),
            compact_text: Some("Not up to date".into()),
            help: Some("Offline".into()),
            spoken: "Not up to date: Offline".into(),
            busy: false,
        }),
        settings: SettingsModel {
            menu_shows: MenuShows::All,
            notify_options: vec![
                NotifyOptionModel {
                    kind: NotifyKind::RunningOut,
                    title: "A limit will run out".into(),
                    help: Some("Before it resets, at your pace".into()),
                    enabled: true,
                },
                NotifyOptionModel {
                    kind: NotifyKind::UsedUp,
                    title: "A limit is used up".into(),
                    help: None,
                    enabled: false,
                },
                NotifyOptionModel {
                    kind: NotifyKind::Reset,
                    title: "A limit is back".into(),
                    help: Some("After it was used up, once the account can be used again".into()),
                    enabled: true,
                },
                NotifyOptionModel {
                    kind: NotifyKind::SignIn,
                    title: "An account needs signing in".into(),
                    help: Some("Its login was refused, so its limits can't be read".into()),
                    enabled: false,
                },
            ],
            agents_empty: Some(EmptyMessage {
                headline: "No agents".into(),
                detail: "Install one".into(),
            }),
            agents_title: "Let these agents check their limits".into(),
            agents: vec![AgentRowModel {
                id: "claude-code".into(),
                name: "Claude Code".into(),
                note: Some("Uses another Turnscope".into()),
                configured_help: Some("Configured to run /old/turnscope".into()),
                changing: Some("Connecting…".into()),
                connected: true,
                enabled: false,
            }],
            subscriptions: vec![AccountModel {
                id: "anthropic:a".into(),
                provider: "anthropic".into(),
                title: "Claude Max".into(),
                label: Some("me@example.com".into()),
                via: Some("Claude Code".into()),
                called: "Claude Max (me@example.com)".into(),
                shown: true,
            }],
            api_keys: vec![],
        },
    };

    // Every optional field is set: Swift decodes one whose key it doesn't know as nil.
    let open_sync = AppSync {
        animated: true,
        counting: true,
        has_status: true,
        can_refresh: true,
        menu_bar: MenuBarModel {
            slots: vec![
                MenuBarSlot {
                    provider: "anthropic".into(),
                    standing: Standing::Lasts,
                    figure: "88%".into(),
                },
                MenuBarSlot {
                    provider: "openai".into(),
                    standing: Standing::UsedUp,
                    figure: "Back 4:00 PM".into(),
                },
            ],
            spoken: "Claude Max 88%".into(),
            out_of_date: true,
        },
        panel: Some(full_panel.clone()),
        effects: vec![
            Effect::PostAlerts {
                alerts: vec![AlertNote {
                    id: 7,
                    account: "anthropic:a".into(),
                    replaces: "turnscope-limit-anthropic:a".into(),
                    sign_in: false,
                    title: "Claude Max".into(),
                    subtitle: "5 hours".into(),
                    body: "Runs out in 50m".into(),
                }],
            },
            Effect::RemoveNotifications {
                identifiers: vec!["turnscope-sign-in-anthropic:a".into()],
            },
        ],
    };

    let closed_sync = AppSync {
        panel: None,
        effects: vec![],
        ..open_sync.clone()
    };

    let snapshots = serde_json::json!([
        { "ok": { "panel": full_panel } },
        { "err": { "error": "unreadable", "code": 2 } }
    ]);

    let dir = tempfile::tempdir().unwrap();
    let syncs_path = dir.path().join("syncs.json");
    let snaps_path = dir.path().join("snapshots.json");
    std::fs::write(
        &syncs_path,
        serde_json::to_string(&[open_sync, closed_sync]).unwrap(),
    )
    .unwrap();
    std::fs::write(&snaps_path, serde_json::to_string(&snapshots).unwrap()).unwrap();

    let driver = dir.path().join("main.swift");
    std::fs::write(
        &driver,
        r#"
import Foundation

struct Out: Encodable {
    var actions: [AppAction]
    var start: StartConfig
    var snapshot: SnapshotConfig
}

let args = CommandLine.arguments
let decoder = JSONDecoder()
let syncs = try decoder.decode([AppSync].self, from: Data(contentsOf: URL(fileURLWithPath: args[1])))
precondition(syncs.count == 2)
let sync = syncs[0]
precondition(sync.hasStatus && sync.canRefresh)
guard let p = sync.panel else { preconditionFailure("expected open panel") }
precondition(syncs[1].panel == nil)
precondition(p.waiting == WaitingModel(
    reading: "Reading…",
    problem: WaitingProblem(
        headline: "Turnscope can't read its data",
        detail: "database locked",
        footnote: "It tries again on its own."
    )
))
precondition(p.verdict?.headline == "Claude Max runs out in 50m")
let row = p.inUse[0]
precondition(row.label == "me@example.com")
precondition(row.figureThen == "· 12%")
precondition(row.note == "As of 2:00 PM")
precondition(row.usedMostHeading == "Used most of 5 hours")
precondition(row.limits[0].ahead == "Runs out in 50m")
precondition(row.limits[0].window == "Resets 3:00 PM")
precondition(row.limits[0].timeLeftFraction == 0.42)
precondition(row.limits[1].size == "of $30")
precondition(row.usedMost[0].sessionId == "0f6e-1234")
precondition(row.usedMost[0].projectPath == "/tmp/turnscope")
precondition(row.usedMost[0].exists == true)
precondition(p.hidden[0].label == "work@example.com")
precondition(p.hidden[0].via == "Codex")
precondition(p.unusedFoldLabel == "1 hidden")
precondition(p.hiddenFoldLabel == "1 hidden")
precondition(p.connectOffer?.note == "Uses another Turnscope")
precondition(p.connectOffer?.configuredHelp == "Configured to run /old/turnscope")
precondition(p.footerNote == FooterNoteModel(text: "Not up to date · update Turnscope", compactText: "Not up to date", help: "Offline", spoken: "Not up to date: Offline", busy: false))
precondition(p.settings.notifyOptions.count == 4)
precondition(p.settings.notifyOptions[0] == NotifyOptionModel(kind: .runningOut, title: "A limit will run out", help: "Before it resets, at your pace", enabled: true))
precondition(p.settings.notifyOptions[1].help == nil && !p.settings.notifyOptions[1].enabled)
precondition(p.settings.agentsEmpty == EmptyMessage(headline: "No agents", detail: "Install one"))
precondition(p.settings.agents[0].note == "Uses another Turnscope")
precondition(p.settings.agents[0].configuredHelp == "Configured to run /old/turnscope")
precondition(p.settings.agents[0].changing == "Connecting…")
precondition(p.settings.subscriptions[0].label == "me@example.com")
precondition(p.settings.subscriptions[0].via == "Claude Code")
precondition(sync.effects == [
    .postAlerts(alerts: [AlertNote(id: 7, account: "anthropic:a", replaces: "turnscope-limit-anthropic:a", signIn: false, title: "Claude Max", subtitle: "5 hours", body: "Runs out in 50m")]),
    .removeNotifications(identifiers: ["turnscope-sign-in-anthropic:a"]),
])

let snaps = try decoder.decode([SnapshotResponse].self, from: Data(contentsOf: URL(fileURLWithPath: args[2])))
precondition(snaps.count == 2)
if case let .ok(panel) = snaps[0] {
    precondition(panel.verdict?.headline == "Claude Max runs out in 50m")
} else {
    preconditionFailure("expected .ok snapshot")
}
if case let .err(message, code) = snaps[1] {
    precondition(message == "unreadable" && code == 2)
} else {
    preconditionFailure("expected .err snapshot")
}

// Launch argument parsing.
precondition(Launch.parse(["Turnscope"]) == Launch())
precondition(
    Launch.parse(["Turnscope", "--fixture", "f.json", "--snapshot", "out", "--data", "d", "--home", "h", "--open", "account,unused"])
        == Launch(fixture: "f.json", snapshot: "out", opening: Opening(account: true, unused: true, settings: nil), data: "d", home: "h")
)
precondition(Launch.parse(["Turnscope", "--fixture", "--open"]) == Launch(opening: Opening()))
precondition(Opening.parse("settings").settings == .general)
precondition(Opening.parse("agents").settings == .agents)
precondition(Opening.parse("accounts").settings == .accounts)

// Panel placement (under item, above Dock, screen-edge clamping, multi-display coordinates).
let laptop = CGRect(x: 0, y: 0, width: 1512, height: 949)
let under = panelFrame(midX: 1200, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(under == CGRect(x: 1200 - 170 - 28, y: 0, width: 396, height: 943))
let docked = CGRect(x: 0, y: 80, width: 1512, height: 869)
precondition(panelFrame(midX: 1200, underY: 949, visible: docked, panelWidth: 340, gutter: 28).height == 863)
let rightEdge = panelFrame(midX: 1500, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(rightEdge.minX + 28 + 340 == 1512 - 6)
let leftEdge = panelFrame(midX: 10, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(leftEdge.minX + 28 == 6)
let narrow = CGRect(x: 0, y: 0, width: 300, height: 949)
precondition(panelFrame(midX: 150, underY: 949, visible: narrow, panelWidth: 340, gutter: 28).minX + 28 == 6)
let secondary = CGRect(x: -1920, y: 982, width: 1920, height: 1055)
let multi = panelFrame(midX: -300, underY: 2037, visible: secondary, panelWidth: 340, gutter: 28)
precondition(multi == CGRect(x: -300 - 170 - 28, y: 982, width: 396, height: 1049))

let out = Out(
    actions: [
        .panelOpened,
        .panelClosed,
        .setHidden(id: "anthropic:a", hidden: true),
        .setConnected(id: "claude-code", connecting: true),
        .setNotify(notify: NotifySettings(runningOut: true, usedUp: false, reset: true, signIn: false)),
        .setMenuShows(shows: .all),
        .setMenuShows(shows: .one),
        .dismissConnect(id: "codex"),
        .acknowledgeAlert(id: 7, signInAccount: nil),
        .acknowledgeAlert(id: 8, signInAccount: "anthropic:a"),
        .refreshNow,
        .tick,
    ],
    start: StartConfig(
        fixture: "f.json",
        data: "/tmp/d",
        home: "/tmp/h",
        connectDisabledReason: "Move Turnscope to Applications to connect agents"
    ),
    snapshot: SnapshotConfig(fixture: "f.json", data: "/tmp/d", home: "/tmp/h")
)
let data = try JSONEncoder().encode(out)
print(String(data: data, encoding: .utf8)!)
"#,
    )
    .unwrap();

    let models =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/macos/Sources/Models.swift");
    let bin = dir.path().join("contract");
    let compile = std::process::Command::new("xcrun")
        .args(["swiftc", "-swift-version", "6", "-warnings-as-errors"])
        .arg(&models)
        .arg(&driver)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("xcrun swiftc should run on macOS");
    assert!(
        compile.status.success(),
        "swiftc failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = std::process::Command::new(&bin)
        .arg(&syncs_path)
        .arg(&snaps_path)
        .output()
        .expect("contract binary should run");
    assert!(
        run.status.success(),
        "Swift contract failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    #[derive(Deserialize)]
    struct SwiftOut {
        actions: Vec<Action>,
        start: StartConfig,
        snapshot: SnapshotConfig,
    }
    let parsed: SwiftOut = serde_json::from_slice(&run.stdout)
        .expect("Rust should decode every Action/Config emitted by Swift");
    assert_eq!(
        parsed.actions,
        vec![
            Action::PanelOpened {},
            Action::PanelClosed {},
            Action::SetHidden {
                id: "anthropic:a".into(),
                hidden: true,
            },
            Action::SetConnected {
                id: "claude-code".into(),
                connecting: true,
            },
            Action::SetNotify {
                notify: Notify {
                    running_out: true,
                    used_up: false,
                    reset: true,
                    sign_in: false,
                },
            },
            Action::SetMenuShows {
                shows: MenuShows::All,
            },
            Action::SetMenuShows {
                shows: MenuShows::One,
            },
            Action::DismissConnect { id: "codex".into() },
            Action::AcknowledgeAlert {
                id: 7,
                sign_in_account: None,
            },
            Action::AcknowledgeAlert {
                id: 8,
                sign_in_account: Some("anthropic:a".into()),
            },
            Action::RefreshNow {},
            Action::Tick {},
        ]
    );
    assert_eq!(parsed.start.fixture.as_deref(), Some(Path::new("f.json")));
    assert_eq!(parsed.start.data.as_deref(), Some(Path::new("/tmp/d")));
    assert_eq!(parsed.start.home.as_deref(), Some(Path::new("/tmp/h")));
    assert_eq!(
        parsed.start.connect_disabled_reason.as_deref(),
        Some("Move Turnscope to Applications to connect agents")
    );
    assert_eq!(
        parsed.snapshot.fixture.as_deref(),
        Some(Path::new("f.json"))
    );
    assert_eq!(parsed.snapshot.data.as_deref(), Some(Path::new("/tmp/d")));
    assert_eq!(parsed.snapshot.home.as_deref(), Some(Path::new("/tmp/h")));
}
