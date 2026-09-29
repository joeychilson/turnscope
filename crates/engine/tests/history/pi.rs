//! Pi's session `01a099f5`, begun at 08:50:10 on 13 September.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Where Pi keeps the session's log under `home`, among those begun in
/// /work.
pub fn session(home: &Path) -> PathBuf {
    home.join(".pi/agent/sessions/--work--/2026-09-13T08-50-10-240Z_01a099f5.jsonl")
}

/// The line that opens the session's log, saying it runs in `cwd`.
pub fn header(cwd: &str) -> Value {
    json!({"type": "session", "version": 3, "id": "01a099f5", "timestamp": "2026-09-13T08:50:10.240Z", "cwd": cwd})
}

/// The person's prompt, `u1`, which the session's responses answer.
pub fn prompt() -> Value {
    json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-13T08:50:20.000Z",
           "message": {"role": "user", "content": "Index the sessions"}})
}
