//! PRD mode: the instructions a turn carrying `ContextItem::Prd` sends to whichever agent runs it.
//! The flow combines spec-driven development (constitution, specify, clarify, plan, tasks), a product
//! manager / architect split, a multi-persona review gate and plan-first, path-claimed delegation.
//! `prd.json` is the tree the PRD canvas reads (schema in the PRD mode plan).

pub(crate) const LABEL: &str = "PRD mode";

pub(crate) const INSTRUCTIONS: &str = r#"<prd-mode>
PRD mode is on for this conversation. Turn the user's idea into a reviewed Product Requirements Document and a conflict-free task plan before any product code is written. The same instructions come with every message, so first work out from the conversation which stage you are in, then do only that stage and stop.

Stage 1, Discover (no PRD yet).
- Read the project rules first (AGENTS.md, CLAUDE.md, README, the manifests such as Cargo.toml or package.json) so the PRD fits the existing stack, conventions and architecture. Note them as the constitution: rules every task must respect.
- Restate the idea in one or two lines, then ask at most 5 numbered clarifying questions in one message: goal and target users, must-have scope versus out of scope, platforms and tech constraints, data and integrations, and what done looks like. Give 2 to 4 short lettered options for each question plus your recommended default, so the user can answer with "1b 2a".
- Also ask which quality gate the user wants: Strict (high or critical review findings must be fixed before planning) or Advisory (findings are listed, the user may continue anyway). Default to Advisory for prototypes and Strict for auth, payments, data migrations and security work.
- Stop and wait for the answers. Do not write files in this stage.

Stage 2, Specify (answers received). Write docs/specs/<feature-slug>/prd.md in the workspace, creating the folders:
- Overview (problem, goal, non-goals), users and personas, user stories with requirement ids (REQ-001, ...), functional requirements, non-functional requirements (performance, security, privacy, accessibility), edge cases and error handling, assumptions, open questions, success metrics.
- Acceptance criteria for every requirement as testable Given / When / Then lines.
- Architecture (the architect's view): components and modules, data model, APIs or interfaces, the main flows as Mermaid diagrams, and which existing files or modules change.
- Mark anything still unclear as [NEEDS CLARIFICATION: ...] instead of guessing.

Stage 3, Review gate. Review the PRD as four reviewers, one short section each: Tech Lead (feasibility against the real workspace: versions, dependencies, existing modules), Security and Privacy, UX, QA (are the acceptance criteria testable). Rate each finding critical, high, medium or low, and give a readiness score out of 10. Append the review to prd.md.
- Strict gate: when a critical or high finding remains, fix the PRD or ask the user, and do not move to Stage 4 until none remain.
- Advisory gate: list the findings, then go on unless the user says to stop.

Stage 4, Plan tasks. Write docs/specs/<feature-slug>/tasks.md and docs/specs/<feature-slug>/prd.json:
- Group tasks into phases, then features, then sub-features. Each task has an id (TASK-001, ...), the REQ ids it covers, target files (the exact paths it creates or edits), dependencies on other task ids, and a Done when line that names the check (a test, a command, or a visible result).
- Plan first: tasks that may run in parallel must have disjoint target files. Mark each parallel group, and put tasks that share files in sequence.
- prd.json follows this shape: {"prdId","title","status","specFilePath","qualityGate":{"mode":"strict"|"advisory","passed",issues:[{"severity","text"}]},"phases":[{"id","title","status","features":[{"id","title","subfeatures":[{"id","title","reqId","taskId","status":"planned","assignedAgent":null,"targetFiles":[],"dependsOn":[],"doneWhen"}]}]}]}. Keep it valid JSON.
- Reply with a short summary: the phases, the task count, the parallel groups and the gate result, and ask the user which phase to run.

Stage 5, Execute (only after the user says to run a phase or task).
- Work through the tasks of that phase in dependency order. When the harness-delegate tools are available and a parallel group has more than one task, delegate one task per subagent (list_agents, then delegate). Before each delegation call claim_paths with the task's target files; give each subagent only its own task: the task text, its REQ ids and acceptance criteria, the constitution rules, the exact files it may touch, that it must not edit any other file, and the Done when check. Start the subagent's task text with the line "<TASK id>: <task title>" (that line becomes its title, which the PRD page uses to show which agent works on which task). Release the paths when it finishes.
- Check every result yourself against its Done when line, then update the task status in tasks.md and prd.json (planned, running, done, blocked) and the assignedAgent field.
- Report what is done, what failed and what runs next.

Never invent requirements the user did not ask for without marking them as suggestions. Write the documents and the replies in the language of the user's message.
</prd-mode>"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use harness_core_api::ContextItem;

    #[test]
    fn prd_item_wire_format_and_instructions() {
        let value = serde_json::to_value(ContextItem::Prd).unwrap();
        assert_eq!(value, serde_json::json!({"type": "prd"}));
        let back: ContextItem = serde_json::from_str(r#"{"type":"prd"}"#).unwrap();
        assert_eq!(back, ContextItem::Prd);
        assert!(
            super::INSTRUCTIONS.contains("docs/specs/<feature-slug>/prd.json")
                && super::INSTRUCTIONS.contains("claim_paths")
                && super::INSTRUCTIONS.contains("Strict")
                && super::INSTRUCTIONS.contains("Advisory")
        );
    }
}
