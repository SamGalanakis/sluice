//! The dashboard names who is who (SPEC §8.1): a unit's owner small and muted on its plan row,
//! on its stop in Needs you, in its Runs tab's step view and in its page's head line, once per
//! view and nothing for an unowned unit; every
//! sender and recipient as the identity it is (a session's `@name`, a step, "You"), and the
//! address messages were stored under before sessions named themselves as stored,
//! "orchestrator", muted, never "the orchestrator".
use super::board_fixture;
use super::stored_messages;
use board_fixture::Fixture;
use sluice_model::ids::{AttemptId, RunId};
use sluice_store::RetrySafety;
use stored_messages::{Stored, stored};

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from:?} not in {html}"));
    let rest = &html[start..];
    &rest[..rest.find(to).unwrap_or(rest.len())]
}

#[tokio::test]
async fn a_units_owner_is_said_once_on_its_plan_row_and_in_its_page_head_and_kept_on_its_runs_tab()
{
    let f = Fixture::new().await;
    let id = f.id;
    let run = RunId::new();
    let attempt = AttemptId::new();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET owner='@lash-f4' WHERE project_id=?1 AND step_id LIKE 'alpha-%'",
                [id.to_string()],
            )?;
            tx.sql().execute(
                "UPDATE steps SET generation=1,work_generation=1,run_ids=?2 WHERE project_id=?1 AND step_id='alpha-build'",
                (id.to_string(), serde_json::json!([run]).to_string()),
            )?;
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,'alpha-build',1,1,'executing','{}','hash','2026-10-14T00:00:00Z')",
                (attempt.to_string(), id.to_string()),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at) VALUES (?1,?2,?3,'alpha-build',1,1,'2026-10-14T00:00:00Z')",
                (run.to_string(), id.to_string(), attempt.to_string()),
            )?;
            tx.changed(Some(id), "steps");
            Ok(())
        })
        .await
        .unwrap();
    let own = "<span class=\"own\">@lash-f4</span>";
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    let row = between(&plan, "id=\"u-alpha\"", "<div class=\"ro-drill\">");
    assert!(row.contains(own), "the row's title line: {row}");
    let (_, runs) = f
        .get(&format!("/projects/id/{id}/steps/alpha-build?tab=runs"))
        .await;
    let runs_head = between(&runs, "<div class=\"sv-hd has-own\">", "</div>");
    assert!(runs_head.contains(own), "the Runs tab's head: {runs_head}");
    let view = between(&plan, "id=\"u-alpha\"", "</details>");
    let view = &view[view.find("<div class=\"ro-drill\">").unwrap()..];
    assert!(
        !view.contains("class=\"own\"") && !view.contains("has-own"),
        "the open row already says its owner: {view}"
    );
    assert_eq!(plan.matches("class=\"own\"").count(), 1, "{plan}");

    let (_, page) = f.get(&format!("/projects/id/{id}/units/alpha")).await;
    let head = between(&page, "id=\"unit-band\"", "id=\"unit-detail\"");
    assert!(head.contains(own), "the head's line: {head}");
    assert!(
        !page.contains("has-own"),
        "the page says it once, in its head: {page}"
    );
    let (_, page) = f.get(&format!("/projects/id/{id}/units/beta")).await;
    assert!(!page.contains("class=\"own\""), "{page}");

    // the person's own unit is "you"
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET owner='owner' WHERE project_id=?1 AND step_id LIKE 'alpha-%'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "steps");
            Ok(())
        })
        .await
        .unwrap();
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    let row = between(&plan, "id=\"u-alpha\"", "<div class=\"ro-drill\">");
    assert!(row.contains("<span class=\"own\">you</span>"), "{row}");
}

#[tokio::test]
async fn stops_say_their_owner_after_their_title_and_an_unowned_one_says_nothing() {
    let f = Fixture::new().await;
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET owner='@lash-f4' WHERE project_id=?1 AND step_id LIKE 'beta-%'",
                [id.to_string()],
            )?;
            tx.sql().execute(
                "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='alpha-build'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "steps");
            Ok(())
        })
        .await
        .unwrap();
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    let unowned = super::plan_html::stopped(&plan, "alpha");
    assert!(!unowned.contains("class=\"own\""), "{unowned}");
    let owned = super::plan_html::stopped(&plan, "beta");
    assert!(
        owned.contains("</a></b> <span class=\"own\">@lash-f4</span> failed"),
        "the stop's sentence: {owned}"
    );
}

#[tokio::test]
async fn messages_name_their_identities_and_the_old_shared_address_stays_as_stored_and_muted() {
    let f = Fixture::new().await;
    let id = f.id;
    for m in [
        Stored {
            thread: "step-alpha-build",
            from: "orchestrator",
            to: Some("alpha-build"),
            body: "Use the new dates.",
            ..Default::default()
        },
        Stored {
            thread: "step-alpha-build",
            from: "alpha-build",
            to: Some("orchestrator"),
            body: "Noted.",
            ..Default::default()
        },
        Stored {
            thread: "owner",
            from: "orchestrator",
            to: Some("owner"),
            body: "Keep the old maps?",
            title: Some("Keep the old maps?"),
            question: true,
            ..Default::default()
        },
        Stored {
            thread: "@lash-f4",
            from: "@lash-f4",
            to: Some("owner"),
            body: "Drafts are in.",
            ..Default::default()
        },
        Stored {
            thread: "step-alpha-review",
            from: "@lash-f4",
            to: Some("alpha-review"),
            body: "Check the dates first.",
            ..Default::default()
        },
    ] {
        stored(&f.writer, id, m).await;
    }
    let old = "<span class=\"who who-old\">orchestrator</span>";
    let thread = |t: &str| format!("/projects/id/{id}/thread?thread={}", t.replace('@', "%40"));
    let never = |html: &str| {
        assert!(!html.contains("the orchestrator"), "{html}");
        assert!(!html.contains("The orchestrator"), "{html}");
        assert!(!html.contains(">Orchestrator<"), "{html}");
    };

    let (_, page) = f.get(&thread("step-alpha-build")).await;
    assert_eq!(page.matches(old).count(), 2, "sender and recipient: {page}");
    never(&page);

    let (_, page) = f.get(&thread("@lash-f4")).await;
    assert!(page.contains("You and @lash-f4"), "{page}");
    assert!(
        page.contains("<span class=\"who\">@lash-f4</span>"),
        "{page}"
    );

    let (_, page) = f.get(&thread("owner")).await;
    assert!(
        page.contains("You and <span class=\"who-old\">orchestrator</span>"),
        "{page}"
    );
    never(&page);

    let (_, page) = f.get(&thread("step-alpha-review")).await;
    assert!(
        page.contains("<span class=\"who\">@lash-f4</span>"),
        "{page}"
    );

    // the question, as the plan, home and the inbox draw it
    let (_, plan) = f.get(&format!("/projects/id/{id}")).await;
    assert!(
        plan.contains("<b><span class=\"who-old\">orchestrator</span></b> asks:"),
        "{plan}"
    );
    never(&plan);
    let (_, home) = f.get("/").await;
    assert!(
        home.contains("<span class=\"who who-old\">orchestrator</span> asks: <b><a href="),
        "{home}"
    );
    never(&home);
    let (_, inbox) = f.get("/inbox").await;
    assert!(inbox.contains(old), "{inbox}");
    never(&inbox);

    // the log names them alike
    let (_, log) = f.get(&format!("/projects/id/{id}/log")).await;
    assert!(
        log.contains("<span class=\"who-old\">orchestrator</span>"),
        "{log}"
    );
    never(&log);
}
