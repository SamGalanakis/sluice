//! Message rows and their records as any release may have stored them: free thread
//! names, senders without a run, questions to nobody. Readers (the inbox, `next`,
//! `watch`, the log) must take them all; posting itself is tested through `post`.
use sluice_model::{
    commands::{Message, MessageVerb},
    events::Event,
    ids::{MessageId, ProjectId},
};
use sluice_store::{RetrySafety, WriteTransaction, Writer};

#[derive(Debug, Clone, Default)]
pub struct Stored<'a> {
    pub thread: &'a str,
    pub from: &'a str,
    pub to: Option<&'a str>,
    pub body: &'a str,
    pub title: Option<&'a str>,
    pub ui: Option<&'a str>,
    pub input: Option<&'a str>,
    pub question: bool,
    pub reply: Option<MessageId>,
}

/// Write one stored message (a reply resolves its open parent, as posting did).
pub fn store(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    m: &Stored<'_>,
) -> sluice_store::Result<Message> {
    let mut message = Message {
        id: MessageId(0),
        verb: match (m.reply, m.question) {
            (Some(_), _) => MessageVerb::Reply,
            (None, true) => MessageVerb::Ask,
            (None, false) => MessageVerb::Say,
        },
        from: m.from.into(),
        to: m.to.map(Into::into),
        thread: if m.thread.is_empty() { "t" } else { m.thread }.into(),
        body: m.body.into(),
        title: m.title.map(Into::into),
        ui: m.ui.map(Into::into),
        input: m.input.map(Into::into),
        data: None,
        run: None,
        at: String::new(),
        to_message: m.reply,
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
    tx.sql().execute(
        "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",title,body,needs_reply,reply_to,ui,input,at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        (
            message.id.0,
            project.to_string(),
            &message.thread,
            &message.from,
            &message.to,
            &message.title,
            &message.body,
            m.question,
            m.reply.map(|r| r.0),
            &message.ui,
            &message.input,
            &message.at,
        ),
    )?;
    if let Some(parent) = m.reply.filter(|_| !m.question) {
        tx.sql().execute(
            "UPDATE messages SET resolved_by=?1 WHERE project_id=?2 AND id=?3 AND needs_reply=1 AND resolved_by IS NULL",
            (message.id.0, project.to_string(), parent.0),
        )?;
    }
    for view in ["messages", "questions", "status"] {
        tx.changed(Some(project), view);
    }
    Ok(message)
}

pub async fn stored(writer: &Writer, project: ProjectId, m: Stored<'_>) -> Message {
    let (thread, from, to, body, title, ui, input) = (
        m.thread.to_owned(),
        m.from.to_owned(),
        m.to.map(str::to_owned),
        m.body.to_owned(),
        m.title.map(str::to_owned),
        m.ui.map(str::to_owned),
        m.input.map(str::to_owned),
    );
    let (question, reply) = (m.question, m.reply);
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let m = Stored {
                thread: &thread,
                from: &from,
                to: to.as_deref(),
                body: &body,
                title: title.as_deref(),
                ui: ui.as_deref(),
                input: input.as_deref(),
                question,
                reply,
            };
            store(tx, project, &m)
        })
        .await
        .unwrap()
}
