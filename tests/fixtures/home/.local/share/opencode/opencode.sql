-- OpenCode's database, which tests/cli.rs builds as opencode.db beside this
-- file: the tables and columns its reader reads, and one session in
-- /work/web on Sep 30 from 05:00 UTC, with no login kept.
CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, fork_session_id TEXT, directory TEXT,
    title TEXT, time_created INTEGER, time_updated INTEGER);
CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER,
    time_created INTEGER, time_updated INTEGER, data TEXT);
CREATE TABLE credential (integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER);

INSERT INTO session_v2 VALUES ('ses_2f9a8b7c6d5e4f3a2b1c0d9e8f', NULL, NULL, '/work/web',
    'Show signup errors inline', 1790744400000, 1790744580000);

INSERT INTO session_message VALUES ('msg_oc1', 'ses_2f9a8b7c6d5e4f3a2b1c0d9e8f', 'user', 1,
    1790744400000, 1790744400000,
    '{"text": "Show the signup form errors next to each field."}');
INSERT INTO session_message VALUES ('msg_oc2', 'ses_2f9a8b7c6d5e4f3a2b1c0d9e8f', 'assistant', 2,
    1790744410000, 1790744460000,
    '{"model": {"providerID": "opencode-go", "id": "glm-5.3"},
      "time": {"created": 1790744410000, "completed": 1790744460000},
      "tokens": {"input": 2000, "output": 300, "reasoning": 100, "cache": {"read": 5000, "write": 0}},
      "cost": 0.004,
      "content": [
        {"type": "reasoning", "text": "The errors render in one list above the form."},
        {"type": "text", "text": "I will render each error under its field."},
        {"type": "tool", "id": "call_oc1", "name": "edit",
         "state": {"status": "completed",
                   "input": {"filePath": "src/signup.tsx", "oldString": "<ErrorList />", "newString": "<FieldError name={name} />"},
                   "content": "Edit applied.", "metadata": {}}}]}');
INSERT INTO session_message VALUES ('msg_oc3', 'ses_2f9a8b7c6d5e4f3a2b1c0d9e8f', 'shell', 3,
    1790744490000, 1790744490000,
    '{"shellID": "sh_oc1", "command": "npm test",
      "output": {"output": "1 failing: signup lists errors above the form"}, "exit": 1}');
INSERT INTO session_message VALUES ('msg_oc4', 'ses_2f9a8b7c6d5e4f3a2b1c0d9e8f', 'assistant', 4,
    1790744520000, 1790744580000,
    '{"model": {"providerID": "opencode-go", "id": "glm-5.3"},
      "time": {"created": 1790744520000, "completed": 1790744580000},
      "tokens": {"input": 200, "output": 80, "reasoning": 0, "cache": {"read": 7000, "write": 0}},
      "cost": 0.001,
      "content": [{"type": "text", "text": "The failing test still expects the old list; it needs updating."}]}');
