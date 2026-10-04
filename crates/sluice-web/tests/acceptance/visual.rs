//! Reproducible native dashboard matrix and interactive settings/drawer acceptance.
use super::{
    chrome::Chrome,
    clients::{Scratch, root},
};
use serde_json::{Value, json};
use sluice_model::ids::ProjectId;
use sluice_store::{RetrySafety, Writer};
#[allow(dead_code)]
#[path = "../../examples/dashboard_fixture/board.rs"]
mod board_fixture;
#[allow(dead_code)]
#[path = "../../examples/dashboard_fixture/home.rs"]
mod home_fixture;
#[allow(dead_code)]
#[path = "../../examples/dashboard_fixture/messages.rs"]
mod message_fixture;
#[allow(dead_code)]
pub(super) async fn seed(writer: &Writer) -> (ProjectId, ProjectId, ProjectId) {
    let home = home_fixture::create(writer).await;
    board_fixture::seed(writer, home).await;
    message_fixture::seed(writer, home).await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            // Production serves the real builtin registry, with no injected custom.open.
            tx.sql().execute(
                "UPDATE plans SET doc=replace(doc,'custom.open','core.external')",
                [],
            )?;
            tx.sql().execute(
                "UPDATE steps SET declaration=replace(declaration,'custom.open','core.external')",
                [],
            )?;
            tx.changed(None, "fixture");
            let board = tx
                .sql()
                .query_row(
                    "SELECT project_id FROM projects WHERE name='board-fixture'",
                    [],
                    |r| r.get::<_, String>(0),
                )?
                .parse()
                .unwrap();
            let paused = tx
                .sql()
                .query_row(
                    "SELECT project_id FROM projects WHERE name='board-paused'",
                    [],
                    |r| r.get::<_, String>(0),
                )?
                .parse()
                .unwrap();
            Ok((home, board, paused))
        })
        .await
        .unwrap()
}
fn capture(browser: &mut Chrome, inventory: &mut Vec<Value>, state: &str, directory: &str) {
    for width in [390, 1440, 2560] {
        for theme in ["light", "dark"] {
            browser.viewport(width, theme).unwrap();
            let geometry=browser.eval(r#"(()=>{const content=[...document.querySelector('main').children].find(e=>e.getBoundingClientRect().width>10&&e.id!=='stream-state'&&!e.classList.contains('vh')),r=content.getBoundingClientRect(),n=document.querySelector('nav.top');return {width:document.documentElement.clientWidth,scroll:document.documentElement.scrollWidth,left:r.left,column:r.width,nav:n.getBoundingClientRect().left+parseFloat(getComputedStyle(n).paddingLeft),drawer:document.documentElement.classList.contains('drawer-open'),height:document.documentElement.scrollHeight}})()"#).unwrap();
            assert!(
                geometry["scroll"].as_f64().unwrap() <= geometry["width"].as_f64().unwrap(),
                "overflow {state} {width} {geometry}"
            );
            if geometry["drawer"] != json!(true) && !state.starts_with("python-") {
                let left = geometry["left"].as_f64().unwrap();
                assert!(
                    (left - geometry["nav"].as_f64().unwrap()).abs() < 1.,
                    "nav alignment {state} {geometry}"
                );
                assert!(
                    (left
                        - (geometry["width"].as_f64().unwrap()
                            - geometry["column"].as_f64().unwrap())
                            / 2.)
                        .abs()
                        < 1.,
                    "column {state} {geometry}"
                );
            }
            let file = format!("{directory}/{state}-{width}-{theme}.png");
            browser.screenshot(&root().join(&file)).unwrap();
            inventory.push(json!({"file":file,"state":state,"width":width,"theme":theme,"geometry":geometry,"inspected":false}));
        }
    }
    std::fs::write(
        root().join(format!("{directory}/capture-manifest.json")),
        serde_json::to_vec_pretty(&inventory).unwrap(),
    )
    .unwrap();
    eprintln!("captured {state}");
}
fn navigate(browser: &mut Chrome, url: &str, path: &str) {
    browser.navigate(&format!("{url}{path}")).unwrap();
    browser
        .wait("!!document.querySelector('main') && !!window.browserErrors")
        .unwrap();
    browser.wait("document.readyState==='complete'").unwrap();
}
fn settings_apply(browser: &mut Chrome, field: &str, value: &str) {
    let result=browser.eval(&format!(r#"(async()=>{{const r=await fetch(location.pathname,{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({{field:{},value:{},expected_settings_rev:Number(document.querySelector('#settings-live').dataset.revision)}})}});return {{status:r.status,text:await r.text()}}}})()"#,json!(field),json!(value))).unwrap();
    assert_eq!(result["status"], json!(200), "{result}");
    browser
        .wait("document.querySelector('#settings-live').dataset.revision!==window.previousRevision")
        .unwrap();
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "G6 native Chromium screenshots; build workspace first and inspect the manifest"]
async fn native_dashboard_matrix_settings_and_drawer() {
    let scratch_home = tempfile::tempdir().unwrap();
    let writer = Writer::open(scratch_home.path()).unwrap();
    let (home, board, paused) = seed(&writer).await;
    writer.shutdown().await.unwrap();
    let mut scratch = Scratch::new(scratch_home);
    scratch.boot().await;
    let url = scratch.url.clone();
    let manifest=tokio::task::spawn_blocking(move || {
        let mut browser=Chrome::open(&url).unwrap();
        let directory="target/screens/p6-07";
        let mut inventory=vec![];
        let board_base=format!("/projects/id/{board}");
        let home_base=format!("/projects/id/{home}");
        for (name,path) in [
            ("home","/".into()),("functions","/fns".into()),("log","/log".into()),
            ("inbox","/inbox".into()),("questions","/questions".into()),("history","/history".into()),
            ("project-functions",format!("/fns?project={home}")),("project-log",format!("{home_base}/log")),
            ("project-inbox",format!("{home_base}/inbox")),("project-questions",format!("{home_base}/questions")),
            ("project-history",format!("{home_base}/history")),("thread",format!("{home_base}/thread?thread=long-conversation")),
            ("board",board_base.clone()),("paused-board",format!("/projects/id/{paused}")),
            ("step",format!("{board_base}/steps/failed")),("unit",format!("{board_base}/units/build")),
            ("settings",format!("{board_base}/settings")),
        ] {
            navigate(&mut browser,&url,&path);
            assert_eq!(browser.eval("browserErrors").unwrap(),json!([]),"{name}");
            capture(&mut browser,&mut inventory,name,directory);
        }
        navigate(&mut browser,&url,&board_base);
        browser.viewport(1440,"light").unwrap();
        browser.wait("!!window.sluiceStream && document.querySelectorAll('.wires path').length>0").unwrap();
        browser.eval("window.drawerRequests=[];const realFetch=window.fetch;window.fetch=(input,opts)=>{if(String(input).includes('/steps/'))drawerRequests.push({url:String(input),signal:opts?.signal});return realFetch(input,opts)}").unwrap();
        browser.eval("document.querySelector('[data-step=failed]').click()").unwrap();
        browser.wait("!!document.querySelector('#drawer [name=message]')").unwrap();
        browser.eval("document.querySelector('#drawer details').open=true;document.querySelector('#drawer [name=message]').focus();document.querySelector('#drawer [name=message]').value='Preserve this focused feedback'").unwrap();
        capture(&mut browser,&mut inventory,"drawer",directory);
        browser.send("Input.dispatchKeyEvent",json!({"type":"keyDown","key":"Escape","code":"Escape"})).unwrap();
        browser.wait("!document.documentElement.classList.contains('drawer-open')").unwrap();
        assert_eq!(browser.eval("drawerRequests.at(-1).signal.aborted").unwrap(),json!(true));
        assert_eq!(browser.eval("document.activeElement.dataset.step").unwrap(),json!("failed"));
        browser.viewport(390,"light").unwrap();
        browser.wait("document.querySelector('svg.edges').childElementCount===0").unwrap();
        navigate(&mut browser,&url,&format!("{board_base}/settings"));
        browser.wait("document.querySelector('[data-preview]').hidden===false").unwrap();
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision;document.querySelector('#project-description').focus();document.querySelector('#project-description').value='Unsaved settings draft'").unwrap();
        settings_apply(&mut browser,"name","renamed-acceptance");
        browser.wait("document.title.includes('renamed-acceptance')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#project-description').value").unwrap(),json!("Unsaved settings draft"));
        assert_eq!(browser.eval("location.pathname").unwrap(),json!(format!("{board_base}/settings")));
        assert_eq!(browser.eval("(async()=> (await fetch('/projects/board-fixture/settings')).status)()").unwrap(),json!(404));
        capture(&mut browser,&mut inventory,"renamed-settings",directory);
        browser.eval("document.querySelector('#project-name').value='Invalid Name';document.querySelector('form[data-setting=name]').requestSubmit()").unwrap();
        browser.wait("document.querySelector('#name-feedback').textContent.toLowerCase().includes('lowercase')").unwrap();
        assert!(browser.eval("document.querySelector('#name-feedback').textContent.length<500").unwrap().as_bool().unwrap());
        capture(&mut browser,&mut inventory,"settings-name-error",directory);
        browser.eval("document.querySelector('#project-name').value='renamed-acceptance';document.querySelector('#project-description').value='# Long markdown\\n\\n'+('A long paragraph with **bold** text.\\n\\n'.repeat(10))+'<script>unsafe</script>';document.querySelector('[data-preview]').click()").unwrap();
        browser.wait("document.querySelector('#description-preview').textContent.includes('Long markdown')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#description-preview script')===null").unwrap(),json!(true));
        capture(&mut browser,&mut inventory,"settings-long-markdown",directory);
        browser.eval("document.querySelector('#resource-cpu').value='-1';document.querySelector('#resource-cpu').form.requestSubmit()").unwrap();
        browser.wait("document.querySelector('#resource-cpu').form.querySelector('.settings-feedback').textContent.length>0").unwrap();
        capture(&mut browser,&mut inventory,"settings-resource-error",directory);
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision").unwrap();
        settings_apply(&mut browser,"name","a-long-project-name-for-the-phone-layout-and-its-immediate-selector-update");
        browser.wait("document.title.includes('a-long-project-name')").unwrap();
        capture(&mut browser,&mut inventory,"settings-long-name",directory);
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision").unwrap();
        settings_apply(&mut browser,"name","renamed-acceptance");
        browser.wait("document.title.includes('renamed-acceptance')").unwrap();
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision").unwrap();
        settings_apply(&mut browser,"paused","true");
        browser.wait("document.querySelector('#settings-live .tag')?.textContent==='Paused'").unwrap();
        capture(&mut browser,&mut inventory,"settings-paused",directory);
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision").unwrap();
        settings_apply(&mut browser,"archived","true");
        browser.wait("document.querySelector('#delete-guard').dataset.allowed==='true'").unwrap();
        capture(&mut browser,&mut inventory,"settings-archived",directory);
        assert_eq!(browser.eval("document.querySelector('#delete-button').disabled").unwrap(),json!(true));
        browser.eval("document.querySelector('#confirm-name').value='renamed-acceptance';document.querySelector('#confirm-name').dispatchEvent(new Event('input',{bubbles:true}))").unwrap();
        assert_eq!(browser.eval("document.querySelector('#delete-button').disabled").unwrap(),json!(false));
        capture(&mut browser,&mut inventory,"settings-delete-confirmed",directory);
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision").unwrap();
        settings_apply(&mut browser,"name","concurrently-renamed");
        browser.wait("document.querySelector('#confirm-name').value===''").unwrap();
        assert_eq!(browser.eval("document.querySelector('#delete-button').disabled").unwrap(),json!(true));
        assert_eq!(browser.eval("browserErrors").unwrap(),json!([]));
        inventory
    }).await.unwrap();
    let path = root().join("target/screens/p6-07/capture-manifest.json");
    std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "G1b/G6 native rename callbacks; record-at blocks the next message reply"]
async fn record_at_rename_with_active_run_preserves_callbacks_and_next() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let run = sluice_model::ids::RunId::new();
    let attempt = sluice_model::ids::AttemptId::new();
    let (id,cursor)=writer.write(RetrySafety::NonIdempotent, move |tx| {
        let project=sluice_store::projects::project_create(tx,sluice_store::projects::CreateProject {
            name:"callback-project".parse().unwrap(),description:"An active fixture run".into(),icon:None,resources:None,author:"gate".into()
        },&sluice_store::projects::EmptyPlanInitializer,&sluice_store::projects::NoResourceSettings)?;
        let id=project.project_id;
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,run_ids) VALUES (?1,'work',0,'{}','running',?2)",(id.to_string(),json!([run]).to_string()))?;
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'work','executing',?3,'fixture','now')",(attempt.to_string(),id.to_string(),json!({"declared":{"ready":"boolean"},"provenance":{"runtime":{"capability":"scratch-callback-capability"}}}).to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?1,?2,?3,'work','now')",(run.to_string(),id.to_string(),attempt.to_string()))?;
        tx.changed(Some(id),"status");
        let cursor=tx.sql().query_row("SELECT coalesce(max(seq),0) FROM records",[],|r|r.get::<_,i64>(0))?;
        Ok((id,cursor))
    }).await.unwrap();
    writer.shutdown().await.unwrap();
    let mut scratch = Scratch::new(home);
    scratch.boot().await;
    let url = scratch.url.clone();
    tokio::task::spawn_blocking(move || {
        let mut browser=Chrome::open(&format!("{url}/projects/id/{id}/settings")).unwrap();
        browser.wait("document.querySelector('[data-preview]')?.hidden===false").unwrap();
        let next=json!({"projects":[format!("id:{id}")],"since_seq":cursor,"timeout":10,"settle":0,"settle_max":0});
        let typed=sluice_web::mcp::decode_tool("next",next.as_object().unwrap().clone(),None).unwrap();
        let wire=serde_json::to_vec(&typed).unwrap();
        let _:sluice_model::commands::CommandRequest=sluice_model::rpc::decode_json(&wire).unwrap();
        browser.eval(&format!("window.nextResult=null;fetch('/api/tools/next',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({next})}}).then(r=>r.json()).then(r=>window.nextResult=r);void 0")).unwrap();
        browser.eval("window.previousRevision=document.querySelector('#settings-live').dataset.revision;document.querySelector('#project-description').focus();document.querySelector('#project-description').value='Draft during active run'").unwrap();
        settings_apply(&mut browser,"name","renamed-callback-project");
        browser.wait("document.title.includes('renamed-callback-project')").unwrap();
        assert_eq!(browser.eval("document.querySelector('#project-description').value").unwrap(),json!("Draft during active run"));
        let args=json!({"project":"renamed-callback-project","step":"work","run":run,"outputs":{"ready":true}});
        let reply=browser.eval(&format!("(async()=>{{const r=await fetch('/api/tools/step_submit',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({args})}});return {{status:r.status,body:await r.json()}}}})()")).unwrap();
        assert_eq!(reply["status"],json!(200),"callback {reply}");
        let message=json!({"project":"renamed-callback-project","thread":"step-work","from":"work","to":"orchestrator","body":"Callback survived browser rename","needs_reply":true,"run":run});
        let posted=browser.eval(&format!("(async()=>{{const r=await fetch('/api/tools/message_post',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({message})}});return {{status:r.status,body:await r.json()}}}})()")).unwrap();
        assert_eq!(posted["status"],json!(200),"{posted}");
        browser.wait("window.nextResult!==null").unwrap();
        let next=browser.eval("JSON.stringify(window.nextResult)").unwrap();
        assert!(next.as_str().unwrap().contains("Callback survived browser rename"),"{next}");

        assert!(reply.to_string().contains("true"),"{reply}");
        assert_eq!(browser.eval("document.querySelector('#delete-guard').dataset.allowed").unwrap(),json!("false"));
    }).await.unwrap();
    scratch.stop(1);
    scratch.http().await;
    let submit=json!({"command":"step_submit","args":{"project":id,"step":"work","run":run,"outputs":{"ready":true},"author":"gate"}}).to_string();
    let reply = scratch.cli(&["tool", "rpc", &submit]);
    assert_eq!(reply["reply"], json!("ack"), "{reply}");
    let reads = sluice_store::ReadPool::open(scratch.home.path(), 1).unwrap();
    assert_eq!(
        reads
            .snapshot(move |c| Ok(c.query_row(
                "SELECT version FROM submissions WHERE run_id=?1",
                [run.to_string()],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "native deletion gate requires coordinator ProjectDelete dispatch"]
async fn native_delete_removes_selector_and_closes_stream() {
    let mut scratch = Scratch::new(tempfile::tempdir().unwrap());
    scratch.boot().await;
    let created=scratch.cli(&["tool","rpc",r#"{"command":"project_create","args":{"name":"delete-acceptance","description":"Disposable deletion gate","icon":null,"resources":{},"author":"gate"}}"#]);
    let sluice_model::commands::CommandReply::Project(project) =
        serde_json::from_value(created).unwrap()
    else {
        panic!("identity")
    };
    let id = project.project_id;
    scratch.cli(&["tool","rpc",&json!({"command":"project_update","args":{"project":{"kind":"id","value":id},"archived":true,"author":"gate"}}).to_string()]);
    let url = scratch.url.clone();
    tokio::task::spawn_blocking(move || {
        let mut browser=Chrome::open(&format!("{url}/projects/id/{id}/settings")).unwrap();
        browser.wait("document.querySelector('#delete-guard')?.dataset.allowed==='true'").unwrap();
        browser.eval("document.querySelector('#confirm-name').value='delete-acceptance';document.querySelector('#confirm-name').dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('#delete-form').requestSubmit()").unwrap();
        browser.wait("location.pathname==='/'").unwrap();
        assert_eq!(browser.eval("document.querySelector('.switcher').textContent.includes('delete-acceptance')").unwrap(),json!(false));
        assert_eq!(browser.eval(&format!("(async()=> (await fetch('/projects/id/{id}/settings')).status)()")).unwrap(),json!(404));
    }).await.unwrap();
}
