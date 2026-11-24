use std::collections::HashSet;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::ui::{AgentRow, AppWindow, ChatUi, PlanLimit, PlanSimple};

const AGY_ACCOUNT: &str = "agy-account-";
const SUBSCRIPTION: &[&str] = &["claude-code", "codex", "agy", "command-code"];

fn subscription(id: &str) -> bool {
    SUBSCRIPTION.contains(&id) || id.starts_with("claude-code-") || id.starts_with(AGY_ACCOUNT)
}

fn row_bound(row: &str, bound: &HashSet<String>, agy_active: &HashSet<String>) -> bool {
    if bound.contains(row) {
        return true;
    }
    if row == crate::agy_usage::AGY_ID {
        return bound.contains(crate::agy_usage::AGY_ID);
    }
    if let Some(slot) = row.strip_prefix("agy:") {
        return bound.contains(&format!("{AGY_ACCOUNT}{slot}"))
            || (agy_active.contains(slot) && bound.contains(crate::agy_usage::AGY_ID));
    }
    false
}

fn peak(p: &PlanLimit) -> f32 {
    let mut top = p.five.max(p.week);
    if p.has_opus {
        top = top.max(p.opus);
    }
    for b in p.bars.iter() {
        top = top.max(b.value);
    }
    top.clamp(0.0, 1.0)
}

fn pct(v: f32) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let n = (v * 100.0).round() as i32;
    n
}

fn simple_of_plan(p: &PlanLimit) -> PlanSimple {
    let value = if p.known { peak(p) } else { 0.0 };
    let detail = if !p.error.is_empty() {
        p.error.to_string()
    } else if !p.summary.is_empty() {
        p.summary.to_string()
    } else if p.known {
        format!("{}% used", pct(value))
    } else {
        String::new()
    };
    let label = if p.plan.is_empty() {
        p.label.to_string()
    } else {
        format!("{} · {}", p.label, p.plan)
    };
    PlanSimple {
        id: p.id.clone(),
        label: label.into(),
        detail: detail.into(),
        value,
        known: p.known,
        warn: !p.error.is_empty() || value > 0.9,
        agent: false,
    }
}

fn simple_of_agent(a: &AgentRow) -> PlanSimple {
    let detail = match a.state {
        0 => "Available".to_owned(),
        1 => "Not installed".to_owned(),
        3 => "Adapter missing".to_owned(),
        _ if a.reset.is_empty() => "Limit reached".to_owned(),
        _ => format!("Limit reached · resets {}", a.reset),
    };
    PlanSimple {
        id: a.id.clone(),
        label: a.name.clone(),
        detail: detail.into(),
        value: 0.0,
        known: false,
        warn: a.state == 2,
        agent: true,
    }
}
pub fn render(window: &AppWindow, all: &[PlanLimit], agy_active: &HashSet<String>) {
    let limits: Vec<PlanLimit> = all.to_vec();
    let agy_rows = all
        .iter()
        .any(|p| p.id.starts_with(crate::agy_usage::AGY_ID));
    let mut simple: Vec<PlanSimple> = limits.iter().map(simple_of_plan).collect();
    let agents = window.get_agents();
    for a in agents.iter() {
        let id = a.id.as_str();
        let covered = a.claude
            || (id == "codex" && limits.iter().any(|p| p.id.as_str() == id))
            || (agy_rows && (id == crate::agy_usage::AGY_ID || id.starts_with(AGY_ACCOUNT)));
        if covered || !subscription(id) {
            continue;
        }
        simple.push(simple_of_agent(&a));
    }
    let sticky: HashSet<String> = std::iter::once(window.get_sticky_agent().to_string()).collect();
    let current: Vec<PlanSimple> = simple
        .iter()
        .filter(|p| row_bound(p.id.as_str(), &sticky, agy_active))
        .cloned()
        .collect();
    // 修复：圆环弹窗只显示一个峰值条，看不到 5 小时/每周/Argon 各项额度；改为显示当前 agent 的全部额度条。
    let active: Vec<PlanLimit> = limits
        .iter()
        .filter(|p| row_bound(p.id.as_str(), &sticky, agy_active))
        .cloned()
        .collect();
    let chat = window.global::<ChatUi>();
    chat.set_agy_plan(agy_rows);
    chat.set_plan_active(ModelRc::new(VecModel::from(active)));
    chat.set_plan_current(ModelRc::new(VecModel::from(current)));
    chat.set_plan_limits(ModelRc::new(VecModel::from(limits)));
    chat.set_plan_simple(ModelRc::new(VecModel::from(simple)));
}
