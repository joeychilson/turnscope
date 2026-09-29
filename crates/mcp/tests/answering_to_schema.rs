//! Every tool's figures are what its output schema says they are, since a
//! client on a revision that takes structured content may check them
//! against it, and refuse an answer that doesn't match.

mod history {
    pub mod files;
    pub mod limits;
    pub mod server;
    pub mod sessions;
}

use serde_json::{Map, Value, json};

use history::limits::{BETA, claude_code};
use history::server::{answer, refusal, talk};
use history::sessions::{CLAUDE, EXPLORE};

/// Why `value` isn't what `schema` says, at `path`; `None` when it is. The
/// schemas use only `type`, `properties`, `required`, `items`, `enum` and
/// `anyOf`, and so does this.
fn mismatch(value: &Value, schema: &Value, path: &str) -> Option<String> {
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        return any
            .iter()
            .all(|schema| mismatch(value, schema, path).is_some())
            .then(|| format!("{path}: {value} is none of {schema}"));
    }
    if let Some(types) = schema.get("type") {
        let types: Vec<&str> = match types {
            Value::String(one) => vec![one],
            Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
            _ => return Some(format!("{path}: the schema's type {types} doesn't read")),
        };
        let is = |kind: &str| match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            _ => false,
        };
        if !types.iter().any(|kind| is(kind)) {
            return Some(format!("{path}: {value} is not {types:?}"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Some(format!("{path}: {value} is not one of {allowed:?}"));
    }
    if let Value::Object(fields) = value {
        let empty = Map::new();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !fields.contains_key(required) {
                return Some(format!("{path}: {required} is missing"));
            }
        }
        for (name, field) in fields {
            if let Some(schema) = properties.get(name)
                && let Some(why) = mismatch(field, schema, &format!("{path}.{name}"))
            {
                return Some(why);
            }
        }
    }
    if let (Value::Array(items), Some(schema)) = (value, schema.get("items")) {
        for (place, item) in items.iter().enumerate() {
            if let Some(why) = mismatch(item, schema, &format!("{path}[{place}]")) {
                return Some(why);
            }
        }
    }
    None
}

#[test]
fn the_validator_finds_what_doesnt_match() {
    let schema = json!({"type": "object", "required": ["a"], "properties": {
        "a": {"type": ["integer", "null"]},
        "b": {"type": "array", "items": {"type": "string", "enum": ["x"]}},
        "c": {"anyOf": [{"type": "string"}, {"type": "null"}]},
    }});
    assert_eq!(
        mismatch(&json!({"a": 1, "b": ["x"], "c": null}), &schema, "$"),
        None
    );
    for wrong in [
        json!({}),
        json!({"a": 1.5}),
        json!({"a": 1, "b": ["y"]}),
        json!({"a": 1, "c": 2}),
    ] {
        assert!(mismatch(&wrong, &schema, "$").is_some(), "{wrong}");
    }
}

#[test]
fn every_tools_figures_match_its_output_schema() {
    // Sessions from several agents, and limits with the sessions that used
    // them.
    let (sessions, _home, _data) = history::sessions::server();
    let (limits, _, _dirs) = history::limits::server(claude_code());
    let listed = talk(
        &sessions,
        "2025-06-18",
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})],
    );
    let schemas: Vec<(String, Value)> = listed[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| {
            Some((
                tool["name"].as_str()?.to_owned(),
                tool.get("outputSchema")?.clone(),
            ))
        })
        .collect();
    let calls = [
        (&limits, "check_limits", json!({})),
        (&limits, "check_limits", json!({"all": true, "below": 50})),
        (
            &limits,
            "check_limits",
            json!({"account": "other@example.com", "below": 50}),
        ),
        (&limits, "explain_limit", json!({})),
        (&limits, "explain_limit", json!({"by": "projects"})),
        (&limits, "explain_limit", json!({"by": "models"})),
        (
            &limits,
            "explain_limit",
            json!({"by": "agents", "limit": "week"}),
        ),
        (
            &limits,
            "explain_limit",
            json!({"session": format!("claude-code:{BETA}")}),
        ),
        (&sessions, "find_sessions", json!({})),
        (&limits, "find_sessions", json!({})),
        (&sessions, "find_sessions", json!({"words": "migration"})),
        (
            &sessions,
            "get_session",
            json!({"session": format!("claude-code:{CLAUDE}")}),
        ),
        (
            &sessions,
            "get_session",
            json!({"session": format!("claude-code:{EXPLORE}")}),
        ),
        (&limits, "get_session", json!({})),
        (&sessions, "get_usage", json!({})),
        (&sessions, "get_usage", json!({"by": "day"})),
        (&limits, "get_usage", json!({"by": "account"})),
        (&sessions, "get_usage", json!({"by": "session"})),
    ];
    for (server, tool, arguments) in calls {
        let answered = answer(server, tool, arguments.clone());
        let (_, schema) = schemas
            .iter()
            .find(|(name, _)| name == tool)
            .unwrap_or_else(|| panic!("{tool} has no output schema"));
        assert_eq!(
            mismatch(&answered.data, schema, "$"),
            None,
            "{tool} {arguments}: {}",
            answered.data
        );
        // Sentences lead, and the figures follow them in the text too.
        assert!(!answered.said().is_empty(), "{tool} {arguments}");
        assert!(answered.text.ends_with(&answered.data.to_string()));
    }
    // A refusal is sentences alone, with no figures to match.
    let refused = refusal(&sessions, "get_usage", json!({"by": "colour"}));
    assert!(!refused.contains('{'), "{refused}");
}
