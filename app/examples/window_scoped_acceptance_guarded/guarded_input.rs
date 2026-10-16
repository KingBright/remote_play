//! One explicit synthetic window, existing production V2, no OS permission requests.
//! Cleanup is outside the cancellable operation and confirms each owned resource.
use protocol::{InputEvent, session::{CaptureSource, CaptureSourceInfo, SessionCommand, SubscriptionRequest}};
use remote_core::{SharedHostStats, Statistics, workspace_session::{WorkspaceConnection, WorkspaceEvents}};
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, sync::{Arc, atomic::Ordering::Relaxed}, time::{Duration, Instant}};
use tokio::sync::watch;
#[path = "portal_receiver.rs"]
mod portal_receiver;
#[path = "hevc_diagnostic.rs"]
mod hevc_diagnostic;

fn new_private_receipt(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    options.open(path)
}
fn existing_profile() -> Result<PathBuf> {
    let profile=if let Some(value)=std::env::var_os("REMOTE_PLAY_DEVICE_GROUP_DIR") {PathBuf::from(value)}
    else if cfg!(target_os="windows") {PathBuf::from(std::env::var_os("APPDATA").ok_or("appdata unavailable")?).join("RemotePlay/Mesh")}
    else if cfg!(target_os="macos") {PathBuf::from(std::env::var_os("HOME").ok_or("home unavailable")?).join("Library/Application Support/RemotePlay/NativeMesh")}
    else {PathBuf::from(std::env::var_os("HOME").ok_or("home unavailable")?).join(".config/remote-play/mesh")};
    if !profile.is_absolute() || !profile.is_dir() {return Err("existing absolute paired profile required".into());}
    Ok(profile)
}

const ID: u32 = 256;
const LIST: u32 = 900_120;
const TITLE: &str = "RemotePlay scoped-window acceptance";
const STOPPED: &str = "all capture and connection workers stopped";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
struct Target { pid: i32, window: u32, nonce: String }
impl Target {
    fn validate(self) -> Result<Self> {
        if self.pid <= 0 || self.window == 0 || self.nonce.len()!=32 || !self.nonce.bytes().all(|c|c.is_ascii_hexdigit()) { return Err("positive synthetic PID and Window ID required".into()); }
        Ok(self)
    }
    fn select(&self, sources: &[CaptureSourceInfo]) -> Result<CaptureSourceInfo> {
        let mut exact = sources.iter().filter(|s| s.source == CaptureSource::Window(self.window)
            && s.process_id == Some(self.pid) && s.title == format!("{TITLE} {}",self.nonce));
        let selected = exact.next().ok_or("exact synthetic Window/PID/title not found")?;
        if exact.next().is_some() || selected.width < 200 || selected.height < 150 {
            return Err("ambiguous or too-small synthetic window".into());
        }
        Ok(selected.clone())
    }
}

#[derive(Clone, Copy)]
struct Budgets { operation: Duration, acknowledgement: Duration, pause: Duration }
impl Default for Budgets {
    fn default() -> Self { Self { operation: Duration::from_secs(28), acknowledgement: Duration::from_secs(8), pause: Duration::from_millis(140) } }
}

trait Link {
    fn connection_id(&self) -> u32;
    async fn send(&mut self, command: SessionCommand) -> Result<()>;
    async fn subscribe(&mut self, source: CaptureSource) -> Result<()>;
    async fn next(&mut self, wait: Duration) -> Option<SessionCommand>;
    fn video(&self) -> (u64, u64, u64);
    fn reset_video(&mut self) {}
    fn export_frame(&self, _path: &std::path::Path) -> Result<bool> { Ok(false) }
}

struct Wire {
    conn: WorkspaceConnection,
    events: WorkspaceEvents,
    stats: Arc<Statistics>,
    media: Option<client::ClientMediaRuntime>,
}
impl Link for Wire {
    fn connection_id(&self) -> u32 { self.conn.id }
    async fn send(&mut self, command: SessionCommand) -> Result<()> { self.conn.control(command).await }
    async fn subscribe(&mut self, source: CaptureSource) -> Result<()> {
        let media = client::ClientMediaRuntime::start_video_only(self.stats.clone()).map_err(|e|e.to_string())?;
        self.conn.subscribe(SubscriptionRequest { id: ID, source, width:640, height:360, fps:10, bitrate_kbps:1000, audio:false },
            media.decode_tx.clone(), self.stats.clone(), Arc::new(SharedHostStats::default())).await?;
        self.media=Some(media); Ok(())
    }
    async fn next(&mut self, wait: Duration) -> Option<SessionCommand> {
        tokio::time::timeout(wait, self.events.control.recv()).await.ok().flatten()
    }
    fn video(&self) -> (u64,u64,u64) {
        (self.stats.video_frames_decoded.load(Relaxed) as u64, self.stats.video_decode_errors.load(Relaxed) as u64, self.stats.udp_packets_recv.load(Relaxed) as u64)
    }
    fn reset_video(&mut self) { if let Some(media)=&self.media { media.reset_video(); } }
    fn export_frame(&self,path:&std::path::Path)->Result<bool> { portal_receiver::export_luma(self,path) }
}

async fn normal_ack<L:Link>(link:&mut L, budget:Duration, accepts:impl Fn(&SessionCommand)->bool) -> Result<SessionCommand> {
    let until=Instant::now()+budget;
    while let Some(left)=until.checked_duration_since(Instant::now()) {
        let Some(command)=link.next(left).await else { break; };
        if accepts(&command) { return Ok(command); }
        if let SessionCommand::Error{reason,..}|SessionCommand::Closed{reason,..}=command { return Err(reason); }
    }
    Err("bounded host acknowledgement timed out".into())
}

async fn settle<L:Link>(link:&mut L, duration:Duration) -> Result<()> {
    let until=Instant::now()+duration;
    while let Some(left)=until.checked_duration_since(Instant::now()) {
        match link.next(left).await {
            Some(SessionCommand::Error{reason,..}|SessionCommand::Closed{reason,..})=>return Err(reason),
            Some(_)=>{}, None=>break,
        }
    }
    Ok(())
}

async fn prepare_input<L:Link>(link:&mut L, target:Target, budgets:Budgets, owned:&mut bool, report:&mut Value) -> Result<()> {
    link.send(SessionCommand::ListSources{request_id:LIST}).await?;
    let sources=normal_ack(link,budgets.acknowledgement,|c| matches!(c,SessionCommand::Sources{request_id:LIST,..})).await?;
    let SessionCommand::Sources{sources,..}=sources else { unreachable!() };
    let source=target.select(&sources)?;
    report["exact_window_catalog_match"]=true.into();
    // Set before enqueue: cancellation cannot lose a possibly-created capture.
    *owned=true;
    link.subscribe(source.source).await?;
    let subscribed=normal_ack(link,budgets.acknowledgement,|c| matches!(c,SessionCommand::Subscribed{id:ID,..})).await?;
    let SessionCommand::Subscribed{supports_input,..}=subscribed else { unreachable!() };
    report["subscribed_acknowledged"]=true.into();
    report["actual_host_input_ready"]=supports_input.into();
    if !supports_input { return Err("actual host reported this synthetic window view-only; no input sent".into()); }
    let until=Instant::now()+budgets.acknowledgement;
    loop {
        let (frames,errors,packets)=link.video();
        report["fresh_decoded_frames"]=frames.into(); report["decode_errors"]=errors.into(); report["received_packets"]=packets.into();
        if errors!=0 { return Err("native decoder reported an error; no input sent".into()); }
        if frames>=4 { break; }
        if Instant::now()>=until { return Err("synthetic-window video was not ready; no input sent".into()); }
        settle(link,budgets.pause).await?;
    }
    report["frames_before_input"]=link.video().0.into();
    Ok(())
}

async fn operation<L:Link>(link:&mut L, target:Target, budgets:Budgets, owned:&mut bool, report:&mut Value) -> Result<()> {
    prepare_input(link,target,budgets,owned,report).await?;
    // Only center click and 'a', no clipboard, modifier, shortcut or business action.
    let events=[InputEvent::MouseMoveAbsolute{x:32768,y:32768},InputEvent::MouseDown(0),InputEvent::MouseUp(0),
        InputEvent::Key{key_code:0,pressed:true,modifiers:0},InputEvent::Key{key_code:0,pressed:false,modifiers:0}];
    for event in events {
        link.send(SessionCommand::SourceInput{id:ID,source_revision:0,event}).await?;
        let sent=report["input_commands_enqueued"].as_u64().unwrap_or(0)+1;
        report["input_commands_enqueued"]=sent.into();
        settle(link,budgets.pause).await?;
    }
    report["input_sequence_enqueued"]=true.into();
    let before=report["frames_before_input"].as_u64().unwrap_or(0);
    let until=Instant::now()+budgets.acknowledgement;
    loop {
        let (frames,errors,packets)=link.video();
        report["frames_after_input"]=frames.into();report["decode_errors"]=errors.into();report["received_packets"]=packets.into();
        if errors!=0{return Err("native decoder failed after input".into());}
        if frames>=before+2{break;}
        if Instant::now()>=until{return Err("no fresh synthetic-window video after input; cleanup follows".into());}
        settle(link,budgets.pause).await?;
    }
    Ok(())
}

// Explicit harmless suite: no command/control/option, clipboard or system shortcut.
fn suite_events() -> Vec<InputEvent> {
    let mut events=vec![InputEvent::MouseMoveAbsolute{x:16384,y:24576},InputEvent::MouseDown(0),
        InputEvent::MouseMoveAbsolute{x:32768,y:32768},InputEvent::MouseMoveAbsolute{x:49151,y:40960},
        InputEvent::MouseUp(0),InputEvent::MouseScroll{delta_x:0,delta_y:-3}];
    for key_code in [0,11,8] { // abc, physical macOS key codes, not arbitrary text injection.
        events.push(InputEvent::Key{key_code,pressed:true,modifiers:0});
        events.push(InputEvent::Key{key_code,pressed:false,modifiers:0});
    }
    events.extend([InputEvent::ModifiersChanged(1),InputEvent::Key{key_code:0,pressed:true,modifiers:1},
        InputEvent::Key{key_code:0,pressed:false,modifiers:1},InputEvent::ModifiersChanged(0)]);
    events
}

async fn operation_mode<L:Link>(link:&mut L,target:Target,budgets:Budgets,owned:&mut bool,report:&mut Value,suite:bool)->Result<()> {
    if !suite {return operation(link,target,budgets,owned,report).await;}
    prepare_input(link,target,budgets,owned,report).await?;
    report["requested_sequence"]=json!("drag_scroll_abc_shift_a");
    for event in suite_events() {
        link.send(SessionCommand::SourceInput{id:ID,source_revision:0,event}).await?;
        report["input_commands_enqueued"]=json!(report["input_commands_enqueued"].as_u64().unwrap_or(0)+1);
        settle(link,budgets.pause).await?;
    }
    report["input_sequence_enqueued"]=true.into();
    // Enqueue is not native target evidence; the independent fixture verifies effects.
    Ok(())
}

// Cleanup deliberately ignores operation cancellation and individual errors.
// It attempts all stages and never accepts a timeout or wrong ID as an ACK.
async fn cleanup_ack<L:Link>(link:&mut L, command:SessionCommand, budget:Duration,
    accepts:impl Fn(&SessionCommand)->bool, errors:&mut Vec<String>) -> bool {
    let until=Instant::now()+budget;
    let mut next_send=Instant::now();
    while let Some(left)=until.checked_duration_since(Instant::now()) {
        if Instant::now()>=next_send {
            match tokio::time::timeout(left,link.send(command.clone())).await {
                Ok(Ok(()))=>{}, Ok(Err(e))=>errors.push(e), Err(_)=>{errors.push("cleanup enqueue timed out".into());break;}
            }
            next_send=Instant::now()+Duration::from_millis(300).min(budget/2);
        }
        let wait=left.min(Duration::from_millis(100)).min(next_send.saturating_duration_since(Instant::now()));
        match link.next(wait).await {
            Some(reply) if accepts(&reply)=>return true,
            Some(SessionCommand::Error{reason,..})=>errors.push(reason),
            Some(_)|None=>{},
        }
    }
    false
}

async fn cleanup<L:Link>(link:&mut L, owned:bool, budgets:Budgets, report:&mut Value) {
    cleanup_with_input_requirement(link,owned,budgets,report,true).await;
}

async fn cleanup_with_input_requirement<L:Link>(link:&mut L, owned:bool, budgets:Budgets, report:&mut Value, require_input_release:bool) {
    let mut errors=Vec::new();
    report["input_release_required"]=require_input_release.into();
    if owned {
        let released=cleanup_ack(link,SessionCommand::ReleaseInput{id:ID},budgets.acknowledgement,
            |c|matches!(c,SessionCommand::InputReleased{id:ID}),&mut errors).await;
        report["input_released_acknowledged"]=released.into();
        let unsubscribed=cleanup_ack(link,SessionCommand::Unsubscribe{id:ID},budgets.acknowledgement,
            |c|matches!(c,SessionCommand::Unsubscribed{id:ID}),&mut errors).await;
        report["unsubscribed_acknowledged"]=unsubscribed.into();
    }
    let connection_id=link.connection_id();
    let closed=cleanup_ack(link,SessionCommand::Close{connection_id},budgets.acknowledgement,
        |c|matches!(c,SessionCommand::Closed{connection_id:id,reason} if *id==connection_id && reason==STOPPED),&mut errors).await;
    report["closed_and_workers_stopped_acknowledged"]=closed.into();
    report["cleanup_errors"]=json!(errors);
    if require_input_release && report["cleanup_errors"].as_array().is_some_and(|errors|errors.iter().any(|e|e.as_str().is_some_and(|s|s.starts_with("INPUT_UNAVAILABLE:")))) {
        report["operation_succeeded"]=false.into();report["error"]="host rejected input during cleanup".into();
    }
    report["cleanup_complete"]=json!(closed && (!owned || ((!require_input_release || report["input_released_acknowledged"]==true) && report["unsubscribed_acknowledged"]==true)));
}

async fn drive_mode<L:Link>(link:&mut L,target:Target,budgets:Budgets,mut cancel:watch::Receiver<bool>,suite:bool) -> Value {
    let mut report=json!({"schema":1,"kind":"controlled-synthetic-window-production-v2-input-probe",
        "connection_id":link.connection_id(),"subscription_id":ID,"fixture_pid":target.pid,"fixture_window":target.window,"fixture_nonce":target.nonce.clone(),
        "source_revision":0,"input_commands_enqueued":0,"input_sequence_enqueued":false,"cleanup_complete":false,
        "target_effect_verified":false,"host_received_or_injected_counters_exposed":false,"frame_contents_exported":false,
        "OS_permissions_requested_or_changed":false,"desktop_subscribed":false,"audio":false,"clipboard":false});
    let mut owned=false;
    let cancelled=*cancel.borrow();
    let result=if cancelled { Err("cancelled before operation".into()) } else {
        tokio::select! {
            result=tokio::time::timeout(budgets.operation,operation_mode(link,target,budgets,&mut owned,&mut report,suite))=>
                result.unwrap_or_else(|_|Err("operation timed out; owned cleanup follows".into())),
            _=cancel.changed()=>Err("cancelled; owned cleanup follows".into()),
        }
    };
    report["operation_succeeded"]=result.is_ok().into();
    if let Err(error)=result { report["error"]=error.into(); }
    cleanup(link,owned,budgets,&mut report).await;
    if *cancel.borrow() {report["operation_succeeded"]=false.into();report["cancelled"]=true.into();}
    report
}


async fn drive<L:Link>(link:&mut L,target:Target,budgets:Budgets,cancel:watch::Receiver<bool>)->Value {
    drive_mode(link,target,budgets,cancel,false).await
}

// Dedicated diagnostic mode. The direct parent observes held native effects,
// then terminates only this owned helper without running graceful cleanup.
// Its independent fixture observations, not this receipt, prove release.
fn phase_receipt(output:&std::path::Path, phase:&str, report:&Value) -> Result<()> {
    use std::io::Write;
    let path=output.with_extension(format!("{phase}.json"));
    let mut file=new_private_receipt(&path).map_err(|_|"new private phase receipt required")?;
    writeln!(file,"{report}").map_err(|_|"phase receipt write failed".into())
}

async fn hold_loss_operation(link:&mut Wire,target:Target,budgets:Budgets,owned:&mut bool,
    report:&mut Value,output:&std::path::Path) -> Result<()> {
    prepare_input(link,target,budgets,owned,report).await?;
    let until=Instant::now()+budgets.acknowledgement;
    let binding=loop {
        if let Some(binding)=link.conn.input_binding(ID,0) {break binding;}
        if Instant::now()>=until {return Err("current input binding unavailable; no input sent".into());}
        settle(link,Duration::from_millis(50)).await?;
    };
    // The epoch is an ephemeral capability; keep it in memory only.
    // No modifiers/shortcuts, dragging, desktop or non-fixture target.
    for event in [InputEvent::MouseMoveAbsolute{x:32768,y:32768},InputEvent::MouseDown(0),
        InputEvent::Key{key_code:0,pressed:true,modifiers:0}] {
        if !link.conn.try_bound_input(binding,Some(event)) {return Err("exact held-input enqueue failed".into());}
        report["input_commands_enqueued"]=json!(report["input_commands_enqueued"].as_u64().unwrap_or(0)+1);
        if !link.conn.refresh_bound_input(binding) {return Err("held binding expired during setup".into());}
        settle(link,Duration::from_millis(50)).await?;
    }
    report["helper_pid"]=std::process::id().into();
    report["phase"]="armed_for_independent_fixture_check".into();
    phase_receipt(output,"armed",report)?;
    loop {
        settle(link,Duration::from_millis(50)).await?;
        if !link.conn.refresh_bound_input(binding) {return Err("held binding changed or expired; no replay".into());}
    }
}

async fn drive_hold_loss(link:&mut Wire,target:Target,budgets:Budgets,mut cancel:watch::Receiver<bool>,output:&std::path::Path,focus:bool) -> Value {
    let mut report=json!({"schema":1,"kind":if focus {"controlled-synthetic-held-input-focus"} else {"controlled-synthetic-held-input-loss"},
        "connection_id":link.connection_id(),"subscription_id":ID,"fixture_pid":target.pid,
        "fixture_window":target.window,"fixture_nonce":target.nonce.clone(),"source_revision":0,
        "input_commands_enqueued":0,"external_process_loss_required":!focus,"target_effect_verified":false,
        "cleanup_complete":false,"desktop_subscribed":false,"audio":false,"clipboard":false,
        "OS_permissions_requested_or_changed":false,"cross_device_network_loss_verified":false});
    let mut owned=false;
    let result=if *cancel.borrow() {Err("cancelled before held operation".into())} else {
        tokio::select! {
            result=tokio::time::timeout(budgets.operation,hold_loss_operation(link,target,budgets,&mut owned,&mut report,output))=>
                result.unwrap_or_else(|_|Err("held operation timed out; owned cleanup follows".into())),
            _=cancel.changed()=>Err("held operation cancelled; owned cleanup follows".into()),
        }
    };
    report["operation_succeeded"]=result.is_ok().into();
    if let Err(error)=result {report["error"]=error.into();}
    // Created BEFORE any ReleaseInput/Unsubscribe/Close. The supervisor rejects
    // an observation if this exists before the parent terminates the helper.
    if let Err(error)=phase_receipt(output,"closing",&report) {
        report["operation_succeeded"]=false.into();report["error"]=error.into();
    }
    cleanup(link,owned,budgets,&mut report).await;
    report
}

pub fn main_cli(args:&[String]) {
    if args.get(1).is_some_and(|mode|mode=="replay-hevc") { hevc_diagnostic::replay_cli(&args); return; }
    if args.get(1).is_some_and(|mode|matches!(mode.as_str(),"inspect-hevc"|"inspect-hevc-input")) { hevc_diagnostic::main_cli(&args); return; }
    if args.get(1).is_some_and(|mode|mode=="wait-shared-display") {
        return portal_receiver::main_cli(args);
    }
    let hold_loss=args.get(1).is_some_and(|mode|matches!(mode.as_str(),"hold-loss"|"hold-focus"));
    let suite=args.get(1).is_some_and(|mode|mode=="input-suite");
    if args.len()!=7 || !matches!(args[1].as_str(),"input-window"|"input-suite"|"hold-loss"|"hold-focus") { eprintln!("usage: probe input-window|input-suite|hold-loss|hold-focus LOOPBACK_TARGET FIXTURE_PID WINDOW_ID FIXTURE_NONCE NEW_ABSOLUTE_OUTPUT_JSON");std::process::exit(2); }
    let checked=(||->Result<_>{
        let address=args[2].parse::<SocketAddr>().map_err(|_|"invalid route")?;
        if !address.ip().is_loopback() || address.port()==0 { return Err("must use the existing viewer-local loopback route".into()); }
        let target=Target{pid:args[3].parse().map_err(|_|"invalid PID")?,window:args[4].parse().map_err(|_|"invalid Window ID")?,nonce:args[5].clone()}.validate()?;
        let output=PathBuf::from(&args[6]);
        if !output.is_absolute() || output.exists() || !output.parent().is_some_and(|p|p.is_dir()) { return Err("new absolute output in an existing owned directory required".into()); }
        if hold_loss && ["armed","closing"].iter().any(|phase|output.with_extension(format!("{phase}.json")).exists()) {
            return Err("held mode requires new phase receipts".into());
        }
        Ok((address,target,output))
    })();
    let (address,target,output)=match checked{Ok(v)=>v,Err(e)=>{eprintln!("{e}");std::process::exit(2);}};
    let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let report=runtime.block_on(async {
        let (tx,rx)=watch::channel(false);
        tokio::spawn(async move {
            #[cfg(unix)] { let mut term=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
                tokio::select!{_ = tokio::signal::ctrl_c()=>{},_ = term.recv()=>{}}; }
            #[cfg(not(unix))] { let _=tokio::signal::ctrl_c().await; }
            let _=tx.send(true);
        });
        let setup=(||->Result<_>{
            let profile=existing_profile()?;
            let identity=remote_core::mesh::AppPrivateMeshConfigStore::new(profile).load().map_err(|_|"existing paired profile unavailable")?.ok_or("existing paired profile absent")?;
            remote_core::init_crypto_provider();remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
            let scratch=std::env::temp_dir().join(format!("rp-controlled-window-input-{}",std::process::id()));
            let mut directory=std::fs::DirBuilder::new();
            #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;directory.mode(0o700);}
            directory.create(&scratch).map_err(|_|"owned scratch unavailable")?;
            Ok(scratch)
        })();
        let scratch=match setup{Ok(p)=>p,Err(e)=>return json!({"error":e,"input_commands_enqueued":0,"target_effect_verified":false,"connection_opened":false})};
        let report=match tokio::time::timeout(Duration::from_secs(12),WorkspaceConnection::connect(address,scratch.clone())).await {
            Ok(Ok((conn,events)))=>{
                let mut link=Wire{conn,events,stats:Statistics::new(),media:None};
                if hold_loss {drive_hold_loss(&mut link,target,Budgets::default(),rx,&output,args[1]=="hold-focus").await}
                else if suite {drive_mode(&mut link,target,Budgets::default(),rx,true).await}
                else {drive(&mut link,target,Budgets::default(),rx).await}
            },
            Ok(Err(e))=>json!({"error":e,"input_commands_enqueued":0,"target_effect_verified":false,"connection_opened":false}),
            Err(_)=>json!({"error":"connection setup timed out","input_commands_enqueued":0,"target_effect_verified":false,"connection_opened":false}),
        };
        let _=std::fs::remove_dir_all(scratch); report
    });
    use std::io::Write;
    let written=new_private_receipt(&output)
        .and_then(|mut f|writeln!(f,"{}",serde_json::to_string_pretty(&report).unwrap()));
    println!("{report}");
    if written.is_err() || report["operation_succeeded"]!=true || report["cleanup_complete"]!=true { std::process::exit(1); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        sent:Vec<SessionCommand>,replies:VecDeque<SessionCommand>,ready:bool,frames:u64,
        release_ack:bool,unsubscribe_ack:bool,close_ack:bool,wrong_close:bool,
        input_failure:bool,cancel_on_key:Option<watch::Sender<bool>>,catalog_pid:i32,catalog_nonce:String,
        wrong_ack_ids:bool,late_input_error:bool,drop_first_release:bool,release_sends:usize,
        cancel_on_close:Option<watch::Sender<bool>>,
    }
    impl Default for Fake {
        fn default()->Self{Self{sent:vec![],replies:VecDeque::new(),ready:true,frames:4,release_ack:true,unsubscribe_ack:true,close_ack:true,wrong_close:false,input_failure:false,cancel_on_key:None,catalog_pid:123,catalog_nonce:NONCE.into(),wrong_ack_ids:false,late_input_error:false,drop_first_release:false,release_sends:0,cancel_on_close:None}}
    }
    impl Link for Fake {
        fn connection_id(&self)->u32{44}
        async fn send(&mut self,c:SessionCommand)->Result<()> {
            self.sent.push(c.clone());
            match c {
                SessionCommand::ListSources{request_id}=>self.replies.push_back(SessionCommand::Sources{request_id,sources:vec![CaptureSourceInfo{source:CaptureSource::Window(77),title:format!("{TITLE} {}",self.catalog_nonce),application:"owned fixture".into(),process_id:Some(self.catalog_pid),width:540,height:380,supports_input:true}]}),
                SessionCommand::ReleaseInput{id} if self.release_ack=>{
                    self.release_sends+=1;
                    if self.late_input_error{self.replies.push_back(SessionCommand::Error{request_id:ID,reason:"INPUT_UNAVAILABLE: revoked after enqueue".into()});}
                    if !self.drop_first_release || self.release_sends>1{self.replies.push_back(SessionCommand::InputReleased{id:if self.wrong_ack_ids{id+1}else{id}});}
                },
                SessionCommand::Unsubscribe{id} if self.unsubscribe_ack=>self.replies.push_back(SessionCommand::Unsubscribed{id:if self.wrong_ack_ids{id+1}else{id}}),
                SessionCommand::Close{connection_id} if self.close_ack=>{
                    if let Some(tx)=&self.cancel_on_close{let _=tx.send(true);}
                    self.replies.push_back(SessionCommand::Closed{connection_id:if self.wrong_ack_ids{connection_id+1}else{connection_id},reason:if self.wrong_close{"timeout"}else{STOPPED}.into()});
                },
                SessionCommand::SourceInput{event:InputEvent::Key{pressed:true,..},..}=>{
                    if let Some(tx)=&self.cancel_on_key{let _=tx.send(true);}
                    if self.input_failure{self.replies.push_back(SessionCommand::Error{request_id:ID,reason:"INPUT_UNAVAILABLE: revoked".into()});}
                }, _=>{},
            } Ok(())
        }
        async fn subscribe(&mut self,source:CaptureSource)->Result<()> {
            assert_eq!(source,CaptureSource::Window(77));
            self.replies.push_back(SessionCommand::Subscribed{id:ID,audio_owner:None,supports_input:self.ready});Ok(())
        }
        async fn next(&mut self,wait:Duration)->Option<SessionCommand>{if let Some(c)=self.replies.pop_front(){Some(c)}else{tokio::time::sleep(wait).await;None}}
        fn video(&self)->(u64,u64,u64){let n=self.frames+if input_count(self)==5{4}else{0};(n,0,n)}
    }
    const NONCE:&str="0123456789abcdef0123456789abcdef";
    fn target()->Target{Target{pid:123,window:77,nonce:NONCE.into()}}
    fn short()->Budgets{Budgets{operation:Duration::from_millis(100),acknowledgement:Duration::from_millis(12),pause:Duration::from_millis(2)}}
    async fn run(fake:&mut Fake)->Value{let (_tx,rx)=watch::channel(false);drive(fake,target(),short(),rx).await}
    fn input_count(fake:&Fake)->usize{fake.sent.iter().filter(|c|matches!(c,SessionCommand::SourceInput{..})).count()}
    fn has_close(fake:&Fake)->bool{fake.sent.iter().any(|c|matches!(c,SessionCommand::Close{connection_id:44}))}
    #[tokio::test] async fn normal_exit_releases_input_unsubscribes_and_confirms_exact_closed(){let mut f=Fake::default();let r=run(&mut f).await;assert_eq!(r["operation_succeeded"],true);assert_eq!(r["cleanup_complete"],true);assert_eq!(input_count(&f),5);assert_eq!(r["target_effect_verified"],false);assert!(has_close(&f));}
    #[tokio::test] async fn denied_live_readiness_never_sends_input_but_cleans_capture(){let mut f=Fake{ready:false,..Fake::default()};let r=run(&mut f).await;assert_eq!(input_count(&f),0);assert_eq!(r["cleanup_complete"],true);assert_eq!(r["operation_succeeded"],false);}
    #[tokio::test] async fn wrong_fixture_pid_never_subscribes_or_sends_input(){let mut f=Fake{catalog_pid:999,..Fake::default()};let r=run(&mut f).await;assert_eq!(input_count(&f),0);assert!(r.get("subscribed_acknowledged").is_none());assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn timeout_without_native_frames_still_releases_and_closes(){let mut f=Fake{frames:0,..Fake::default()};let r=run(&mut f).await;assert_eq!(input_count(&f),0);assert_eq!(r["cleanup_complete"],true);assert!(has_close(&f));}
    #[tokio::test] async fn host_error_after_keydown_still_releases_and_closes(){let mut f=Fake{input_failure:true,..Fake::default()};let r=run(&mut f).await;assert_eq!(r["operation_succeeded"],false);assert_eq!(r["input_commands_enqueued"],4);assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn cancellation_after_keydown_does_not_cancel_cleanup(){let (tx,rx)=watch::channel(false);let mut f=Fake{cancel_on_key:Some(tx),..Fake::default()};let r=drive(&mut f,target(),short(),rx).await;assert_eq!(r["operation_succeeded"],false);assert_eq!(r["cleanup_complete"],true);assert!(has_close(&f));}
    #[tokio::test] async fn unsubscribe_failure_never_skips_close_or_claims_cleanup(){let mut f=Fake{unsubscribe_ack:false,..Fake::default()};let r=run(&mut f).await;assert!(has_close(&f));assert_eq!(r["closed_and_workers_stopped_acknowledged"],true);assert_eq!(r["cleanup_complete"],false);}
    #[tokio::test] async fn release_failure_never_skips_unsubscribe_or_close(){let mut f=Fake{release_ack:false,..Fake::default()};let r=run(&mut f).await;assert_eq!(r["unsubscribed_acknowledged"],true);assert!(has_close(&f));assert_eq!(r["cleanup_complete"],false);}
    #[tokio::test] async fn timeout_closed_reason_cannot_prove_workers_stopped(){let mut f=Fake{wrong_close:true,..Fake::default()};let r=run(&mut f).await;assert_eq!(r["cleanup_complete"],false);assert_eq!(r["closed_and_workers_stopped_acknowledged"],false);}
    #[tokio::test] async fn cancellation_before_catalog_only_closes_own_connection(){let (tx,rx)=watch::channel(true);let mut f=Fake::default();let r=drive(&mut f,target(),short(),rx).await;drop(tx);assert_eq!(f.sent.len(),1);assert!(has_close(&f));assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn wrong_ack_ids_cannot_confirm_any_owned_cleanup(){let mut f=Fake{wrong_ack_ids:true,..Fake::default()};let r=run(&mut f).await;assert_eq!(r["cleanup_complete"],false);assert_eq!(r["input_released_acknowledged"],false);assert_eq!(r["closed_and_workers_stopped_acknowledged"],false);}
    #[tokio::test] async fn lost_release_ack_retries_same_subscription_and_then_closes(){let mut f=Fake{drop_first_release:true,..Fake::default()};let r=run(&mut f).await;assert!(f.release_sends>=2);assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn late_permission_error_never_becomes_input_success(){let mut f=Fake{late_input_error:true,..Fake::default()};let r=run(&mut f).await;assert_eq!(r["operation_succeeded"],false);assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn cancellation_during_cleanup_still_confirms_close(){let (tx,rx)=watch::channel(false);let mut f=Fake{cancel_on_close:Some(tx),..Fake::default()};let r=drive(&mut f,target(),short(),rx).await;assert_eq!(r["cancelled"],true);assert_eq!(r["cleanup_complete"],true);assert_eq!(r["operation_succeeded"],false);}
    #[tokio::test] async fn overall_operation_timeout_after_a_press_still_cleans_owned_input(){let mut f=Fake::default();let mut budgets=short();budgets.operation=Duration::from_millis(6);let (_tx,rx)=watch::channel(false);let r=drive(&mut f,target(),budgets,rx).await;assert_eq!(r["operation_succeeded"],false);assert!(input_count(&f)>0);assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn different_fixture_nonce_never_subscribes_or_sends_input(){let mut f=Fake{catalog_nonce:"ffffffffffffffffffffffffffffffff".into(),..Fake::default()};let r=run(&mut f).await;assert_eq!(input_count(&f),0);assert!(r.get("subscribed_acknowledged").is_none());assert_eq!(r["cleanup_complete"],true);}
    #[tokio::test] async fn suite_keeps_same_guard_and_confirms_cleanup() {
        let mut f=Fake::default();let (_tx,rx)=watch::channel(false);
        let r=drive_mode(&mut f,target(),Budgets{operation:Duration::from_secs(1),acknowledgement:Duration::from_millis(20),pause:Duration::from_millis(1)},rx,true).await;
        assert_eq!(r["operation_succeeded"],true);assert_eq!(r["cleanup_complete"],true);
        assert_eq!(input_count(&f),16);assert_eq!(r["target_effect_verified"],false);
    }
    #[tokio::test] async fn suite_view_only_never_enqueues_input() {
        let mut f=Fake{ready:false,..Fake::default()};let (_tx,rx)=watch::channel(false);
        let r=drive_mode(&mut f,target(),short(),rx,true).await;
        assert_eq!(input_count(&f),0);assert_eq!(r["cleanup_complete"],true);
    }
    #[test] fn suite_only_uses_safe_keys_and_releases_modifiers_and_button() {
        let e=suite_events();assert_eq!(e.len(),16);
        assert!(matches!(e.last(),Some(InputEvent::ModifiersChanged(0))));
        assert_eq!(e.iter().filter(|v|matches!(v,InputEvent::MouseDown(0))).count(),1);
        assert_eq!(e.iter().filter(|v|matches!(v,InputEvent::MouseUp(0))).count(),1);
        for v in e {if let InputEvent::Key{key_code,modifiers,..}=v {assert!([0,11,8].contains(&key_code));assert!(modifiers<=1);}}
    }
    #[test] fn target_ids_are_required(){assert!(Target{pid:0,window:77,nonce:NONCE.into()}.validate().is_err());assert!(Target{pid:123,window:0,nonce:NONCE.into()}.validate().is_err());}
}
