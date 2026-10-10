//! Real Chromium gate. The fixture has a scratch store and no runner. The CDP
//! pipe client is the existing read-only Python helper while p6-07 owns Rust CDP.
use sluice_model::{
    error::PublicError,
    ids::ProjectId,
    plan::{FnSignature, SignatureProvider},
    types::Type,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings, UpdateProject},
};
use sluice_web::{
    settings::{SettingsState, StoreCommands},
    views::{DashboardState, EmptyCatalog, dashboard_router},
};
use std::sync::Arc;
struct Resources;
impl SignatureProvider for Resources {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        (name == "fixture.capacity").then(|| FnSignature {
            outputs: [("capacity".into(), Type::Int)].into_iter().collect(),
            ..Default::default()
        })
    }
}
impl projects::ResourceSettings for Resources {
    fn set_resources(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        id: ProjectId,
        patch: &serde_json::Value,
    ) -> sluice_store::Result<bool> {
        sluice_store::resources::patch_resources(tx, id, patch, self)
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn chromium_settings_commands_streams_geometry_and_screenshots() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "browser-project".parse().unwrap(),
                    description: "# Project description\n\nA calm board for supervising agents."
                        .into(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await
        .unwrap();
    let id = project.project_id;
    writer.write(RetrySafety::NonIdempotent,move |tx| {projects::project_update(tx,&sluice_model::ids::ProjectSelector::Id(id),UpdateProject{resources:Some(serde_json::json!({"workers":3,"dynamic":{"capacity_fn":"fixture.capacity"}})),author:"owner".into(),..Default::default()},&Resources)?;sluice_store::resources::observe_capacity(tx,id,"dynamic",1,Err(PublicError::FnFailure{message:"Capacity service is unavailable. Last good capacity retained.".into()}))?;Ok(())}).await.unwrap();
    let run = sluice_model::ids::RunId::new();
    let attempt = sluice_model::ids::AttemptId::new();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,generation,work_generation,run_ids) VALUES (?1,'work',0,'{}','running',1,1,?2)",(id.to_string(),serde_json::json!([run]).to_string()))?;
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,'work',1,1,'executing',?3,'hash','now')",(attempt.to_string(),id.to_string(),serde_json::json!({"declared":{"result":"string"}}).to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at) VALUES (?1,?2,?3,'work',1,1,'now')",(run.to_string(),id.to_string(),attempt.to_string()))?;
        tx.changed(Some(id),"status"); Ok(())
    }).await.unwrap();
    let dashboard = DashboardState::new(
        ReadPool::open(home.path(), 2).unwrap(),
        Arc::new(EmptyCatalog),
    );
    let guard = Arc::new(projects::StoredWorkOnly);
    let commands = Arc::new(StoreCommands {
        writer: writer.clone(),
        resources: Arc::new(Resources),
        deletion_guard: guard.clone(),
    });
    let state = SettingsState::new(dashboard.clone(), commands, guard);
    let next_writer = writer.clone();
    let next_reads = dashboard.reads.clone();
    let waiter = tokio::spawn(async move {
        sluice_runtime::watch::next(
            &next_writer,
            &next_reads,
            sluice_runtime::watch::NextOptions {
                projects: vec![id],
                timeout: Some(std::time::Duration::from_secs(30)),
                settle: std::time::Duration::ZERO,
                ..Default::default()
            },
        )
        .await
    });
    let callback_writer = writer.clone();
    let callback_reads = dashboard.reads.clone();
    let callback = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let renamed = callback_reads.snapshot(move |c| Ok(projects::resolve(c,&sluice_model::ids::ProjectSelector::Id(id))?.name.as_str()=="browser-project-renamed")).await.unwrap();
                if renamed { break; }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            callback_writer.write(RetrySafety::NonIdempotent,move |tx| {
                let submission = serde_json::from_value(serde_json::json!({"project":id,"step":"work","run":run,"outputs":{"result":"callback after browser rename"},"author":"worker"}))?;
                assert_eq!(sluice_store::attempts::step_submit(tx,submission)?,Some(1));
                let message: sluice_model::commands::Ask = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":id},"to":"orchestrator","body":"callback after browser rename","run":run}))?;
                sluice_store::messages::post(tx,message.try_into()?,&sluice_store::messages::NoPlanInputs)?;
                tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=?1",[attempt.to_string()])?;
                tx.sql().execute("UPDATE runs SET finished_at='done' WHERE run_id=?1",[run.to_string()])?;
                tx.sql().execute("UPDATE steps SET status='succeeded' WHERE project_id=?1",[id.to_string()])?;
                tx.changed(Some(id),"status"); Ok(())
            }).await.unwrap();
        }).await.unwrap();
    });
    let router = dashboard_router(dashboard).merge(sluice_web::settings::router(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 3065);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("python3")
            .arg("-c")
            .arg(BROWSER)
            .arg(&root)
            .arg(format!("http://{addr}"))
            .arg(id.to_string())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    callback.await.unwrap();
    let next = waiter.await.unwrap().unwrap();
    assert!(!next.timed_out);
    assert!(
        serde_json::to_string(&next)
            .unwrap()
            .contains("callback after browser rename")
    );
    server.abort();
    let _ = server.await;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    writer.shutdown().await.unwrap();
}
const BROWSER: &str = r##"
import base64, json, sys
from pathlib import Path
sys.path.insert(0,str(Path(sys.argv[1])/'tests'))
from browser import Chrome, find_chrome
root, url, project = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
assert ':3065' not in url
exe=find_chrome()
assert exe, 'Chromium is required for the settings browser gate'
chrome=Chrome(exe)
screens=root/'target/screens/p6-06'
screens.mkdir(parents=True,exist_ok=True)
path='/projects/id/'+project+'/settings'
geometry=[]
try:
 chrome.open(url+path)
 chrome.wait("document.querySelector('#project-name') && document.querySelector('[data-preview]').hidden === false")
 def capture(name,width,theme):
  chrome.send('Emulation.setDeviceMetricsOverride',{'width':width,'height':1000,'deviceScaleFactor':1,'mobile':False})
  chrome.send('Emulation.setEmulatedMedia',{'features':[{'name':'prefers-color-scheme','value':theme}]})
  chrome.eval("document.documentElement.removeAttribute('data-appearance')")
  chrome.eval('document.fonts.ready')
  chrome.eval('window.scrollTo(0,0);new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))')
  g=chrome.eval("(()=>{const content=document.querySelector('.settings-page, #projects, #functions').getBoundingClientRect(),nav=document.querySelector('#top-nav .switcher').getBoundingClientRect(),cog=document.querySelector('.project-settings')?.getBoundingClientRect();return {scroll:document.documentElement.scrollWidth,width:document.documentElement.clientWidth,left:content.left,nav:document.querySelector('#top-nav').getBoundingClientRect().left+parseFloat(getComputedStyle(document.querySelector('#top-nav')).paddingLeft),column:content.width,height:document.documentElement.scrollHeight,cog:cog&&{width:cog.width,height:cog.height}}})()")
  assert g['scroll']<=g['width'],(name,width,g)
  assert abs(g['left']-g['nav'])<1,(name,g)
  assert abs(g['left']-(g['width']-g['column'])/2)<1,(name,g)
  if g.get('cog'): assert g['cog']['width']>=44 and g['cog']['height']>=44,g
  geometry.append({'state':name,'viewport':width,'theme':theme,**g})
  shot=chrome.send('Page.captureScreenshot',{'format':'png','captureBeyondViewport':True,'clip':{'x':0,'y':0,'width':width,'height':g['height'],'scale':1}})['data']
  (screens/f'rust-{name}-{width}-{theme}.png').write_bytes(base64.b64decode(shot))
 def matrix(name):
  for width in (390,1440,2560):
   for theme in ('light','dark'): capture(name,width,theme)
 def states(name):
  for theme in ('light','dark'): capture(name,390,theme)
 def apply(field,value,resource=''):
  script="""(async()=>{const r=await fetch(location.pathname,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({field:%s,value:%s,resource:%s,expected_settings_rev:Number(document.querySelector('#settings-live').dataset.revision)})});return {status:r.status,text:await r.text()}})()"""%(json.dumps(field),json.dumps(value),json.dumps(resource))
  result=chrome.eval(script)
  assert result['status']==200,result
  return result
 matrix('settings')
 chrome.send('Page.navigate',{'url':url+'/'})
 chrome.wait("document.querySelector('#content') && document.title.includes('Projects')")
 matrix('home')
 chrome.send('Page.navigate',{'url':url+'/fns?project='+project})
 chrome.wait("document.querySelector('#content') && document.title.includes('Functions')")
 matrix('functions')
 chrome.send('Page.navigate',{'url':url+path})
 chrome.wait("document.querySelector('#project-name') && document.querySelector('[data-preview]').hidden === false")
 chrome.eval("window.requests=[]; const realFetch=window.fetch; window.fetch=(input,opts)=>{requests.push(String(input));return realFetch(input,opts)}; window.errors=[];addEventListener('error',e=>errors.push(e.message));addEventListener('unhandledrejection',e=>errors.push(String(e.reason)))")
 assert chrome.eval("document.querySelector('#delete-button').getAttribute('aria-disabled')==='true'")
 assert chrome.eval("document.querySelector('#delete-explanation').textContent.includes('running steps')")
 assert chrome.eval("document.querySelector('#delete-explanation a').textContent==='Open the plan'")
 assert chrome.eval("!document.querySelector('#confirm-name')")
 # No keystroke writes; retain a focused unsaved description across rename.
 chrome.eval("document.querySelector('#project-description').focus();document.querySelector('#project-description').value='Unsaved draft';document.querySelector('#project-description').dispatchEvent(new Event('input',{bubbles:true}))")
 assert not chrome.eval("requests.some(r=>r.endsWith('/settings'))")
 chrome.eval("document.querySelector('#project-name').value='browser-project-renamed';document.querySelector('form[data-setting=name]').requestSubmit()")
 chrome.wait("document.querySelector('#settings-live').dataset.name==='browser-project-renamed' && document.querySelector('#name-feedback').textContent==='Saved'")
 chrome.wait("document.title==='browser-project-renamed · Settings · sluice'")
 assert chrome.eval("document.querySelector('.sw-name').textContent==='browser-project-renamed'")
 assert chrome.eval("location.pathname")==path
 assert chrome.eval("document.querySelector('#project-description').value==='Unsaved draft'")
 assert chrome.eval("(async()=> (await fetch('/projects/browser-project/settings')).status)()")==404
 # Duplicate label is a conflict with no partial change, and input stays available.
 chrome.eval("document.querySelector('#project-name').value='Invalid Name';document.querySelector('form[data-setting=name]').requestSubmit()")
 chrome.wait("document.querySelector('#name-feedback').textContent.includes('Lowercase') || document.querySelector('#name-feedback').textContent.includes('lowercase')")
 assert chrome.eval("document.querySelector('#project-name').value==='Invalid Name'")
 states('name-error')
 long='a-project-with-a-name-that-is-long-enough-to-wrap-on-a-small-phone-'*3
 apply('name',long)
 chrome.wait('document.querySelector("#settings-live").dataset.name==='+json.dumps(long))
 chrome.eval('document.querySelector("#project-name").value='+json.dumps(long))
 states('long-name')
 markdown='# Long markdown\n\n'+('A paragraph with **bold text** and a [safe link](https://example.org).\n\n'*15)+'```\n'+'long_code_'*70+'\n```\n\n<script>throw new Error("unsafe")</script>'
 chrome.eval('document.querySelector("#project-description").value='+json.dumps(markdown))
 chrome.eval("document.querySelector('[data-preview]').click()")
 chrome.wait("document.querySelector('#description-preview').textContent.includes('Long markdown')")
 assert chrome.eval("document.querySelector('#description-preview script')===null")
 states('long-markdown')
 # Restore compact content so later states expose controls.
 chrome.eval("document.querySelector('#project-description').value='A calm description';document.querySelector('[data-preview]').click()")
 chrome.wait("document.querySelector('#description-preview').textContent==='A calm description\\n'")
 # a capacity is a whole number of 0 or more: the field refuses -1 before anything is sent
 chrome.eval("window.requests=[]; document.querySelector('#resource-workers').value='-1';document.querySelector('#resource-workers').form.requestSubmit()")
 assert chrome.eval("document.querySelector('#resource-workers').validity.rangeUnderflow")
 assert not chrome.eval("requests.some(r=>r.endsWith('/settings'))")
 assert chrome.eval("document.querySelector('#resource-workers').value==='-1'")
 states('resource-error')
 apply('paused','true')
 chrome.wait("document.querySelector('#settings-live .tag')?.textContent==='Paused'")
 states('paused')
 # Delete a live, unarchived project after its work finishes.
 chrome.wait("document.querySelector('#delete-button').getAttribute('aria-disabled')==='false'")
 assert chrome.eval("document.querySelector('#delete-button').getBoundingClientRect().height>=32")
 chrome.eval("document.querySelector('#delete-button').click()")
 chrome.wait("document.querySelector('#confirmation').open")
 assert chrome.eval("document.activeElement.matches('[data-keep]')")
 states('delete-dialog')
 chrome.send('Input.dispatchKeyEvent',{'type':'keyDown','key':'Tab','code':'Tab','windowsVirtualKeyCode':9})
 chrome.send('Input.dispatchKeyEvent',{'type':'keyUp','key':'Tab','code':'Tab','windowsVirtualKeyCode':9})
 assert chrome.eval("document.querySelector('#confirmation').contains(document.activeElement)")

 chrome.send('Input.dispatchKeyEvent',{'type':'keyDown','key':'Escape','code':'Escape','windowsVirtualKeyCode':27})
 chrome.send('Input.dispatchKeyEvent',{'type':'keyUp','key':'Escape','code':'Escape','windowsVirtualKeyCode':27})
 chrome.wait("!document.querySelector('#confirmation').open")
 chrome.wait("document.activeElement.id==='delete-button'")
 chrome.eval("document.querySelector('#delete-button').click()")
 # A concurrent rename closes the outdated confirmation.
 apply('name','final-name')
 chrome.wait("document.querySelector('#settings-live').dataset.name==='final-name' && !document.querySelector('#confirmation').open")
 assert chrome.eval("document.querySelector('#delete-button').getAttribute('aria-disabled')==='false'")
 assert chrome.eval("errors")==[],chrome.eval('errors')
 chrome.eval("document.querySelector('#delete-button').click()")
 chrome.wait("document.querySelector('#confirmation').open")
 assert chrome.eval("document.querySelector('#confirmation .danger').textContent==='Delete final-name'")
 chrome.eval("document.querySelector('#confirmation .danger').click()")
 chrome.wait("location.pathname==='/'")
 assert not chrome.eval('document.querySelector(".switcher").textContent.includes("final-name")')
 (screens/'geometry.json').write_text(json.dumps(geometry,indent=2))
finally:
 chrome.close()
"##;
