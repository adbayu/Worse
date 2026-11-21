use super::*;

type R = Result<(), Box<dyn std::error::Error>>;

fn seed(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("memory").join("p1"))?;
    std::fs::write(dir.join("harness.sqlite"), vec![7u8; 200_000])?;
    std::fs::write(dir.join("settings.json"), "{}")?;
    std::fs::write(dir.join("memory").join("p1").join("MEMORY.md"), "note")?;
    std::fs::write(dir.join("x.lock"), "")?;
    Ok(())
}

#[tokio::test]
async fn relocates_and_moves_back() -> R {
    let root = tempfile::tempdir()?;
    let base = root.path().join("base");
    let target = root.path().join("elsewhere").join("data");
    seed(&base)?;
    schedule(&base, &target).await?;
    assert_eq!(pending(&base).await, Some(absolute(&target)?));
    let report = apply_pending(&base).await?.ok_or("no report")?;
    assert_eq!(report.files_copied, 3);
    assert!(report.cleanup_complete);
    assert!(pending(&base).await.is_none());
    let info = resolve_data_dir(&base).await;
    assert!(info.relocated);
    assert_eq!(info.data_dir, absolute(&target)?);
    assert_eq!(
        std::fs::read_to_string(target.join("memory/p1/MEMORY.md"))?,
        "note"
    );
    assert!(!base.join("harness.sqlite").exists());
    assert!(!base.join("memory").exists());
    assert!(
        base.join("x.lock").exists(),
        "lock files are neither copied nor deleted"
    );

    // Move back to the base dir: pointer removed, old dir removed.
    std::fs::remove_file(base.join("x.lock"))?;
    let report = relocate(&base, &info.data_dir, &base).await?;
    assert!(report.cleanup_complete);
    let info = resolve_data_dir(&base).await;
    assert!(!info.relocated);
    assert!(base.join("harness.sqlite").exists());
    assert!(!target.exists());
    Ok(())
}

#[tokio::test]
async fn interruption_before_switch_keeps_old_dir_and_retry_succeeds() -> R {
    let root = tempfile::tempdir()?;
    let base = root.path().join("base");
    let target = root.path().join("new");
    seed(&base)?;
    for point in [FailPoint::AfterCopy, FailPoint::AfterVerify] {
        assert!(relocate_with_fail_point(&base, &base, &target, Some(point))
            .await
            .is_err());
        let info = resolve_data_dir(&base).await;
        assert!(!info.relocated);
        assert!(base.join("harness.sqlite").exists());
        assert!(target.join(COPY_MARKER).exists());
    }
    relocate(&base, &base, &target).await?;
    assert!(resolve_data_dir(&base).await.relocated);
    assert!(!target.join(COPY_MARKER).exists());
    Ok(())
}

#[tokio::test]
async fn interruption_after_switch_resumes_cleanup() -> R {
    let root = tempfile::tempdir()?;
    let base = root.path().join("base");
    let target = root.path().join("new");
    seed(&base)?;
    assert!(
        relocate_with_fail_point(&base, &base, &target, Some(FailPoint::AfterSwitch))
            .await
            .is_err()
    );
    assert!(base.join("harness.sqlite").exists());
    let info = resolve_data_dir(&base).await;
    assert!(info.relocated);
    assert!(!base.join("harness.sqlite").exists());
    assert!(!base.join(CLEANUP_FILE).exists());
    Ok(())
}

#[tokio::test]
async fn rejects_bad_targets_and_lost_marker() -> R {
    let root = tempfile::tempdir()?;
    let base = root.path().join("base");
    seed(&base)?;
    assert!(schedule(&base, Path::new("relative")).await.is_err());
    assert!(schedule(&base, &base.join("inner")).await.is_err());
    let busy = root.path().join("busy");
    std::fs::create_dir_all(&busy)?;
    std::fs::write(busy.join("f"), "x")?;
    assert!(schedule(&base, &busy).await.is_err());
    std::fs::write(
        base.join(POINTER_FILE),
        serde_json::to_string(&serde_json::json!({"dataDir": root.path().join("gone")}))?,
    )?;
    assert!(!resolve_data_dir(&base).await.relocated);
    Ok(())
}
