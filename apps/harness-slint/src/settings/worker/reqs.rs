use super::{
    api, AgentKind, BTreeMap, Concept, ConfigTarget, EntryBody, PathBuf, Scope, Value, WritePlan,
};

/// A write the user must confirm.
pub enum PlanOp {
    Upsert {
        target: ConfigTarget,
        body: Box<EntryBody>,
        previous: Option<String>,
    },
    Delete {
        concept: Concept,
        target: ConfigTarget,
        name: String,
    },
    Toggle {
        concept: Concept,
        target: ConfigTarget,
        name: String,
        on: bool,
    },
    Copy {
        concept: Concept,
        from: ConfigTarget,
        to: ConfigTarget,
        name: String,
    },
    Raw {
        path: PathBuf,
        text: String,
    },
    Instructions {
        target: ConfigTarget,
        path: PathBuf,
        text: String,
    },
    Memory {
        scope: Scope,
        text: String,
    },
    ImportMcp(Vec<AgentKind>),
    /// Installs a catalog skill at user scope for every agent that supports it (spec skill-marketplace.md §2).
    InstallCatalog {
        name: String,
        skill: Box<harness_agent_config::Skill>,
        /// `(relative path, content)` of the skill's extra files.
        extra: Vec<(String, String)>,
    },
    /// Removes an installed skill from every agent that has it.
    RemoveCatalog {
        name: String,
    },
}

pub enum DocKind {
    Instructions(AgentKind),
    Memory,
}

pub enum Req {
    Patch {
        path: String,
        value: Value,
    },
    Shortcuts(BTreeMap<String, Vec<String>>),
    Relocate(PathBuf),
    CancelRelocation,
    List {
        seq: u64,
        concept: Concept,
        target: ConfigTarget,
    },
    ReadRaw {
        seq: u64,
        path: PathBuf,
    },
    Plan {
        title: &'static str,
        op: PlanOp,
    },
    Confirm,
    Cancel,
    Doc {
        seq: u64,
        kind: DocKind,
        scope: Scope,
    },
    Agents(Vec<String>),
    /// Projects page list (`removed` lists the restore candidates).
    Projects {
        seq: u64,
        query: String,
    },
    RemoveProjects {
        keys: Vec<String>,
        delete_data: bool,
    },
    RestoreProjects(Vec<String>),
    /// Archive page; `offset > 0` appends the next page.
    Archived {
        seq: u64,
        query: String,
        offset: u32,
    },
    Unarchive(String),
    DeleteArchived(String),
    /// Names of the installed skills at user scope, per agent id (skill marketplace).
    SkillsInstalled {
        seq: u64,
    },
    /// Skill trigger health report over the installed skills (spec `skill-health.md`).
    SkillsHealth {
        seq: u64,
    },
}

pub(super) enum Pending {
    Plan(WritePlan),
    Import(WritePlan, String),
    Memory(api::MemoryPlan),
}
