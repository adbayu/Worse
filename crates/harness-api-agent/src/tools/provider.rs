use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;

use super::{normalize, resolve, str_arg};

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

pub struct ToolEnv<'a> {
    pub http: &'a reqwest::Client,
    pub cwd: &'a Path,
}

pub trait ToolProvider: Sync {
    fn definitions(&self) -> Vec<Value>;
    fn handles(&self, name: &str) -> bool;
    fn run<'a>(&'a self, env: ToolEnv<'a>, name: &'a str, args: &'a Value) -> ToolFuture<'a>;
}

struct WebTools;

impl ToolProvider for WebTools {
    fn definitions(&self) -> Vec<Value> {
        crate::web::definitions()
    }

    fn handles(&self, name: &str) -> bool {
        crate::web::handles(name)
    }

    fn run<'a>(&'a self, env: ToolEnv<'a>, name: &'a str, args: &'a Value) -> ToolFuture<'a> {
        Box::pin(crate::web::run(env.http, name, args))
    }
}

struct FactCheckTools;

impl ToolProvider for FactCheckTools {
    fn definitions(&self) -> Vec<Value> {
        vec![crate::factcheck::definition()]
    }

    fn handles(&self, name: &str) -> bool {
        crate::factcheck::handles(name)
    }

    fn run<'a>(&'a self, env: ToolEnv<'a>, _name: &'a str, args: &'a Value) -> ToolFuture<'a> {
        let root = crate::factcheck::writes(args)
            .then(|| resolve(env.cwd, str_arg(args, "dir").unwrap_or_default()))
            .filter(|p| p.starts_with(normalize(env.cwd)));
        let result = crate::factcheck::run_at(args, root.as_deref());
        Box::pin(std::future::ready(result))
    }
}

static PROVIDERS: [&dyn ToolProvider; 2] = [&WebTools, &FactCheckTools];

#[must_use]
pub fn providers() -> &'static [&'static dyn ToolProvider] {
    &PROVIDERS
}

#[must_use]
pub fn provider_for(name: &str) -> Option<&'static dyn ToolProvider> {
    providers().iter().copied().find(|p| p.handles(name))
}
