use super::*;

fn temp_auth(tag: &str) -> Auth {
    let dir =
        std::env::temp_dir().join(format!("harness-remote-auth-{}-{tag}", std::process::id()));
    Auth::load(&dir)
}

#[test]
fn short_codes_use_the_alphabet() {
    for _ in 0..64 {
        let code = short_code();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|byte| CODE_ALPHABET.contains(&byte)));
    }
}

#[test]
fn constant_time_compare_matches_equality() {
    assert!(ct_eq("abc", "abc"));
    assert!(!ct_eq("abc", "abd"));
    assert!(!ct_eq("abc", "ab"));
    assert!(!ct_eq("", "a"));
}

#[test]
fn rate_limit_allows_ten_then_blocks() {
    let auth = temp_auth("rate");
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    for _ in 0..RATE_LIMIT {
        assert!(auth.allow(ip));
        assert!(auth.fail(ip));
    }
    assert!(!auth.allow(ip));
    assert!(!auth.fail(ip));
}

#[test]
fn offer_is_single_use_and_expires() {
    let auth = temp_auth("offer");
    let offer = auth.new_offer("Pixel");
    assert_eq!(offer.token.len(), 64);
    assert_eq!(offer.code.len(), 8);
    let (id, token) = auth
        .pair(None, Some(&offer.code.to_lowercase()), None)
        .unwrap();
    assert_eq!(auth.device_for_token(&token).as_deref(), Some(id.as_str()));
    assert!(auth.pair(None, Some(&offer.code), None).is_none());
    let stale = auth.new_offer("Old");
    auth.expire(&stale.token);
    assert!(auth.pair(Some(&stale.token), None, None).is_none());
}

#[test]
fn full_device_list_unpairs_the_oldest() {
    let auth = temp_auth("cap");
    let mut tokens = Vec::new();
    for _ in 0..=MAX_DEVICES {
        let offer = auth.new_offer("Phone");
        let (_, token) = auth.pair(None, Some(&offer.code), None).unwrap();
        tokens.push(token);
    }
    assert!(auth.device_for_token(&tokens[0]).is_none());
    assert!(auth.device_for_token(&tokens[MAX_DEVICES]).is_some());
}
