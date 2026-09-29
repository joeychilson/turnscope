//! The server knows who is asking from what the agent that started it
//! leaves it: its environment, its working folder, and its ancestors'
//! command lines, as each agent was measured to leave them.

use std::collections::HashMap;
use std::path::Path;

use turnscope_engine::Agent;
use turnscope_mcp::Caller;

/// Who `variables`, `working` and `ancestors` say is asking, with home
/// /Users/joey.
fn identify(variables: &[(&str, &str)], working: &str, ancestors: &[&str]) -> Caller {
    let variables: HashMap<&str, &str> = variables.iter().copied().collect();
    Caller::identify(
        |name| variables.get(name).map(|value| (*value).to_owned()),
        Some(Path::new(working)),
        Some(Path::new("/Users/joey")),
        ancestors,
    )
}

#[test]
fn claude_code_is_known_by_what_it_adds_to_a_servers_environment() {
    // As Claude Code 2.1.284 starts a stdio server: its own environment,
    // with CLAUDECODE, the project it works in and its session added.
    let caller = identify(
        &[
            ("CLAUDECODE", "1"),
            ("CLAUDE_PROJECT_DIR", "/Users/joey/work/atlas"),
            (
                "CLAUDE_CODE_SESSION_ID",
                "cbc15b75-e2b2-4ca8-9597-d1fc650219d9",
            ),
        ],
        "/Users/joey/work/atlas/src",
        &["/bin/zsh"],
    );
    assert_eq!(
        caller,
        Caller {
            agent: Some(Agent::ClaudeCode),
            folder: Some("/Users/joey/work/atlas".to_owned()),
            config: None,
            session: Some("cbc15b75-e2b2-4ca8-9597-d1fc650219d9".to_owned()),
        }
    );
    // Its AI_AGENT marker alone is enough; the folder is then where the
    // server runs.
    let marked = identify(
        &[("AI_AGENT", "claude-code_2-1-284_harness")],
        "/Users/joey/work",
        &[],
    );
    assert_eq!(
        (marked.agent, marked.folder.as_deref()),
        (Some(Agent::ClaudeCode), Some("/Users/joey/work"))
    );
}

#[test]
fn other_agents_are_known_by_the_process_that_started_the_server() {
    // Codex passes a server only a fixed list of variables, so it is known
    // by its process; a login shell or a runtime between them is passed.
    for (ancestors, agent) in [
        (&["claude --resume x"][..], Agent::ClaudeCode),
        (&["/Users/joey/.local/bin/codex"][..], Agent::Codex),
        (&["-/bin/zsh", "codex exec --yolo"][..], Agent::Codex),
        (&["opencode"][..], Agent::OpenCode),
        (
            &["/Users/joey/.grok/downloads/grok-1.0.41-macos-aarch64"][..],
            Agent::Grok,
        ),
        (&["node /usr/local/lib/node_modules/.bin/pi"][..], Agent::Pi),
    ] {
        let caller = identify(&[], "/Users/joey/work/atlas", ancestors);
        assert_eq!(caller.agent, Some(agent), "{ancestors:?}");
        assert_eq!(caller.folder.as_deref(), Some("/Users/joey/work/atlas"));
    }
    // Shells, runtimes and names that only start like an agent's, and a
    // folder that isn't absolute: neither is known.
    let nobody = identify(
        &[],
        "relative",
        &["-/bin/zsh", "claudette", "pip install", "node", "launchd"],
    );
    assert_eq!(nobody, Caller::default());
}

#[test]
fn a_sign_in_kept_outside_the_default_folder_is_noted() {
    // CODEX_HOME at its default is no different from none.
    let default = identify(
        &[("CODEX_HOME", "/Users/joey/.codex/")],
        "/Users/joey/work",
        &["codex"],
    );
    assert_eq!(default.config, None);
    let work = identify(
        &[("CODEX_HOME", "/Users/joey/.codex-work")],
        "/Users/joey/work",
        &["codex"],
    );
    assert_eq!(
        work.config,
        Some((
            "CODEX_HOME".to_owned(),
            "/Users/joey/.codex-work".to_owned()
        ))
    );
    let claude = identify(
        &[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CONFIG_DIR", "/Users/joey/.claude-work"),
        ],
        "/Users/joey/work",
        &[],
    );
    assert_eq!(
        claude.config,
        Some((
            "CLAUDE_CONFIG_DIR".to_owned(),
            "/Users/joey/.claude-work".to_owned()
        ))
    );
    // Another agent's variable says nothing of this one.
    let grok = identify(
        &[("CODEX_HOME", "/Users/joey/.codex-work")],
        "/Users/joey/work",
        &["grok"],
    );
    assert_eq!((grok.agent, grok.config), (Some(Agent::Grok), None));
}
