use super::sections::CommitGate;
use super::*;

#[test]
fn partial_file_gets_defaults() -> Result<(), serde_json::Error> {
    let s: Settings = serde_json::from_str(r#"{"appearance":{"theme":"dark","uiFontSize":99}}"#)?;
    let s = s.normalized();
    assert_eq!(s.appearance.theme, ThemePref::Dark);
    assert_eq!(s.appearance.ui_font_size, 20);
    assert!(s.general.show_reasoning);
    assert_eq!(s.general.language, Language::System);
    assert_eq!(s.schema_version, SCHEMA_VERSION);
    // 旧 settings.json 没有 skillHealth：缺省关闭，不改变既有的检查行为（skill-health.md §State owner）。
    assert!(!s.skill_health.live_check);
    Ok(())
}

#[test]
fn follow_up_accepts_zcode_alias() -> Result<(), serde_json::Error> {
    let g: GeneralSettings = serde_json::from_str(r#"{"followUp":"guide"}"#)?;
    assert_eq!(g.follow_up, FollowUp::Steer);
    assert_eq!(Language::from_tag("zh"), Some(Language::ZhCn));
    Ok(())
}

#[test]
fn native_frame_defaults_to_true_and_round_trips() -> Result<(), serde_json::Error> {
    // Existing settings.json files have no `nativeFrame`: the OS frame is the default.
    let s: Settings = serde_json::from_str(r#"{"appearance":{"theme":"dark"}}"#)?;
    assert!(s.appearance.native_frame);
    let json = serde_json::to_value(&s)?;
    assert_eq!(json["appearance"]["nativeFrame"], true);

    let s: Settings = serde_json::from_str(r#"{"appearance":{"nativeFrame":false}}"#)?;
    assert!(!s.appearance.native_frame);
    // camelCase key, like the rest of settings.json.
    let json = serde_json::to_value(&s)?;
    assert_eq!(json["appearance"]["nativeFrame"], false);
    Ok(())
}

#[test]
fn setup_auto_update_defaults_to_on() -> Result<(), serde_json::Error> {
    let s: Settings = serde_json::from_str(r#"{"setup":{"firstRunDone":true}}"#)?;
    assert!(s.setup.auto_update_adapters);
    assert!(s.setup.first_run_done);
    let s: Settings = serde_json::from_str(r#"{"setup":{"autoUpdateAdapters":false}}"#)?;
    assert!(!s.setup.auto_update_adapters);
    let json = serde_json::to_value(&s)?;
    assert_eq!(json["setup"]["autoUpdateAdapters"], false);
    Ok(())
}

#[test]
fn security_defaults_and_round_trip() -> Result<(), serde_json::Error> {
    let s: Settings = serde_json::from_str("{}")?;
    assert_eq!(s.security, SecuritySettings::default());
    assert_eq!(s.security.commit_gate, CommitGate::Off);
    assert!(!s.security.scan_on_lockfile_change);
    assert!(s.security.show_diff_badge);

    let s: Settings =
        serde_json::from_str(r#"{"security":{"commitGate":"block","offlineScan":true}}"#)?;
    assert_eq!(s.security.commit_gate, CommitGate::Block);
    assert!(s.security.offline_scan);
    let json = serde_json::to_value(&s)?;
    assert_eq!(json["security"]["commitGate"], "block");
    assert_eq!(json["security"]["showDiffBadge"], true);

    assert!(serde_json::from_str::<Settings>(r#"{"security":{"commitGate":"nope"}}"#).is_err());
    Ok(())
}
#[test]
fn remote_defaults_and_round_trip() -> Result<(), serde_json::Error> {
    // Missing `remote` in an old file: defaults, no auto-start.
    let s: Settings = serde_json::from_str("{}")?;
    assert_eq!(s.remote, RemoteSettings::default());
    assert!(!s.remote.enabled);
    assert_eq!(s.remote.port, 7878);
    assert_eq!(s.remote.tunnel_bin, "cftunnel");

    let s: Settings = serde_json::from_str(
        r#"{"remote":{"enabled":true,"port":9001,"tunnelBin":"C:\\t.exe","subdomain":"harness","tunnelDomain":"example.com"}}"#,
    )?;
    assert!(s.remote.enabled);
    assert_eq!(s.remote.port, 9001);
    assert_eq!(s.remote.subdomain, "harness");
    assert_eq!(s.remote.tunnel_domain, "example.com");
    // Keys are camelCase, like the rest of settings.json.
    let json = serde_json::to_value(&s)?;
    assert_eq!(json["remote"]["tunnelBin"], "C:\\t.exe");
    assert_eq!(json["remote"]["tunnelDomain"], "example.com");
    // A privileged port falls back to the default instead of failing to bind.
    let s: Settings = serde_json::from_str(r#"{"remote":{"port":80}}"#)?;
    assert_eq!(s.normalized().remote.port, 7878);
    Ok(())
}
