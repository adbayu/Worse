//! Configuration data of Harness kept out of the code: built-in agent specs (`assets/agents.toml`),
//! numeric limits ([`Limits`]) and prompt templates (`assets/prompts/*.md`). Every value is embedded
//! at compile time; prompts can be overridden per user under `<HARNESS_HOME>/prompts/`.

mod agents;
mod limits;
mod paths;
mod prompts;

pub use agents::{builtin_agents, BuiltinAgent, Dialect};
pub use limits::Limits;
pub use paths::{harness_home, user_home, HOME_DIR_NAME, HOME_ENV};
pub use prompts::{prompt, render, PromptId, PROMPTS_DIR};
