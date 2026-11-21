//! Run journal (spec `docs/specs/harness/resumable-runs.md` §3): `runs` / `run_steps` written through the
//! store connection, fed by [`crate::resume::track`] and read when the UI asks about a resumable run.

use harness_core_api::{AgentId, SessionId, ToolCallStatus, ToolCallView, TurnId};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::HubError;

/// Longest text kept per journal field.
const TEXT_CAP: usize = 4000;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs(
  turn_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent TEXT NOT NULL, user_text TEXT NOT NULL,
  state TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER);
CREATE INDEX IF NOT EXISTS runs_session ON runs(session_id, state);
CREATE TABLE IF NOT EXISTS run_steps(
  turn_id TEXT NOT NULL, idx INTEGER NOT NULL, state TEXT NOT NULL, tool_id TEXT, tool_title TEXT,
  tool_input TEXT, tool_output TEXT, at INTEGER NOT NULL, PRIMARY KEY(turn_id, idx));
";

/// Newest resumable (or resumed) run of a session, as offered to the UI (API 0.23.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOffer {
    pub turn_id: TurnId,
    pub agent: AgentId,
    pub user_text: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub steps: u32,
    pub last_step: Option<String>,
    pub resumed: bool,
}

/// One journal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub idx: u32,
    pub state: String,
    pub tool_id: Option<String>,
    pub tool_title: Option<String>,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
}

impl StepRow {
    /// One line of the resume prompt block.
    #[must_use]
    pub fn summary(&self) -> String {
        let title = self.tool_title.clone().unwrap_or_else(|| "step".to_owned());
        let output = self
            .tool_output
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| "-".to_owned(), |t| clip(t, 300));
        format!("{}. {title} -> {output}", self.idx)
    }
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

fn cap(text: Option<&str>) -> Option<String> {
    text.map(|t| clip(t, TEXT_CAP))
}

pub(crate) fn migrate(c: &Connection) -> Result<(), HubError> {
    c.execute_batch(SCHEMA)?;
    Ok(())
}

/// Runs the previous hub left unfinished become `resumable`; their turn keeps the `interrupted` status.
/// A run without a finished tool call has nothing to continue from, so it is closed instead of offered.
pub(crate) fn mark_orphans(c: &Connection) -> Result<(), HubError> {
    c.execute(
        "UPDATE runs SET state='finished', finished_at=COALESCE(finished_at, started_at)
         WHERE state IN ('running','resumable','resumed') AND EXISTS(
           SELECT 1 FROM runs n WHERE n.session_id=runs.session_id
             AND (n.started_at>runs.started_at
               OR (n.started_at=runs.started_at AND n.turn_id>runs.turn_id)))",
        [],
    )?;
    c.execute(
        "UPDATE runs SET state='finished', finished_at=COALESCE(finished_at, started_at)
         WHERE state='running' AND NOT EXISTS(
           SELECT 1 FROM run_steps s WHERE s.turn_id=runs.turn_id AND s.state='tool_result')",
        [],
    )?;
    c.execute(
        "UPDATE runs SET state='resumable', finished_at=COALESCE(finished_at, started_at)
         WHERE state='running'",
        [],
    )?;
    Ok(())
}

/// Starts the run of `turn` (a retry of the same turn drops its old steps).
pub(crate) fn begin(
    c: &Connection,
    session: &SessionId,
    turn: &TurnId,
    agent: &AgentId,
    user_text: &str,
    at: i64,
) -> Result<(), HubError> {
    c.execute(
        "DELETE FROM run_steps WHERE turn_id=?1",
        params![turn.as_str()],
    )?;
    c.execute(
        "INSERT INTO runs(turn_id,session_id,agent,user_text,state,started_at)
         VALUES(?1,?2,?3,?4,'running',?5)
         ON CONFLICT(turn_id) DO UPDATE SET state='running', started_at=?5, finished_at=NULL,
           agent=?3, user_text=?4",
        params![
            turn.as_str(),
            session.as_str(),
            agent.as_str(),
            user_text,
            at
        ],
    )?;
    c.execute(
        "UPDATE runs SET state='finished', finished_at=COALESCE(finished_at, ?3)
         WHERE session_id=?1 AND turn_id<>?2 AND state IN ('running','resumable','resumed')",
        params![session.as_str(), turn.as_str(), at],
    )?;
    Ok(())
}

/// Appends one step; `tool_result` is what makes a run resumable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn step(
    c: &Connection,
    turn: &TurnId,
    state: &str,
    tool_id: Option<&str>,
    title: Option<&str>,
    input: Option<&str>,
    output: Option<&str>,
    at: i64,
) -> Result<(), HubError> {
    let idx: i64 = c.query_row(
        "SELECT COALESCE(MAX(idx),0)+1 FROM run_steps WHERE turn_id=?1",
        params![turn.as_str()],
        |r| r.get(0),
    )?;
    c.execute(
        "INSERT INTO run_steps(turn_id,idx,state,tool_id,tool_title,tool_input,tool_output,at)
         VALUES(?1,?2,?3,?4,
           COALESCE(NULLIF(?5,''),(SELECT p.tool_title FROM run_steps p WHERE p.turn_id=?1
             AND p.tool_id=?4 AND COALESCE(p.tool_title,'')<>'' ORDER BY p.idx DESC LIMIT 1)),
           COALESCE(NULLIF(?6,''),(SELECT p.tool_input FROM run_steps p WHERE p.turn_id=?1
             AND p.tool_id=?4 AND COALESCE(p.tool_input,'')<>'' ORDER BY p.idx DESC LIMIT 1)),
           COALESCE(NULLIF(?7,''),(SELECT p.tool_output FROM run_steps p WHERE p.turn_id=?1
             AND p.tool_id=?4 AND COALESCE(p.tool_output,'')<>'' ORDER BY p.idx DESC LIMIT 1)),
           ?8)",
        params![
            turn.as_str(),
            idx,
            state,
            tool_id,
            title,
            cap(input),
            cap(output),
            at
        ],
    )?;
    Ok(())
}

/// Terminal turn: nothing left to resume.
pub(crate) fn finish(c: &Connection, turn: &TurnId, at: i64) -> Result<(), HubError> {
    c.execute(
        "UPDATE runs SET state='finished', finished_at=?2 WHERE turn_id=?1 AND state='running'",
        params![turn.as_str(), at],
    )?;
    Ok(())
}

/// The user dismissed the offer.
pub(crate) fn discard(c: &Connection, session: &SessionId) -> Result<(), HubError> {
    c.execute(
        "UPDATE runs SET state='discarded' WHERE session_id=?1 AND state='resumable'",
        params![session.as_str()],
    )?;
    Ok(())
}

/// The run is being resumed: offered again (as `resumed`) so a client that reconnects still sees it.
pub(crate) fn mark_resumed(c: &Connection, turn: &TurnId, at: i64) -> Result<(), HubError> {
    c.execute(
        "UPDATE runs SET state='resumed', finished_at=COALESCE(finished_at,?2)
         WHERE turn_id=?1 AND state='resumable'",
        params![turn.as_str(), at],
    )?;
    Ok(())
}

/// The offer for a session: `resumable` first, else the `resumed` run the UI may still be showing.
pub(crate) fn offer(c: &Connection, session: &SessionId) -> Result<Option<RunOffer>, HubError> {
    let row = c
        .query_row(
            "SELECT r.turn_id, r.agent, r.user_text, r.started_at, r.finished_at, r.state,
                    (SELECT COUNT(*) FROM run_steps s WHERE s.turn_id=r.turn_id
                       AND s.state='tool_result'),
                    (SELECT s.tool_title FROM run_steps s WHERE s.turn_id=r.turn_id
                       ORDER BY s.idx DESC LIMIT 1)
             FROM runs r WHERE r.session_id=?1 AND r.state IN ('resumable','resumed')
             ORDER BY (r.state='resumable') DESC, r.started_at DESC LIMIT 1",
            params![session.as_str()],
            |r| {
                Ok(RunOffer {
                    turn_id: TurnId::new(r.get::<_, String>(0)?),
                    agent: AgentId::new(r.get::<_, String>(1)?),
                    user_text: r.get(2)?,
                    started_at: r.get(3)?,
                    finished_at: r.get(4)?,
                    steps: r.get(6)?,
                    last_step: r.get(7)?,
                    resumed: r.get::<_, String>(5)? == "resumed",
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// User message of `turn`, from the transcript (the actor persists the row before `TurnStarted`).
pub(crate) fn turn_user_text(c: &Connection, turn: &TurnId) -> Result<Option<String>, HubError> {
    Ok(c.query_row(
        "SELECT user_text FROM turns WHERE id=?1",
        params![turn.as_str()],
        |r| r.get::<_, String>(0),
    )
    .optional()?)
}

/// Steps of a run, in journal order.
pub(crate) fn steps(c: &Connection, turn: &TurnId) -> Result<Vec<StepRow>, HubError> {
    let mut statement = c.prepare(
        "SELECT idx,state,tool_id,tool_title,tool_input,tool_output FROM run_steps
         WHERE turn_id=?1 ORDER BY idx",
    )?;
    let rows = statement
        .query_map(params![turn.as_str()], |r| {
            Ok(StepRow {
                idx: r.get(0)?,
                state: r.get(1)?,
                tool_id: r.get(2)?,
                tool_title: r.get(3)?,
                tool_input: r.get(4)?,
                tool_output: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Journal step state for one tool status.
#[must_use]
pub(crate) fn step_state(status: Option<ToolCallStatus>) -> &'static str {
    match status {
        Some(ToolCallStatus::Completed | ToolCallStatus::Failed) => "tool_result",
        Some(ToolCallStatus::Pending | ToolCallStatus::InProgress) | None => "tool_call",
    }
}

/// Tool text of a call (input and result), already clipped by [`step`].
#[must_use]
pub(crate) fn tool_texts(tool: &ToolCallView) -> (Option<&str>, Option<&str>) {
    let input = tool.raw_input.as_deref();
    let output = tool
        .raw_output
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .or_else(|| {
            tool.content.iter().find_map(|item| match item {
                harness_core_api::ToolContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
        });
    (input, output)
}

#[cfg(test)]
mod tests;
