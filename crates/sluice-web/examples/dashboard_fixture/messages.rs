use sluice_model::{
    commands::{CommandReply, CommandRequest, Message, MessageVerb},
    error::PublicError,
    events::Event,
    ids::{MessageId, ProjectId},
};
use sluice_store::messages::{NoPlanInputs, Post, post};
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
                    CommandRequest::Ask(m) => Ok(CommandReply::Receipt(
                        post(tx, Post::try_from(m)?, &NoPlanInputs)?.receipt,
                    )),
                    CommandRequest::Say(m) => Ok(CommandReply::Receipt(
                        post(tx, Post::try_from(m)?, &NoPlanInputs)?.receipt,
                    )),
                    CommandRequest::Reply(m) => {
                        let reply = sluice_store::messages::reply_post(tx.sql(), m)?;
                        Ok(CommandReply::Receipt(post(tx, reply, &NoPlanInputs)?.receipt))
                    }
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
        let message = stored(
            writer,
            fixture_project,
            Stored {
                thread,
                from,
                to,
                title: Some(title),
                body: body.into(),
                question: needs_reply,
                ui,
            },
        )
        .await;
        println!("MESSAGE {index} {}", message.id.0);
    }
    for n in 0..16 {
        stored(
            writer,
            fixture_project,
            Stored {
                thread: "long-conversation",
                from: if n % 2 == 0 { "work-2" } else { "orchestrator" },
                to: if n % 2 == 0 { "orchestrator" } else { "owner" },
                title: None,
                body: format!("### Evidence {}\n\nA paragraph explaining the result and its consequences. The test checked the rendered thread and preserved questions in the other conversation.\n\n```text\n{}\n```", n + 1, "long-path/".repeat(14)),
                question: false,
                ui: None,
            },
        )
        .await;
    }
}
struct Stored {
    thread: &'static str,
    from: &'static str,
    to: &'static str,
    title: Option<&'static str>,
    body: String,
    question: bool,
    ui: Option<&'static str>,
}
/// Conversations as a home keeps them, older free-named threads included: the rows and
/// records are written directly, as any release may have stored them.
async fn stored(writer: &Writer, project: ProjectId, m: Stored) -> Message {
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let mut message = Message {
                id: MessageId(0),
                verb: if m.question {
                    MessageVerb::Ask
                } else {
                    MessageVerb::Say
                },
                from: m.from.into(),
                to: Some(m.to.into()),
                thread: m.thread.into(),
                body: m.body,
                title: m.title.map(Into::into),
                ui: m.ui.map(Into::into),
                input: None,
                data: None,
                run: None,
                at: String::new(),
                to_message: None,
                answer: None,
                state: None,
                answered_by: None,
            };
            let record = tx.append_record(Some(project), Event::Message(Box::new(message.clone())))?;
            message.id = MessageId(record.seq.0);
            message.at = record.at;
            tx.sql().execute(
                "UPDATE records SET thread=?1,payload=?2 WHERE seq=?3",
                (
                    &message.thread,
                    serde_json::to_string(&Event::Message(Box::new(message.clone())))?,
                    message.id.0,
                ),
            )?;
            tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",title,body,needs_reply,ui,at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",(message.id.0,project.to_string(),&message.thread,&message.from,&message.to,&message.title,&message.body,m.question,&message.ui,&message.at))?;
            tx.changed(Some(project), "messages");
            Ok(message)
        })
        .await
        .unwrap()
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
