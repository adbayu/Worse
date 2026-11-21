use super::{
    check, hub_config, options_text, relocation, start, step, Env, FailPoint, HarnessCore, Layout,
    ListSessions, Path, PathBuf, SessionId, SettingsStore, ThemePref, R,
};

pub(super) async fn relocation_step(env: &Env, layout: &Layout, s1: &SessionId) -> R<()> {
    let base = env.home.clone();
    let target = layout.home.join("harness-moved");
    // Interrupted after the copy: no pointer, the old dir stays in use, the retry succeeds.
    let interrupted =
        relocation::relocate_with_fail_point(&base, &base, &target, Some(FailPoint::AfterCopy))
            .await;
    check(interrupted.is_err(), "fail point did not interrupt")?;
    let info = relocation::resolve_data_dir(&base).await;
    check(
        info.data_dir == base && !info.relocated,
        "interrupted relocation switched the data dir",
    )?;
    relocation::schedule(&base, &target)
        .await
        .map_err(|e| format!("schedule: {e}"))?;
    let report = relocation::apply_pending(&base)
        .await
        .map_err(|e| format!("relocate: {e}"))?
        .ok_or("no pending relocation applied")?;
    check(report.files_copied > 0, "nothing copied")?;
    let info = relocation::resolve_data_dir(&base).await;
    check(
        info.relocated && info.data_dir == relocation_abs(&target),
        format!("data dir is {}", info.data_dir.display()),
    )?;
    println!("NOTE relocated to {}", info.data_dir.display());
    let gate = start(hub_config(env, &info.data_dir, false)).await?;
    let sessions = gate
        .hub
        .list_sessions(ListSessions::default())
        .await
        .map_err(|e| e.to_string())?;
    check(
        sessions.iter().any(|s| &s.id == s1),
        "session missing after relocation",
    )?;
    let detail = gate
        .hub
        .open_session(s1.clone())
        .await
        .map_err(|e| e.to_string())?;
    check(detail.turns.len() >= 4, "turns missing after relocation")?;
    let text = options_text(&gate, s1, "").await?;
    check(
        text.contains("options=mode:acceptEdits"),
        "choices lost after relocation",
    )?;
    gate.hub.shutdown().await.map_err(|e| e.to_string())?;
    let store = SettingsStore::open(&info.data_dir)
        .await
        .map_err(|e| format!("settings after relocation: {e}"))?;
    check(
        store.snapshot().appearance.theme == ThemePref::Light,
        "settings lost after relocation",
    )?;
    step("relocation");
    Ok(())
}

pub(super) fn relocation_abs(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}
