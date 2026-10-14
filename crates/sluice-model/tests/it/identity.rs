use sluice_model::identity::*;

#[test]
fn session_identity_grammar_and_resolution() {
    {
        for good in ["@a", "@lash-f4", "@x9", &format!("@a{}", "b".repeat(39))] {
            assert!(is_session(good), "{good}");
        }
        for bad in [
            "a",
            "@",
            "@9a",
            "@-a",
            "@A",
            "@a_b",
            "@a.b",
            "@ab ",
            &format!("@a{}", "b".repeat(40)),
        ] {
            assert!(!is_session(bad), "{bad}");
        }
    }

    {
        let bound = SessionName::parse("@bound").unwrap();
        let me =
            |arg, env, bound| resolve_me(arg, env, bound).map(|m| m.map(|m| m.as_str().to_owned()));
        assert_eq!(
            me(Some("@arg"), Some("@env"), None).unwrap(),
            Some("@arg".into())
        );
        assert_eq!(me(None, Some("@env"), None).unwrap(), Some("@env".into()));
        assert_eq!(me(None, None, Some(&bound)).unwrap(), Some("@bound".into()));
        assert_eq!(
            me(Some("@bound"), None, Some(&bound)).unwrap(),
            Some("@bound".into())
        );
        assert_eq!(me(None, None, None).unwrap(), None);
        let other = me(Some("@other"), None, Some(&bound)).unwrap_err();
        assert!(
            other.to_string().contains("this MCP session is @bound"),
            "{other}"
        );
        assert!(me(Some("owner"), None, None).is_err());
    }
}
