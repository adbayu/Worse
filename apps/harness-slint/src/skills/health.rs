use super::{push, skill_health, Grade, Report, Req, STATE};

/// `Check skills`: asks the settings worker for a fresh health report. Never starts skill-reach.
pub(super) fn check_health() {
    let seq = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let s = cell.as_mut()?;
        s.health_seq += 1;
        s.checking = true;
        Some((s.health_seq, s.req.clone()))
    });
    let Some((seq, req)) = seq else {
        return;
    };
    push();
    let _ = req.send(Req::SkillsHealth { seq });
}

/// Report arrived from the settings worker; `live` is the copyable command when the opt-in is on.
pub fn health_loaded(seq: u64, report: Result<Report, String>, live: Option<String>) {
    let _ = slint::invoke_from_event_loop(move || {
        STATE.with(|cell| {
            let mut cell = cell.borrow_mut();
            let Some(s) = cell.as_mut() else {
                return;
            };
            if s.health_seq != seq {
                return;
            }
            s.checking = false;
            s.live_command = live.unwrap_or_default();
            match report {
                Ok(report) => {
                    s.health = report
                        .skills
                        .iter()
                        .map(|skill| {
                            (
                                skill.name.clone(),
                                (grade_label(skill.grade()).to_string(), detail(skill)),
                            )
                        })
                        .collect();
                    s.health_grade = grade_label(report.grade()).to_string();
                    s.health_score = report.score();
                    // The status line carries errors only; the header badge shows the grade (translated in the UI).
                    s.status = String::new();
                    s.status_error = false;
                }
                Err(error) => {
                    s.health.clear();
                    s.health_grade = String::new();
                    s.status = format!("Skill check failed: {error}");
                    s.status_error = true;
                }
            }
        });
        push();
    });
}

/// Grade as a stable code the UI translates (`good` / `warning` / `poor`).
fn grade_label(grade: Grade) -> &'static str {
    match grade {
        Grade::Good => "good",
        Grade::Warning => "warning",
        Grade::Poor => "poor",
    }
}

/// One line per finding, so the row tooltip says what to fix.
fn detail(skill: &skill_health::SkillHealth) -> String {
    if skill.findings.is_empty() {
        return format!(
            "Score {}: the description routes without conflicts",
            skill.score()
        );
    }
    let mut lines: Vec<String> = skill.findings.iter().map(|f| f.message.clone()).collect();
    lines.push(format!("Score {}", skill.score()));
    lines.join("\n")
}
