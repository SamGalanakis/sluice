//! The dashboard names who is who (SPEC §8.1): a unit's owner small and muted on its plan row,
//! on its stop in Needs you, in its Runs tab's step view and in its page's head line, once per
//! view and nothing for an unowned unit; every
//! sender and recipient as the identity it is (a session's `@name`, a step, "You"), and the
//! address messages were stored under before sessions named themselves as stored,
//! "orchestrator", muted, never "the orchestrator".
use super::board_fixture;
use board_fixture::Fixture;
use sluice_model::ids::{AttemptId, RunId};
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from:?} not in {html}"));
    let rest = &html[start..];
    &rest[..rest.find(to).unwrap_or(rest.len())]
}

#[tokio::test]
async fn a_shared_owner_is_omitted_on_the_roster_and_kept_on_unit_and_run_pages() {
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
    let plan = f.opened(&format!("/projects/id/{id}")).await;
    let row = between(&plan, "id=\"u-alpha\"", "<div id=\"drill-");
    assert!(
        !row.contains(own),
        "the sole roster owner is omitted: {row}"
    );
    let (_, runs) = f
        .get(&format!("/projects/id/{id}/steps/alpha-build?tab=runs"))
        .await;
    let runs_head = between(&runs, "<div class=\"sv-hd has-own\">", "</div>");
    assert!(runs_head.contains(own), "the Runs tab's head: {runs_head}");
    let view = between(&plan, "id=\"u-alpha\"", "</details>");
    let view = &view[view.find("<div id=\"drill-").unwrap()..];
    assert!(
        !view.contains("class=\"own\"") && !view.contains("has-own"),
        "the open row keeps owner detail on its unit page: {view}"
    );
    assert_eq!(plan.matches("class=\"own\"").count(), 0, "{plan}");

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
    let plan = f.opened(&format!("/projects/id/{id}")).await;
    let row = between(&plan, "id=\"u-alpha\"", "<div id=\"drill-");
    assert!(!row.contains("class=\"own\""), "{row}");
    // With distinct roster owners, only the project's fallback is redundant.
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("UPDATE steps SET status='running',owner='@reviewer' WHERE project_id=?1 AND step_id='beta-build'", [id.to_string()])?;
        tx.sql().execute("UPDATE projects SET fallback='owner' WHERE project_id=?1", [id.to_string()])?;
        tx.changed(Some(id), "steps");
        Ok(())
    }).await.unwrap();
    let plan = f.opened(&format!("/projects/id/{id}")).await;
    let alpha = between(&plan, "id=\"u-alpha\"", "<div id=\"drill-");
    let beta = between(&plan, "id=\"u-beta\"", "<div id=\"drill-");
    assert!(
        !alpha.contains("class=\"own\""),
        "fallback omitted: {alpha}"
    );
    assert!(
        beta.contains("<span class=\"own\">@reviewer</span>"),
        "distinct owner kept: {beta}"
    );
}

#[tokio::test]
async fn stops_say_their_owner_after_their_state_and_an_unowned_one_says_nothing() {
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
    let plan = f.opened(&format!("/projects/id/{id}")).await;
    let unowned = super::plan_html::stopped(&plan, "alpha");
    assert!(!unowned.contains("class=\"own\""), "{unowned}");
    let owned = super::plan_html::stopped(&plan, "beta");
    assert!(
        owned.contains("<span class=\"nd-who\">· <span class=\"own\">@lash-f4</span> · "),
        "the stop's line: {owned}"
    );
}
