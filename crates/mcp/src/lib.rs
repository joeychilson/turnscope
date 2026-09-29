//! Turnscope's answers for coding agents, over the Model Context Protocol.
//!
//! The app is for the person, about their limits and usage; the server is
//! for the agents they work with. Agents pace themselves against the
//! person's limits, explain what used them, and pick up each other's work,
//! with six tools, each a question: `check_limits`, `explain_limit`,
//! `find_sessions`, `get_session`, `read_session` and `get_usage`; and
//! three prompts that brief an agent to use them: `catch-up`, `pace` and
//! `what-used`.
//!
//! An agent starts `turnscope mcp` and speaks JSON-RPC to it, a message per
//! line on standard input and output, until it closes its input. The server
//! knows who is asking ([`Caller`]), so "my limit" and "this session" need
//! no ids. The same tools run once from a shell as `turnscope call <tool>
//! '<arguments>'`, and [`guard()`] turns a limit into a verdict a hook exits
//! with. Every tool only reads.
//!
//! The server answers from the data directory the app keeps. While the app
//! runs, what it reads is kept current; when nothing keeps it, the server
//! reads what changed before it answers.

mod accounts;
mod caller;
mod explain;
mod guard;
mod handoff;
mod limits;
mod prompts;
mod prose;
mod protocol;
mod sessions;
mod time;
mod tools;
mod usage;

pub use crate::caller::Caller;
pub use crate::guard::{Guard, guard};
pub use crate::protocol::serve;
pub use crate::tools::{Failure, Reply, Server};

/// The names of the tools, in the order they are listed.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    tools::Tool::ALL.into_iter().map(tools::Tool::name)
}
