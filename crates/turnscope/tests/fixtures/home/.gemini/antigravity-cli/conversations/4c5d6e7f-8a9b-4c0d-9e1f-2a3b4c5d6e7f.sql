-- One conversation's database of Antigravity's, which tests/cli.rs builds as
-- a .db beside this file: the tables and columns its reader reads, and a
-- conversation in /work/notes on Sep 30 from 11:00 UTC. The blobs are
-- protobuf written out by hand: `0A 06 08 …` is field 1, a message of 6
-- bytes, whose own field 1 is a number.
CREATE TABLE steps (idx integer, step_type integer NOT NULL DEFAULT 0,
    status integer NOT NULL DEFAULT 0, metadata blob, PRIMARY KEY (idx));
CREATE TABLE gen_metadata (idx integer, data blob, PRIMARY KEY (idx));
CREATE TABLE trajectory_metadata_blob (id text DEFAULT 'main', data blob, PRIMARY KEY (id));

-- A step's metadata holds when it was made (#1, its seconds #1): the
-- request (type 14) at 11:00:00, a reply (15) at 11:00:04, its tool's output
-- (132) at 11:00:09, and the last reply at 11:00:15.
INSERT INTO steps VALUES (0, 14, 3, X'0A0608B0D7F3D506');
INSERT INTO steps VALUES (1, 15, 3, X'0A0608B4D7F3D506');
INSERT INTO steps VALUES (2, 132, 3, X'0A0608B9D7F3D506');
INSERT INTO steps VALUES (3, 15, 3, X'0A0608BFD7F3D506');

-- A model call's data holds the steps it names (#2) and, in #1, its usage
-- (#4) and its model (#19), gemini-3.8-flash. The usage: the model's number
-- (#1), input (#2), output (#3), cache reads (#5), the provider's number
-- (#6), the message's id (#7), reasoning (#9) and the reply (#10).
--
-- For steps 1 and 2: 1,800 in, and 240 out, 90 of them reasoning. Its
-- message is bot-ag-1.
INSERT INTO gen_metadata VALUES (0,
    X'120201020A2F221A08A60A10880E18F00130183A08626F742D61672D31485A5096019A011067656D696E692D332E382D666C617368');
-- For step 3: 400 in, 4,200 read from the cache, and 80 out, 20 of them
-- reasoning. Its message is bot-ag-2.
INSERT INTO gen_metadata VALUES (1,
    X'1201030A30221B08A60A109003185028E82030183A08626F742D61672D324814503C9A011067656D696E692D332E382D666C617368');

-- The folder it works in (#1.#1), file:///work/notes, and when it began
-- (#2), 10:59:55.
INSERT INTO trajectory_metadata_blob VALUES ('main',
    X'0A140A1266696C653A2F2F2F776F726B2F6E6F746573120608ABD7F3D506');
