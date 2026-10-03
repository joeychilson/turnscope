//! Models are named as the catalog names them, so the interface shows
//! "Claude Opus 5" in place of `claude-opus-5`.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod opencode;
    pub mod pi;
}

use serde_json::{Value, json};
use turnscope_engine::{Filter, ModelInfo, ModelKey, Span, UsageQuery, Zone};

use history::home::{Home, write};
use history::{claude_code, opencode, pi};

const SESSION: &str = "9e5f7084-d983-49d9-b587-0cf04a7b4b98";

/// A Pi response from `provider`'s `model`: `input` tokens in, 10 out.
fn pi_response(id: &str, provider: &str, model: &str, input: u64) -> Value {
    json!({"type": "message", "id": id, "parentId": "u1", "timestamp": "2026-09-13T08:51:00.000Z",
           "message": {"role": "assistant", "provider": provider, "model": model, "responseId": id,
                       "timestamp": 1_789_289_428_586u64,
                       "usage": {"input": input, "output": 10, "cacheRead": 0, "cacheWrite": 0,
                                 "totalTokens": input + 10, "cost": {"total": 0.001}}}})
}

#[test]
fn each_model_is_named_by_the_catalog_whichever_provider_served_it() {
    let home = Home::new();
    // Claude Code's claude-opus-5 from Anthropic.
    write(
        &claude_code::session(home.path(), SESSION),
        &[claude_code::response(
            SESSION,
            None,
            "2026-09-14T12:00:10.000Z",
            "1",
            "claude-opus-5",
            json!({"input_tokens": 32, "output_tokens": 16}),
        )],
    );
    // Pi's GLM-5.3 twice from OpenCode Go and once from Z.AI, one model;
    // then grok-4.6-build, which the catalog doesn't list, from xAI.
    write(
        &pi::session(home.path()),
        &[
            pi::header("/work"),
            pi::prompt(),
            pi_response("a1", "opencode-go", "glm-5.3", 3_000),
            pi_response("a2", "zai", "glm-5.3", 4_000),
            pi_response("a3", "opencode-go", "glm-5.3", 2_000),
            pi_response("a4", "xai", "grok-4.6-build", 500),
        ],
    );
    let engine = home.scanned();

    let model = |key: &str, name: Option<&str>| ModelInfo {
        key: ModelKey::of(key),
        name: name.map(str::to_owned),
    };
    assert_eq!(
        engine.models().unwrap(),
        vec![
            model("claude-opus-5", Some("Claude Opus 5")),
            model("glm-5.3", Some("GLM-5.3")),
            model("grok-4.6-build", None),
        ]
    );
}

#[test]
fn usage_outside_the_conversation_that_names_no_model_is_no_model() {
    let home = Home::new();
    // OpenCode's totals for the session count 500 input tokens, which name
    // no model, where its one message of GLM-5.3 shows 100: 400 outside the
    // conversation.
    let message = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
                         "time": {"created": 1_000},
                         "tokens": {"input": 100, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
    let database = opencode::database(&opencode::path(home.path()), "/work");
    database
        .execute_batch(
            "UPDATE session_v2 SET tokens_input = 500, tokens_output = 10 WHERE id = 'ses_a'",
        )
        .unwrap();
    opencode::message(&database, "msg_1", "assistant", 1_000, 1_000, &message);
    drop(database);
    let engine = home.scanned();

    let everything = UsageQuery {
        span: Span::default(),
        filter: Filter::default(),
        by: None,
        every: None,
    };
    let total = engine.usage(&everything, &Zone::system()).unwrap().total;
    assert_eq!(total.outside, 400);
    let keys: Vec<String> = engine
        .models()
        .unwrap()
        .iter()
        .map(|model| model.key.to_string())
        .collect();
    assert_eq!(keys, ["glm-5.3"]);
}
