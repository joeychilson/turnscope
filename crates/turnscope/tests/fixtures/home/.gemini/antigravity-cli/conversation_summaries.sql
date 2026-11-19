-- The titles of Antigravity's conversations, which tests/cli.rs builds as a
-- .db beside this file: the columns its reader reads, and the title it gave
-- the one conversation here.
CREATE TABLE conversation_summaries (conversation_id text, title text NOT NULL DEFAULT '',
    step_count integer NOT NULL DEFAULT 0, last_modified_time datetime NOT NULL,
    parent_conversation_id text NOT NULL DEFAULT '', PRIMARY KEY (conversation_id));

INSERT INTO conversation_summaries VALUES ('4c5d6e7f-8a9b-4c0d-9e1f-2a3b4c5d6e7f',
    'Sorting the notes by date', 4, '2026-09-30 11:00:15.5+00:00', '');
