use sluice_model::{
    commands::{CommandReply, CommandRequest, MessagePost},
    error::PublicError,
    ids::ProjectId,
};
use sluice_store::{RetrySafety, Writer};
use sluice_web::views::{
    PageState,
    inbox::{CommandFuture, MessageCommands, MessageState},
};
use std::sync::Arc;
struct FixtureCommands(Writer);
impl MessageCommands for FixtureCommands {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_> {
        let writer = self.0.clone();
        Box::pin(async move {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| match request {
                    CommandRequest::MessagePost(post) => Ok(CommandReply::Posted {
                        id: sluice_store::messages::message_post(
                            tx,
                            post,
                            &sluice_store::messages::NoPlanInputs,
                        )?
                        .id,
                    }),
                    CommandRequest::MarkRead(read) => {
                        sluice_store::messages::mark_read(tx, read)?;
                        Ok(CommandReply::Ack)
                    }
                    _ => Err(PublicError::BadRequest {
                        message: "fixture only supports message commands".into(),
                    }
                    .into()),
                })
                .await
        })
    }
}

pub async fn seed(writer: &Writer, id: ProjectId) {
    let fixture_project = id;
    let form = r#"root = Stack([Heading("Choose the release limit"), Text("The number is sent as a JSON number."), Form("limit", [Input("value", "Limit", "1 to 10", "number", "3", ["required", "min:1", "max:10"]), Checkbox("confirmed", "I checked the evidence", false)], [Button("Apply", "submit"), Button("Close", "close", {}, "secondary")])])"#;
    for (index, thread, from, to, title, body, needs_reply, ui) in [
        (
            0,
            "release",
            "work-2",
            "owner",
            "Approve the release",
            "The checks passed. **Choose a limit** before continuing.",
            true,
            Some(form),
        ),
        (
            1,
            "stale",
            "work-2",
            "owner",
            "A stale approval",
            "A second browser can answer this question while this form is open.",
            true,
            Some("root = Stack([Button(\"Approve\", \"approve\")])"),
        ),
        (
            2,
            "step-work-2",
            "work-2",
            "orchestrator",
            "Where should this run?",
            "The worker needs a directory, and this question belongs in Questions.",
            true,
            None,
        ),
        (
            3,
            "decision",
            "orchestrator",
            "owner",
            "The selected approach",
            "We kept the small typed command boundary. The alternative was a web-owned writer. The transaction tests support the choice; the adapter can be replaced without changing the pages.",
            false,
            None,
        ),
        (
            4,
            "long-conversation",
            "owner",
            "orchestrator",
            "Review the evidence",
            "Please record the decision and the test evidence here.",
            false,
            None,
        ),
    ] {
        let post: MessagePost = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":fixture_project.to_string()},"thread":thread,"from":from,"to":to,"title":title,"body":body,"needs_reply":needs_reply,"ui":ui})).unwrap();
        let message = writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::message_post(
                    tx,
                    post,
                    &sluice_store::messages::NoPlanInputs,
                )
            })
            .await
            .unwrap();
        println!("MESSAGE {index} {}", message.id.0);
    }
    for n in 0..16 {
        let post: MessagePost = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":fixture_project.to_string()},"thread":"long-conversation","from":if n%2==0 {"work-2"} else {"orchestrator"},"to":if n%2==0 {"orchestrator"} else {"owner"},"body":format!("### Evidence {}\n\nA paragraph explaining the result and its consequences. The test checked the rendered thread and preserved questions in the other conversation.\n\n```text\n{}\n```", n+1, "long-path/".repeat(14)),"needs_reply":false})).unwrap();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::message_post(
                    tx,
                    post,
                    &sluice_store::messages::NoPlanInputs,
                )
            })
            .await
            .unwrap();
    }
}
pub async fn configure(state: &mut PageState, writer: &Writer, _id: ProjectId) {
    state.messages = Some(MessageState {
        dashboard: state.dashboard.clone(),
        commands: Arc::new(FixtureCommands(writer.clone())),
    });
}

pub fn layer(router: axum::Router) -> axum::Router {
    router
}
