use sluice_model::{error::PublicError, shown::*};

#[test]
fn persisted_status_vocabulary_and_cancel_errors() {
    {
        for (i, state) in Shown::ALL.into_iter().enumerate() {
            assert_eq!(state.rank(), i, "{state:?}");
            assert_eq!(Shown::from_key(state.key()), Some(state));
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.key())
            );
        }
        let keys: std::collections::BTreeSet<_> = Shown::ALL.map(Shown::key).into();
        let words: std::collections::BTreeSet<_> = Shown::ALL.map(Shown::word).into();
        let lanes: std::collections::BTreeSet<_> = Shown::ALL.map(|s| s.spec().lane).into();
        assert_eq!(keys.len(), Shown::ALL.len());
        assert_eq!(words.len(), Shown::ALL.len());
        assert_eq!(lanes.len(), Shown::ALL.len());
        assert_eq!(Shown::from_key("unknown"), None);
    }

    {
        for (stored, cancel) in [
            (
                r#"{"error":"cancelled","message":"cancel requested"}"#,
                true,
            ),
            (
                r#"{"error":"agent_failure","kind":"Cancelled","message":"x"}"#,
                true,
            ),
            (
                r#"{"error":"agent_failure","kind":"WallCap","message":"x"}"#,
                false,
            ),
            (
                r#"{"error":"fn_failure","message":"cancelled: not needed"}"#,
                true,
            ),
            (
                r#"{"error":"fn_failure","message":"Cancelled: shouting"}"#,
                false,
            ),
            (r#"{"error":"fn_failure","message":"exit code 1"}"#, false),
            ("cancelled", true),
            ("cancelled: by hand", true),
            ("tests failed", false),
        ] {
            assert_eq!(stored_is_cancel(stored), cancel, "{stored}");
        }
    }

    {
        let failure = PublicError::AgentFailure {
            retry_at: None,
            kind: "EngineExited".into(),
            message: "\nclaude: transcript record exceeds 1 MiB\npane at failure".into(),
            session: None,
        };
        let words = set_aside("superseded by the new lane", &failure);
        assert_eq!(
            words,
            "superseded by the new lane (it had failed: EngineExited: claude: transcript record exceeds 1 MiB)"
        );
        assert_eq!(
            split_set_aside(&words),
            (
                "superseded by the new lane",
                Some("EngineExited: claude: transcript record exceeds 1 MiB")
            )
        );
        let bare = set_aside(
            " ",
            &PublicError::FnFailure {
                message: "boom".into(),
            },
        );
        assert_eq!(bare, "it had failed: fn_failure: boom");
        assert_eq!(split_set_aside(&bare), ("", Some("fn_failure: boom")));
        assert_eq!(split_set_aside("pivot (later)"), ("pivot (later)", None));
        let long = set_aside(
            "",
            &PublicError::FnFailure {
                message: "x".repeat(400),
            },
        );
        assert!(long.chars().count() < 240 && long.ends_with('…'), "{long}");
    }
}
