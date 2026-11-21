use harness_config::PromptId;

pub trait PromptSource: Send + Sync {
    fn system_template(&self) -> String;
}

pub struct ConfigPrompts;

impl PromptSource for ConfigPrompts {
    fn system_template(&self) -> String {
        harness_config::prompt(PromptId::ApiAgentSystem)
    }
}
