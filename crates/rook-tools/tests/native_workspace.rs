#![cfg(unix)]
use rook_tools::{
    Tool, ToolBox, ToolContext,
    environments::{Context, Profile, Settings, native::NativeWorkspace},
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, sync::Arc};
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    ONE.lock().await
}
fn context(dir: &std::path::Path, remote: &std::path::Path) -> ToolContext {
    let program = dir.join("ssh-fixture");
    std::fs::write(&program, "#!/bin/sh\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let settings = Settings {
        native_workspace: true,
        selected: "native".into(),
        profiles: std::collections::BTreeMap::from([(
            "native".into(),
            Profile::Ssh {
                host: "trusted-fixture".into(),
                program: program.to_string_lossy().into(),
                remote_workspace: remote.to_string_lossy().into(),
            },
        )]),
        ..Default::default()
    };
    assert!(settings.errors().is_empty());
    let mut ctx = ToolContext::new(dir.into());
    ctx.execution =
        Some(Arc::new(Context { settings, pool: Default::default(), state_root: dir.join("state") }));
    ctx
}
#[tokio::test]
async fn files_come_from_the_selected_remote_root_and_never_a_same_named_local_replica() {
    let _one = one_at_a_time().await;
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let ctx = context(local.path(), remote.path());
    std::fs::write(local.path().join("a.txt"), "WRONG_LOCAL_SOURCE").unwrap();
    std::fs::write(remote.path().join("a.txt"), "REMOTE_SOURCE\nneedle\n").unwrap();
    let read = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"a.txt"})).await.unwrap();
    assert!(!read.is_error, "{}", read.content);
    let value: Value = serde_json::from_str(&read.content).unwrap();
    assert_eq!(value["response"]["result"]["text"], "REMOTE_SOURCE\nneedle\n");
    assert_eq!(value["workspace"]["root"], remote.path().to_string_lossy().as_ref());
    assert!(!read.content.contains("WRONG_LOCAL_SOURCE"));
    let list = NativeWorkspace.call(&ctx, &json!({"operation":"list"})).await.unwrap();
    assert!(list.content.contains("a.txt"));
    let search = NativeWorkspace.call(&ctx, &json!({"operation":"search","pattern":"needle"})).await.unwrap();
    assert!(!search.is_error, "{}", search.content);
    let found: Value = serde_json::from_str(&search.content).unwrap();
    assert_eq!(found["response"]["result"]["matches"][0]["line"], 2);
    assert!(
        ToolBox::standard()
            .call(&ctx, "read_file", &json!({"path":"a.txt"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("remote filesystem authority")
    );
    assert_eq!(std::fs::read_to_string(local.path().join("a.txt")).unwrap(), "WRONG_LOCAL_SOURCE");
}
#[tokio::test]
async fn symlink_escape_and_reached_source_limits_are_refused_without_returning_outside_text() {
    let _one = one_at_a_time().await;
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut ctx = context(local.path(), remote.path());
    std::fs::write(outside.path().join("private"), "OUTSIDE_PRIVATE_SOURCE").unwrap();
    std::os::unix::fs::symlink(outside.path(), remote.path().join("link")).unwrap();
    let reply = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"link/private"})).await.unwrap();
    assert!(reply.is_error);
    assert!(!reply.content.contains("OUTSIDE_PRIVATE_SOURCE"));
    ctx.execution = Some(Arc::new(Context {
        settings: Settings { file_bytes: 1024, ..ctx.execution.as_ref().unwrap().settings.clone() },
        pool: Default::default(),
        state_root: local.path().join("state"),
    }));
    let bytes = vec![b'x'; 1025];
    assert!(bytes.len() > 1024);
    std::fs::write(remote.path().join("large"), bytes).unwrap();
    let reply = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"large"})).await.unwrap();
    assert!(reply.is_error);
    assert!(reply.content.contains("file_bytes"));
}

#[tokio::test]
async fn native_edits_have_owned_receipts_and_undo_refuses_external_changes() {
    let _one = one_at_a_time().await;
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let mut ctx = context(local.path(), remote.path());
    ctx.session = Some(1);
    std::fs::write(remote.path().join("a.txt"), "before\n").unwrap();
    let edited = NativeWorkspace
        .call(&ctx, &json!({"operation":"edit","path":"a.txt","old":"before","new":"after"}))
        .await
        .unwrap();
    assert!(!edited.is_error, "{}", edited.content);
    assert_eq!(std::fs::read_to_string(remote.path().join("a.txt")).unwrap(), "after\n");
    let value: Value = serde_json::from_str(&edited.content).unwrap();
    let id = value["receipt"]["call"].as_str().unwrap();
    assert_eq!(value["receipt"]["status"], "applied");
    assert!(value["receipt"].get("before").is_none());
    let mut foreign = ctx.clone();
    foreign.session = Some(2);
    assert!(
        NativeWorkspace
            .call(&foreign, &json!({"operation":"undo","id":id}))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not belong")
    );
    std::fs::write(remote.path().join("a.txt"), "external\n").unwrap();
    assert!(NativeWorkspace.call(&ctx, &json!({"operation":"undo","id":id})).await.is_err());
    assert_eq!(std::fs::read_to_string(remote.path().join("a.txt")).unwrap(), "external\n");
    std::fs::write(remote.path().join("a.txt"), "after\n").unwrap();
    let undone = NativeWorkspace.call(&ctx, &json!({"operation":"undo","id":id})).await.unwrap();
    assert!(undone.content.contains("undone"));
    assert_eq!(std::fs::read_to_string(remote.path().join("a.txt")).unwrap(), "before\n");
    let listing = NativeWorkspace.call(&ctx, &json!({"operation":"checkpoint"})).await.unwrap();
    assert!(listing.content.contains(id));
}

#[tokio::test]
async fn a_read_only_checker_or_missing_session_cannot_prepare_remote_mutations() {
    let _one = one_at_a_time().await;
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let mut ctx = context(local.path(), remote.path());
    let args = json!({"operation":"write","path":"new.txt","content":"new"});
    assert!(
        NativeWorkspace.call(&ctx, &args).await.unwrap_err().to_string().contains("engine-owned session")
    );
    ctx.session = Some(3);
    ctx.read_only = true;
    assert!(
        NativeWorkspace.call(&ctx, &args).await.unwrap_err().to_string().contains("checker cannot change")
    );
    assert!(!remote.path().join("new.txt").exists());
    ctx.read_only = false;
    let written = NativeWorkspace.call(&ctx, &args).await.unwrap();
    let value: Value = serde_json::from_str(&written.content).unwrap();
    assert_eq!(value["receipt"]["before_sha256"], Value::Null);
    let id = &value["receipt"]["call"];
    NativeWorkspace.call(&ctx, &json!({"operation":"undo","id":id})).await.unwrap();
    assert!(!remote.path().join("new.txt").exists());
}

struct Live {
    host: String,
    base: String,
    marker: String,
    owner: String,
    identity: Option<String>,
}
impl Live {
    fn python(&self, code: &str, data: Value) -> std::process::Output {
        use std::io::Write;
        let quoted = format!("'{}'", code.replace('\'', "'\\''"));
        let mut child = std::process::Command::new("ssh")
            .args([
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                &self.host,
                &format!("python3 -c {quoted}"),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(data.to_string().as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
    fn new(host: String, owner: String) -> Self {
        let marker = ulid::Ulid::generate().to_string();
        let base = format!("/tmp/rook-native-live-{marker}");
        let fixture = Self { host, base, marker, owner, identity: None };
        let out=fixture.python("import sys,json,os\nc=json.load(sys.stdin)\nassert c['base']=='/tmp/rook-native-live-'+c['marker']\nos.mkdir(c['base'],0o700)\nopen(c['base']+'/.fixture-owner','w').write(c['marker'])\nopen(c['base']+'/evidence.txt','w').write('before\\n')\n",json!({"base":fixture.base,"marker":fixture.marker}));
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        fixture
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        let code = "import sys,json,os,hashlib,shutil\nc=json.load(sys.stdin)\nb=c['base']\nif b!='/tmp/rook-native-live-'+c['marker'] or os.path.islink(b):raise SystemExit(1)\nif open(b+'/.fixture-owner').read()!=c['marker']:raise SystemExit(1)\ni=os.stat(b)\np=hashlib.sha256((str(i.st_dev)+':'+str(i.st_ino)).encode()).hexdigest()\nif c.get('identity'):\n d='/tmp/.rook-native-'+str(os.getuid())+'/'+p+'/'+c['identity']+'/'+c['owner']\n if os.path.isdir(d):\n  for f in os.scandir(d):\n   if f.name.endswith('.json'):\n    r=json.load(open(f.path))\n    if r.get('status')=='running' and not r.get('execution',{}).get('quiet'):raise SystemExit(2)\n  shutil.rmtree(d)\nshutil.rmtree(b)\n";
        let out = self.python(
            code,
            json!({"base":self.base,"marker":self.marker,"owner":self.owner,"identity":self.identity}),
        );
        if !out.status.success() {
            eprintln!("retained owned SSH fixture {} for inspection", self.base);
        }
    }
}
struct FixtureSecret;
impl rook_tools::Secrets for FixtureSecret {
    fn value(&self, name: &str) -> Option<String> {
        (name == "fixture_key").then(|| "synthetic\"private\nvalue".into())
    }
    fn redactions(&self) -> Vec<String> {
        vec!["synthetic\"private\nvalue".into()]
    }
}

#[tokio::test]
#[ignore = "requires an explicitly configured trusted Linux SSH fixture"]
async fn actual_ssh_native_execution_retains_file_effects_and_undo_without_any_local_replica() {
    let _one = one_at_a_time().await;
    let host = std::env::var("ROOK_TEST_SSH_HOST")
        .expect("set the existing trusted host alias; no passwords in test arguments");
    let local = tempfile::tempdir().unwrap();
    let session = ulid::Ulid::generate();
    let mut live = Live::new(host.clone(), session.to_string());
    let mut ctx = ToolContext::new(local.path().into());
    ctx.session = Some(session.into());
    ctx.secrets = Some(Arc::new(FixtureSecret));
    let settings = Settings {
        native_workspace: true,
        selected: "native".into(),
        profiles: std::collections::BTreeMap::from([(
            "native".into(),
            Profile::Ssh { host, program: "ssh".into(), remote_workspace: live.base.clone() },
        )]),
        ..Default::default()
    };
    ctx.execution = Some(Arc::new(Context {
        settings,
        pool: Default::default(),
        state_root: local.path().join("state"),
    }));
    let read = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"evidence.txt"})).await.unwrap();
    let read: Value = serde_json::from_str(&read.content).unwrap();
    live.identity = Some(read["workspace"]["identity"].as_str().unwrap().into());
    let args = json!({"operation":"run","command":"printf '%s' \"$ROOK_SECRET_FIXTURE_KEY\"; printf changed > evidence.txt; mkdir generated; printf new > generated/new.txt; ln -s ../evidence.txt generated/link; exit 7","secrets":["fixture_key"]});
    let run = NativeWorkspace.call(&ctx, &args).await.unwrap();
    assert!(run.is_error, "exit 7 is a failed command, with retained effects");
    assert!(!run.content.contains("synthetic"), "{}", run.content);
    let run: Value = serde_json::from_str(&run.content).unwrap();
    assert_eq!(run["observation"]["execution"]["code"], 7);
    assert_eq!(run["observation"]["execution"]["quiet"], true);
    assert_eq!(run["observation"]["execution"]["stdout"], "[redacted]");
    let id = run["receipt"]["call"].as_str().unwrap();
    let inspected = NativeWorkspace.call(&ctx, &json!({"operation":"checkpoint","id":id})).await.unwrap();
    assert!(!inspected.is_error);
    assert!(!inspected.content.contains("synthetic"));
    assert!(!local.path().join("evidence.txt").exists());
    assert!(!local.path().join("generated").exists());
    let changed =
        NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"evidence.txt"})).await.unwrap();
    assert!(changed.content.contains("changed"));
    NativeWorkspace.call(&ctx, &json!({"operation":"undo","id":id})).await.unwrap();
    let restored =
        NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"evidence.txt"})).await.unwrap();
    assert!(restored.content.contains("before"));
    let dirs = NativeWorkspace.call(&ctx, &json!({"operation":"list"})).await.unwrap();
    assert!(!dirs.content.contains("generated"));
    assert!(
        !rook_tools::environments::native::has_unrestored(
            &ctx.execution.as_ref().unwrap().state_root,
            session.into()
        )
        .unwrap()
    );
}

#[tokio::test]
#[ignore = "requires an explicitly configured trusted Linux SSH fixture"]
async fn disconnect_stops_only_the_owned_remote_command_and_never_replays_it_before_undo() {
    let _one = one_at_a_time().await;
    let host = std::env::var("ROOK_TEST_SSH_HOST").expect("set a trusted existing SSH host");
    let local = tempfile::tempdir().unwrap();
    let session = ulid::Ulid::generate();
    let mut live = Live::new(host.clone(), session.to_string());
    let mut ctx = ToolContext::new(local.path().into());
    ctx.session = Some(session.into());
    ctx.command_timeout = std::time::Duration::from_secs(15);
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            native_workspace: true,
            selected: "native".into(),
            profiles: std::collections::BTreeMap::from([(
                "native".into(),
                Profile::Ssh { host, program: "ssh".into(), remote_workspace: live.base.clone() },
            )]),
            ..Default::default()
        },
        pool: Default::default(),
        state_root: local.path().join("state"),
    }));
    let read = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"evidence.txt"})).await.unwrap();
    let read: Value = serde_json::from_str(&read.content).unwrap();
    live.identity = Some(read["workspace"]["identity"].as_str().unwrap().into());
    let run = tokio::spawn({
        let ctx = ctx.clone();
        async move {
            NativeWorkspace.call(&ctx,&json!({"operation":"run","command":"sleep 30 & echo $! > owned.pid; printf ready > ready; wait"})).await
        }
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let ready = NativeWorkspace.call(&ctx, &json!({"operation":"read","path":"ready"})).await.unwrap();
        if !ready.is_error && ready.content.contains("ready") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "remote command did not start");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    assert!(
        NativeWorkspace
            .call(&ctx, &json!({"operation":"write","path":"must-not-exist","content":"unexpected"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("unresolved")
    );
    let listing = NativeWorkspace.call(&ctx, &json!({"operation":"checkpoint"})).await.unwrap();
    let listing: Value = serde_json::from_str(&listing.content).unwrap();
    let entries = listing["receipts"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "no replay or replacement call");
    let id = entries[0]["call"].as_str().unwrap();
    let observed = NativeWorkspace.call(&ctx, &json!({"operation":"checkpoint","id":id})).await.unwrap();
    let observed: Value = serde_json::from_str(&observed.content).unwrap();
    assert_eq!(observed["observation"]["execution"]["quiet"], true);
    assert_eq!(observed["observation"]["execution"]["interrupted"], true);
    NativeWorkspace.call(&ctx, &json!({"operation":"undo","id":id})).await.unwrap();
    let files = NativeWorkspace.call(&ctx, &json!({"operation":"list"})).await.unwrap();
    assert!(!files.content.contains("owned.pid") && !files.content.contains("must-not-exist"));
}
