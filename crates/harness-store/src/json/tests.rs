use super::*;

#[test]
fn spread_keeps_first_position_and_last_value() {
    let obj = spread_json(r#" {"b":1,"a":{"x":[1,2]},"b":"two"}"#).unwrap_or_default();
    assert_eq!(J::Obj(obj).to_text(), r#"{"b":"two","a":{"x":[1,2]}}"#);
}

#[test]
fn spread_of_primitives_and_rejections() {
    assert!(spread_json("null").is_err(), "TS throws on null.traceId");
    assert!(spread_json("").is_err(), "TS JSON.parse throws");
    assert_eq!(
        spread_json("true").map(|o| J::Obj(o).to_text()).ok(),
        Some("{}".to_owned())
    );
    assert_eq!(
        spread_json("12.5").map(|o| J::Obj(o).to_text()).ok(),
        Some("{}".to_owned())
    );
    assert!(spread_json("\"ab\"").is_err());
    assert!(spread_json("[1]").is_err());
    assert!(spread_json("{bad").is_err());
}

#[test]
fn assignment_keeps_position_and_undefined_is_omitted() {
    let obj = Obj::new()
        .with("a", J::bool(true))
        .with("b", J::str("x\"\n\u{1}"))
        .with("a", J::Undef)
        .with("c", J::null());
    assert_eq!(J::Obj(obj).to_text(), r#"{"b":"x\"\n\u0001","c":null}"#);
}

#[test]
fn coercions_follow_js() {
    assert!(Sql::Real(1.0).is_one());
    assert!(!Sql::Text("1".into()).is_one());
    assert_eq!(Sql::Null.js_string().ok(), Some("null".to_owned()));
    assert_eq!(
        Sql::Null.js_number().map(|j| j.to_text()).ok(),
        Some("0".to_owned())
    );
    assert_eq!(
        Sql::Real(1.5).js_number().map(|j| j.to_text()).ok(),
        Some("1.5".to_owned())
    );
    assert_eq!(
        Sql::Real(f64::INFINITY)
            .js_number()
            .map(|j| j.to_text())
            .ok(),
        Some("null".to_owned())
    );
    assert!(Sql::Int(1).strict_eq(&Sql::Real(1.0)));
    assert!(!Sql::Int(1).strict_eq(&Sql::Text("1".into())));
    assert!(Sql::from_ref(ValueRef::Integer(i64::MAX)).is_err());
    assert!(param_truthy(Some(&serde_json::json!("x"))));
    assert!(!param_truthy(Some(&serde_json::json!(0))));
}
