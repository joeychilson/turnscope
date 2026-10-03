# Contract

What `turnscope watch` and the menu bar app say to each other, one JSON object
to a line. The Rust tests check that `turnscope` writes exactly these and reads
these; the app's tests read the same files.

- `feed.json`: everything the app shows (`crates/cli/src/feed.rs`), as the
  `feed` of a `{"feed": …}` line, written out over many lines to review.
- `out.jsonl`: the other lines `turnscope watch` writes: an alert, a recap and
  replies (`crates/cli/src/watch.rs`).
- `in.jsonl`: every request the app can make.

A change to either side changes these files, in the same commit as the other
side. `feed.json` and `out.jsonl` are written from the Rust side with
`TURNSCOPE_WRITE_CONTRACT=1 cargo test -p turnscope`; `in.jsonl` is edited by
hand. Review the diff.
