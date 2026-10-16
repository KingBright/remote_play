//! Explicit coordinated synthetic-screen acceptance only. Never launches a picker.
//! Keep one authenticated connection/subscription through the local Portal grant.
use super::*;
use std::path::Path;

const SWITCH: u32 = 900_231;
const PORTAL_BIT: u32 = 0x8000_0000;

fn shared_display(sources:&[CaptureSourceInfo])->Result<Option<CaptureSourceInfo>> {
    let mut matches=sources.iter().filter(|s| matches!(s.source,CaptureSource::Display(id) if id&PORTAL_BIT!=0)
        && s.title=="Shared display" && s.process_id.is_none() && s.width>=200 && s.height>=150);
    let selected=matches.next().cloned();
    if matches.next().is_some(){return Err("ambiguous Portal displays; no source guessed".into());}
    Ok(selected)
}

fn input_approved(output:&Path,nonce:&str,source:CaptureSource)->Result<bool> {
    let p=output.with_extension("input-approved.json");
    let metadata=match p.symlink_metadata(){Ok(v)=>v,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(false),Err(_)=>return Err("approval receipt unavailable".into())};
    if !metadata.is_file() || metadata.len()>4096 {return Err("unsafe input approval receipt".into());}
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid()!=unsafe{libc::geteuid()} || metadata.mode()&0o077!=0 {return Err("private same-user input approval required".into());}
    }
    let bytes=std::fs::read(p).map_err(|_|"approval receipt unreadable")?;
    let approval:Value=serde_json::from_slice(&bytes).map_err(|_|"invalid approval receipt")?;
    if approval["fixture_nonce"]!=nonce || approval["source"]!=json!(source)
        || approval["source_revision"]!=SWITCH || approval["visual_verified"]!=true {
        return Err("input approval does not match the live synthetic source".into());
    }
    Ok(true)
}

async fn operate<L:Link>(link:&mut L,output:&Path,nonce:&str,b:Budgets,owned:&mut bool,report:&mut Value)->Result<()> {
    // The existing server only lists local-share targets for live subscriptions.
    // A coordinated full-screen fixture must already cover the monitor: this
    // bootstrap may briefly start its legacy capture before pause is acknowledged.
    *owned=true;link.subscribe(CaptureSource::MainDisplay).await?;
    normal_ack(link,b.acknowledgement,|c|matches!(c,SessionCommand::Subscribed{id:ID,..})).await?;
    link.send(SessionCommand::SetActivity{id:ID,revision:1,video:false,audio:false}).await?;
    normal_ack(link,b.acknowledgement,|c|matches!(c,SessionCommand::Activity{id:ID,revision:1,video:false,audio:false})).await?;
    report["bootstrap_paused_acknowledged"]=true.into();
    phase_receipt(output,"waiting",report)?;
    let mut request=900_220;
    let selected=loop {
        link.send(SessionCommand::ListSources{request_id:request}).await?;
        let reply=normal_ack(link,b.acknowledgement,|c|matches!(c,SessionCommand::Sources{request_id,..} if *request_id==request)).await?;
        let SessionCommand::Sources{sources,..}=reply else{unreachable!()};
        if let Some(source)=shared_display(&sources)?{break source;}
        if output.with_extension("stop").exists(){return Err("coordinator stopped while waiting for local share".into());}
        request=request.checked_add(1).ok_or("request range exhausted")?;
        settle(link,b.pause).await?;
    };
    report["portal_source"]=json!(selected.source);
    report["source_revision"]=SWITCH.into();
    link.send(SessionCommand::SwitchSource{id:ID,request_id:SWITCH,source:selected.source}).await?;
    let reply=normal_ack(link,b.acknowledgement,|c|matches!(c,SessionCommand::SourceSwitched{id:ID,request_id:SWITCH,source,..} if *source==selected.source)).await?;
    let SessionCommand::SourceSwitched{supports_input,..}=reply else{unreachable!()};
    report["switch_acknowledged"]=true.into();report["actual_host_input_ready"]=supports_input.into();
    link.reset_video();let before=link.video().0;
    link.send(SessionCommand::SetActivity{id:ID,revision:2,video:true,audio:false}).await?;
    normal_ack(link,b.acknowledgement,|c|matches!(c,SessionCommand::Activity{id:ID,revision:2,video:true,audio:false})).await?;
    let until=Instant::now()+b.acknowledgement;
    loop {
        let (frames,errors,packets)=link.video();
        report["fresh_portal_decoded_frames"]=frames.saturating_sub(before).into();report["decode_errors"]=errors.into();report["received_packets"]=packets.into();
        if errors!=0{return Err("decoder error after Portal switch; no input sent".into());}
        if frames>=before+4{break;}
        if Instant::now()>=until{return Err("no four fresh Portal frames; no input sent".into());}
        settle(link,b.pause).await?;
    }
    report["decoded_luma_exported"]=link.export_frame(&output.with_extension("pgm"))?.into();
    // This receipt is readiness, not an assertion of correct visible content.
    phase_receipt(output,"shared",report)?;
    loop {
        if output.with_extension("stop").exists(){return Ok(());}
        if input_approved(output,nonce,selected.source)? {
            if !supports_input{return Err("Portal share is view-only; no input sent".into());}
            report["operator_visual_confirmation_received"]=true.into();
            for event in suite_events(){
                link.send(SessionCommand::SourceInput{id:ID,source_revision:SWITCH,event}).await?;
                report["input_commands_enqueued"]=json!(report["input_commands_enqueued"].as_u64().unwrap_or(0)+1);
                settle(link,b.pause).await?;
            }
            return Ok(());
        }
        settle(link,b.pause).await?;
    }
}

async fn drive<L:Link>(link:&mut L,output:&Path,nonce:&str,b:Budgets,mut cancel:watch::Receiver<bool>)->Value {
    let mut report=json!({"kind":"coordinated-synthetic-Portal-receiver","connection_id":link.connection_id(),"subscription_id":ID,
        "fixture_nonce":nonce,"input_commands_enqueued":0,"target_effect_verified":false,"visual_verified":false,
        "picker_launched":false,"OS_permissions_changed":false,"new_connection_on_switch":false});
    let mut owned=false;
    let result=if *cancel.borrow(){Err("cancelled before subscription".into())}else{tokio::select!{
        r=tokio::time::timeout(b.operation,operate(link,output,nonce,b,&mut owned,&mut report))=>r.unwrap_or_else(|_|Err("coordinated operation deadline; cleanup follows".into())),
        _=cancel.changed()=>Err("coordinator cancelled; cleanup follows".into()),
    }};
    report["operation_succeeded"]=result.is_ok().into();if let Err(e)=result{report["error"]=e.into();}
    let input_sent=report["input_commands_enqueued"].as_u64().unwrap_or(0)>0;
    cleanup_with_input_requirement(link,owned,b,&mut report,input_sent).await;report
}

pub(super) fn main_cli(args:&[String]) {
    let checked=(||->Result<_>{
        if !cfg!(target_os="macos"){return Err("Portal receiver coordination currently requires the Mac sender".into());}
        if args.len()!=6 || args[5]!="--coordinated-synthetic-screen"{return Err("usage: wait-shared-display LOOPBACK_TARGET FIXTURE_NONCE NEW_ABSOLUTE_OUTPUT_JSON --coordinated-synthetic-screen".into());}
        let target:SocketAddr=args[2].parse().map_err(|_|"invalid route")?;
        if !target.ip().is_loopback()||target.port()==0{return Err("existing loopback device route required".into());}
        let nonce=&args[3];if nonce.len()!=32||!nonce.bytes().all(|c|c.is_ascii_hexdigit()){return Err("fresh synthetic fixture nonce required".into());}
        let output=PathBuf::from(&args[4]);
        if !output.is_absolute()||!output.parent().is_some_and(|p|p.is_dir())||output.exists()
            ||["waiting.json","shared.json","input-approved.json","stop","pgm"].iter().any(|e|output.with_extension(e).exists()){
            return Err("new output and phase paths in an existing owned directory required".into());
        }
        Ok((target,output))
    })();
    let (target,output)=match checked{Ok(v)=>v,Err(e)=>{eprintln!("{e}");std::process::exit(2)}};
    let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let report=runtime.block_on(async{
        let setup=(||->Result<()>{let identity=remote_core::mesh::AppPrivateMeshConfigStore::new(existing_profile()?).load().map_err(|_|"paired profile unavailable")?.ok_or("paired profile absent")?;
            remote_core::init_crypto_provider();remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());Ok(())})();
        if let Err(error)=setup{return json!({"error":error,"input_commands_enqueued":0});}
        let (cancel,rx)=watch::channel(false);tokio::spawn(async move{
            #[cfg(unix)]{let mut term=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();tokio::select!{_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}};}
            #[cfg(not(unix))]{let _=tokio::signal::ctrl_c().await;}
            let _=cancel.send(true);
        });
        match tokio::time::timeout(Duration::from_secs(12),WorkspaceConnection::connect_media(target)).await{
            Ok(Ok((conn,events)))=>{let mut wire=Wire{conn,events,stats:Statistics::new(),media:None};
                drive(&mut wire,&output,&args[3],Budgets{operation:Duration::from_secs(100),..Budgets::default()},rx).await},
            Ok(Err(error))=>json!({"error":error,"input_commands_enqueued":0}),Err(_)=>json!({"error":"connection setup timed out","input_commands_enqueued":0}),
        }
    });
    use std::io::Write;let written=new_private_receipt(&output).and_then(|mut f|writeln!(f,"{}",serde_json::to_string_pretty(&report).unwrap()));println!("{report}");
    if written.is_err()||report["operation_succeeded"]!=true||report["cleanup_complete"]!=true{std::process::exit(1);}
}

#[cfg(target_os="macos")]
pub(super) fn export_luma(wire:&Wire,path:&Path)->Result<bool>{
    #[link(name="CoreVideo",kind="framework")]
    unsafe extern "C"{
        fn CVPixelBufferLockBaseAddress(p:*mut std::ffi::c_void,f:u64)->i32;
        fn CVPixelBufferUnlockBaseAddress(p:*mut std::ffi::c_void,f:u64)->i32;
        fn CVPixelBufferGetBaseAddressOfPlane(p:*mut std::ffi::c_void,n:usize)->*const u8;
        fn CVPixelBufferGetWidthOfPlane(p:*mut std::ffi::c_void,n:usize)->usize;
        fn CVPixelBufferGetHeightOfPlane(p:*mut std::ffi::c_void,n:usize)->usize;
        fn CVPixelBufferGetBytesPerRowOfPlane(p:*mut std::ffi::c_void,n:usize)->usize;
        fn CVPixelBufferGetPixelFormatType(p:*mut std::ffi::c_void)->u32;
    }
    let media=wire.media.as_ref().ok_or("media runtime absent")?;let shared=media.shared_frame();let held=shared.lock().unwrap();
    let frame=held.as_ref().ok_or("decoded Portal frame absent")?;
    unsafe{
        let p=frame.cv_pixel_buffer;
        if ![u32::from_be_bytes(*b"420v"),u32::from_be_bytes(*b"420f")].contains(&CVPixelBufferGetPixelFormatType(p)){return Err("unsupported luma evidence format".into());}
        if CVPixelBufferLockBaseAddress(p,1)!=0{return Err("decoded frame lock failed".into());}
        let w=CVPixelBufferGetWidthOfPlane(p,0);let h=CVPixelBufferGetHeightOfPlane(p,0);let stride=CVPixelBufferGetBytesPerRowOfPlane(p,0);let base=CVPixelBufferGetBaseAddressOfPlane(p,0);
        let bytes=if !base.is_null()&&w>0&&w<=1920&&h>0&&h<=1080&&stride>=w{
            let mut bytes=format!("P5\n{w} {h}\n255\n").into_bytes();for y in 0..h{bytes.extend_from_slice(std::slice::from_raw_parts(base.add(y*stride),w));}Some(bytes)
        }else{None};CVPixelBufferUnlockBaseAddress(p,1);
        let bytes=bytes.ok_or("invalid decoded evidence dimensions")?;use std::io::Write;
        let mut file=new_private_receipt(path).map_err(|e|e.to_string())?;file.write_all(&bytes).map_err(|e|e.to_string())?;
    }
    Ok(true)
}
#[cfg(not(target_os="macos"))]
pub(super) fn export_luma(_wire:&Wire,_path:&Path)->Result<bool>{Ok(false)}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell,collections::VecDeque};
    const NONCE:&str="0123456789abcdef0123456789abcdef";
    fn source(id:u32,title:&str)->CaptureSourceInfo{CaptureSourceInfo{source:CaptureSource::Display(id),title:title.into(),application:String::new(),process_id:None,width:800,height:450,supports_input:true}}
    struct Fake {
        sent:Vec<SessionCommand>,replies:VecDeque<SessionCommand>,catalog:Vec<CaptureSourceInfo>,
        resumed:bool,frames:Cell<u64>,output:PathBuf,approval:bool,bad_approval:bool,bad_switch:bool,
        stop_on_export:bool,subscribes:usize,reset:bool,release_ack:bool,
    }
    impl Fake {fn new(output:PathBuf)->Self{Self{sent:vec![],replies:VecDeque::new(),catalog:vec![source(PORTAL_BIT+9,"Shared display")],resumed:false,frames:Cell::new(0),output,approval:false,bad_approval:false,bad_switch:false,stop_on_export:true,subscribes:0,reset:false,release_ack:true}}}
    impl Link for Fake {
        fn connection_id(&self)->u32{71}
        async fn send(&mut self,c:SessionCommand)->Result<()>{
            self.sent.push(c.clone());match c{
                SessionCommand::SetActivity{id,revision,video,audio}=>{self.resumed=video;self.replies.push_back(SessionCommand::Activity{id,revision,video,audio});},
                SessionCommand::ListSources{request_id}=>self.replies.push_back(SessionCommand::Sources{request_id,sources:self.catalog.clone()}),
                SessionCommand::SwitchSource{id,request_id,source}=>self.replies.push_back(SessionCommand::SourceSwitched{id,request_id:request_id+u32::from(self.bad_switch),source,supports_input:true}),
                SessionCommand::ReleaseInput{id}=>if self.release_ack{self.replies.push_back(SessionCommand::InputReleased{id})}else{return Err("input binding unavailable".into())},
                SessionCommand::Unsubscribe{id}=>self.replies.push_back(SessionCommand::Unsubscribed{id}),
                SessionCommand::Close{connection_id}=>self.replies.push_back(SessionCommand::Closed{connection_id,reason:STOPPED.into()}),_=>{}
            }Ok(())
        }
        async fn subscribe(&mut self,s:CaptureSource)->Result<()>{assert_eq!(s,CaptureSource::MainDisplay);self.subscribes+=1;self.replies.push_back(SessionCommand::Subscribed{id:ID,audio_owner:None,supports_input:true});Ok(())}
        async fn next(&mut self,_wait:Duration)->Option<SessionCommand>{tokio::task::yield_now().await;self.replies.pop_front()}
        fn video(&self)->(u64,u64,u64){if self.resumed{self.frames.set(self.frames.get()+2);} (self.frames.get(),0,8)}
        fn reset_video(&mut self){self.reset=true;self.frames.set(0);}
        fn export_frame(&self,_path:&Path)->Result<bool>{
            if self.approval{
                use std::io::Write;let mut f=new_private_receipt(&self.output.with_extension("input-approved.json")).unwrap();
                writeln!(f,"{}",json!({"fixture_nonce":if self.bad_approval{"other"}else{NONCE},"source":CaptureSource::Display(PORTAL_BIT+9),"source_revision":SWITCH,"visual_verified":true})).unwrap();
            }else if self.stop_on_export{std::fs::write(self.output.with_extension("stop"),b"stop").unwrap();}
            Ok(false)
        }
    }
    fn budgets()->Budgets{Budgets{operation:Duration::from_millis(30),acknowledgement:Duration::from_millis(8),pause:Duration::from_millis(1)}}
    async fn run(fake:&mut Fake,output:&Path,cancel:bool)->Value{let (_tx,rx)=watch::channel(cancel);drive(fake,output,NONCE,budgets(),rx).await}
    #[test]
    fn selection_never_uses_legacy_display_or_wrong_owner_shape(){
        assert!(shared_display(&[source(1,"Shared display"),source(PORTAL_BIT+1,"Desktop")]).unwrap().is_none());
        let mut s=source(PORTAL_BIT+1,"Shared display");s.process_id=Some(1);assert!(shared_display(&[s]).unwrap().is_none());
        assert!(shared_display(&[source(PORTAL_BIT+1,"Shared display"),source(PORTAL_BIT+2,"Shared display")]).is_err());
    }
    #[tokio::test]
    async fn pause_switch_resume_keep_one_connection_and_subscription_without_input(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());let r=run(&mut f,&p,false).await;
        assert_eq!(r["operation_succeeded"],true);assert_eq!(r["cleanup_complete"],true);assert_eq!(r["connection_id"],71);assert_eq!(r["subscription_id"],ID);assert_eq!(f.subscribes,1);assert!(f.reset);
        assert!(tmp.path().join("result.waiting.json").exists()&&tmp.path().join("result.shared.json").exists());
        let pause=f.sent.iter().position(|c|matches!(c,SessionCommand::SetActivity{video:false,..})).unwrap();let switch=f.sent.iter().position(|c|matches!(c,SessionCommand::SwitchSource{..})).unwrap();let resume=f.sent.iter().position(|c|matches!(c,SessionCommand::SetActivity{video:true,..})).unwrap();assert!(pause<switch&&switch<resume);
        assert_eq!(r["input_commands_enqueued"],0);assert_eq!(r["visual_verified"],false);
    }
    #[tokio::test]
    async fn wrong_switch_ack_never_resumes_or_sends_input(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.bad_switch=true;let r=run(&mut f,&p,false).await;
        assert_eq!(r["operation_succeeded"],false);assert_eq!(r["cleanup_complete"],true);assert!(!f.sent.iter().any(|c|matches!(c,SessionCommand::SetActivity{video:true,..}|SessionCommand::SourceInput{..})));
    }
    #[tokio::test]
    async fn missing_portal_times_out_and_cleans_without_reconnecting(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.catalog.clear();let r=run(&mut f,&p,false).await;
        assert_eq!(r["operation_succeeded"],false);assert_eq!(r["cleanup_complete"],true);assert_eq!(f.subscribes,1);assert_eq!(r["input_commands_enqueued"],0);
    }
    #[tokio::test]
    async fn pre_cancel_creates_no_subscription(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());let r=run(&mut f,&p,true).await;assert_eq!(f.subscribes,0);assert_eq!(r["cleanup_complete"],true);
    }
    #[tokio::test]
    async fn approval_must_match_current_fixture_source_and_revision(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.approval=true;f.bad_approval=true;let r=run(&mut f,&p,false).await;
        assert_eq!(r["operation_succeeded"],false);assert_eq!(r["input_commands_enqueued"],0);assert_eq!(r["cleanup_complete"],true);
    }
    #[tokio::test]
    async fn explicit_visual_approval_sends_only_bounded_suite_at_switched_revision(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.approval=true;let r=run(&mut f,&p,false).await;
        assert_eq!(r["operation_succeeded"],true);assert_eq!(r["input_commands_enqueued"],16);assert_eq!(r["target_effect_verified"],false);assert_eq!(r["cleanup_complete"],true);
        assert!(f.sent.iter().filter_map(|c|if let SessionCommand::SourceInput{id,source_revision,..}=c{Some((*id,*source_revision))}else{None}).all(|v|v==(ID,SWITCH)));
    }
    #[tokio::test]
    async fn video_only_without_input_lease_reports_missing_release_ack_truthfully(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.release_ack=false;let r=run(&mut f,&p,false).await;
        assert_eq!(r["input_commands_enqueued"],0);assert_eq!(r["input_release_required"],false);assert_eq!(r["input_released_acknowledged"],false);assert_eq!(r["cleanup_complete"],true);
    }
    #[tokio::test]
    async fn actual_input_still_requires_release_ack(){
        let tmp=tempfile::tempdir().unwrap();let p=tmp.path().join("result.json");let mut f=Fake::new(p.clone());f.approval=true;f.release_ack=false;let r=run(&mut f,&p,false).await;
        assert_eq!(r["input_commands_enqueued"],16);assert_eq!(r["input_release_required"],true);assert_eq!(r["input_released_acknowledged"],false);assert_eq!(r["cleanup_complete"],false);assert_eq!(r["closed_and_workers_stopped_acknowledged"],true);
    }
}
