//! Bounded HEVC boundary diagnostic, only inside a coordinated synthetic-screen slot.
use super::*;
use tokio::sync::mpsc;
use std::sync::Mutex;
use std::io::Write;

fn nal_types(data:&[u8])->Vec<u8>{
    let mut types=Vec::new();let mut i=0;
    while i+3<data.len(){
        let n=if data[i..].starts_with(&[0,0,0,1]){4}else if data[i..].starts_with(&[0,0,1]){3}else{0};
        if n>0 && i+n+1<data.len(){types.push((data[i+n]>>1)&63);i+=n+2;}else{i+=1;}
    }
    types
}
#[derive(Default)]
struct Samples { rows:Vec<Value>, bytes:Vec<u8>, units:usize }
async fn inspect(target:SocketAddr,output:&std::path::Path,nonce:&str,inputs:bool)->Result<Value>{
    let identity=remote_core::mesh::AppPrivateMeshConfigStore::new(existing_profile()?).load()
        .map_err(|_|"paired profile unavailable")?.ok_or("paired profile absent")?;
    remote_core::init_crypto_provider();
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    let started=Instant::now();
    let (conn,events)=tokio::time::timeout(Duration::from_secs(12),WorkspaceConnection::connect_media(target))
        .await.map_err(|_|"authentication deadline")??;
    let stats=Statistics::new();
    let media=client::ClientMediaRuntime::start_video_only(stats.clone()).map_err(|e|e.to_string())?;
    let status=media.decode_status.clone();
    let (tx,mut rx)=mpsc::channel::<(protocol::RtpPacket,protocol::FrameTimingCheckpoints)>(32);
    let decoder=media.decode_tx.clone();let samples=Arc::new(Mutex::new(Samples::default()));
    let captured=samples.clone();
    let tee=tokio::spawn(async move{
        while let Some((packet,timing))=rx.recv().await{
            {let mut s=captured.lock().unwrap();
                if s.rows.len()<96{
                    let types=nal_types(&packet.payload);
                    s.rows.push(json!({"sequence":packet.header.sequence_number,"timestamp":packet.header.timestamp,
                        "ssrc":packet.header.ssrc,"payload_bytes":packet.payload.len(),"nal_types":types,
                        "keyframe_detector":remote_core::media_plane::is_hevc_keyframe(&packet.payload),
                        "prefix_hex":packet.payload.iter().take(12).map(|b|format!("{b:02x}")).collect::<String>()}));
                }
                if s.units<32 && s.bytes.len()+packet.payload.len()<=4*1024*1024{
                    s.bytes.extend_from_slice(&packet.payload);s.units+=1;
                }
            }
            if decoder.send((packet,timing)).await.is_err(){break;}
        }
    });
    let mut link=Wire{conn,events,stats:stats.clone(),media:Some(media)};
    let mut report=json!({"kind":"coordinated-synthetic-HEVC-boundary-diagnostic","fixture_nonce":nonce,
        "connection_id":link.connection_id(),"input_commands_enqueued":0,"visual_verified":false,
        "authentication_and_open_elapsed_ms":started.elapsed().as_millis() as u64,
        "timing_semantics":"local monotonic elapsed authentication plus V2 open; not RTT or one-way latency",
        "raw_stage":"after production reassembly/jitter, before production decoder; unchanged tee",
        "OS_permissions_changed":false});
    let b=Budgets{operation:Duration::from_secs(16),acknowledgement:Duration::from_secs(5),pause:Duration::from_millis(50)};
    let result=async{
        link.conn.subscribe(SubscriptionRequest{id:ID,source:CaptureSource::MainDisplay,width:960,height:540,fps:10,bitrate_kbps:1200,audio:false},
            tx,stats.clone(),Arc::new(SharedHostStats::default())).await?;
        let ack=normal_ack(&mut link,b.acknowledgement,|c|matches!(c,SessionCommand::Subscribed{id:ID,..})).await?;
        let SessionCommand::Subscribed{supports_input,..}=ack else{unreachable!()};
        report["actual_host_input_ready"]=supports_input.into();
        if inputs && !supports_input{return Err("host reported view-only; no input sent".into());}
        report["subscribed_acknowledged"]=true.into();
        let until=Instant::now()+Duration::from_secs(12);
        while Instant::now()<until && stats.video_frames_decoded.load(Relaxed)<4 {
            if output.with_extension("stop").exists(){return Err("coordinator stopped diagnostic".into());}
            settle(&mut link,b.pause).await?;
        }
        Ok::<(),String>(())
    }.await;
    report["operation_succeeded"]=result.is_ok().into();if let Err(e)=result{report["error"]=e.into();}
    report["decoder_status"]=status.lock().unwrap().clone().into();
    report["decoded_frames"]=stats.video_frames_decoded.load(Relaxed).into();
    report["decode_errors"]=stats.video_decode_errors.load(Relaxed).into();
    report["received_packets"]=stats.udp_packets_recv.load(Relaxed).into();
    match link.export_frame(&output.with_extension("pgm")){Ok(v)=>report["decoded_luma_exported"]=v.into(),Err(e)=>{report["operation_succeeded"]=false.into();report["error"]=e.into();}}
    if inputs && report["operation_succeeded"]==true {
        let action=staged_input(&mut link,output,nonce,&mut report).await;
        if let Err(e)=action{report["operation_succeeded"]=false.into();report["error"]=e.into();}
    }
    report["final_decoded_frames"]=stats.video_frames_decoded.load(Relaxed).into();
    report["final_decode_errors"]=stats.video_decode_errors.load(Relaxed).into();
    report["final_decoder_status"]=status.lock().unwrap().clone().into();
    let input_sent=report["input_commands_enqueued"].as_u64().unwrap_or(0)>0;
    cleanup_with_input_requirement(&mut link,true,b,&mut report,input_sent).await;
    tee.abort();let _=tee.await;
    let s=samples.lock().unwrap();
    report["access_units"]=json!(s.rows);report["sampled_units"]=s.units.into();report["sampled_bytes"]=s.bytes.len().into();
    if !s.bytes.is_empty(){new_private_receipt(&output.with_extension("hevc")).map_err(|e|e.to_string())?
        .write_all(&s.bytes).map_err(|e|e.to_string())?;}
    Ok(report)
}
pub(super) fn main_cli(args:&[String]){
    let parsed=(||->Result<_>{
        if args.len()!=6 || args[5]!="--coordinated-synthetic-screen"{return Err("inspect-hevc LOOPBACK_TARGET NONCE NEW_ABSOLUTE_OUTPUT_JSON --coordinated-synthetic-screen required".into());}
        let target:SocketAddr=args[2].parse().map_err(|_|"invalid target")?;
        if !target.ip().is_loopback()||target.port()==0{return Err("existing loopback route required".into());}
        let nonce=&args[3];if nonce.len()!=32||!nonce.bytes().all(|b|b.is_ascii_hexdigit()){return Err("synthetic nonce required".into());}
        let output=PathBuf::from(&args[4]);
        if !output.is_absolute()||!output.parent().is_some_and(|p|p.is_dir())||output.exists()
            ||["hevc","pgm","stop","click-approved.json","text-approved.json","video-ready.json","clicked.json","input.pgm"].iter().any(|ext|output.with_extension(ext).exists()){return Err("fresh owned diagnostic output paths required".into());}
        Ok((target,output))
    })();
    let(target,output)=match parsed{Ok(v)=>v,Err(e)=>{eprintln!("{e}");std::process::exit(2)}};
    let rt=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let report=match rt.block_on(inspect(target,&output,&args[3],args[1]=="inspect-hevc-input")){Ok(v)=>v,Err(e)=>json!({"error":e,"input_commands_enqueued":0})};
    let good=report["operation_succeeded"]==true && report["cleanup_complete"]==true && report["decoded_frames"].as_u64().unwrap_or(0)>0;
    new_private_receipt(&output).unwrap().write_all(serde_json::to_vec_pretty(&report).unwrap().as_slice()).unwrap();
    println!("{report}");std::process::exit(if good{0}else{1});
}

fn ascii_key(c:u8)->Option<u16>{Some(match c{b'a'=>0,b'b'=>11,b'c'=>8,b'd'=>2,b'e'=>14,b'f'=>3,b'0'=>29,b'1'=>18,b'2'=>19,b'3'=>20,b'4'=>21,b'5'=>23,b'6'=>22,b'7'=>26,b'8'=>28,b'9'=>25,_=>return None})}
fn approval(path:&std::path::Path,nonce:&str,phase:&str)->Result<bool>{
    let m=match path.symlink_metadata(){Ok(m)=>m,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(false),Err(e)=>return Err(e.to_string())};
    if !m.is_file()||m.len()>4096{return Err("unsafe approval file".into());}
    #[cfg(unix)]{use std::os::unix::fs::MetadataExt;if m.uid()!=unsafe{libc::geteuid()}||m.mode()&0o077!=0{return Err("private same-user approval required".into());}}
    let v:Value=serde_json::from_slice(&std::fs::read(path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    if v["fixture_nonce"]!=nonce||v["phase"]!=phase||v["source"]!="MainDisplay"||v["source_revision"]!=0
        ||v["coverage_verified"]!=true||v["visual_verified"]!=true||v["fixture_pid"].as_u64().unwrap_or(0)==0
        ||v["fixture_window"].as_u64().unwrap_or(0)==0{return Err("approval must bind exact synthetic display evidence".into());}
    let expiry=v["expires_local_unix_ms"].as_u64().ok_or("approval expiry absent")?;
    if expiry<now||expiry>now+20000{return Err("approval expired or excessive lifetime".into());}
    if phase=="text" && (v["foreground_is_fixture"]!=true||v["native_click_verified"]!=true){return Err("native click and exact foreground proof required before keys".into());}
    Ok(true)
}
async fn wait_approval(link:&mut Wire,output:&std::path::Path,nonce:&str,phase:&str)->Result<()>{
    let until=Instant::now()+Duration::from_secs(45);
    loop{
        if output.with_extension("stop").exists(){return Err("coordinator stopped input".into());}
        if approval(&output.with_extension(format!("{phase}-approved.json")),nonce,phase)?{return Ok(());}
        if Instant::now()>=until{return Err(format!("no bounded {phase} approval; input not sent"));}
        settle(link,Duration::from_millis(100)).await?;
    }
}
async fn staged_input(link:&mut Wire,output:&std::path::Path,nonce:&str,report:&mut Value)->Result<()>{
    if report["decoded_frames"].as_u64().unwrap_or(0)<4||report["decode_errors"]!=0{return Err("clean decoded synthetic video required before input".into());}
    super::phase_receipt(output,"video-ready",report)?;
    wait_approval(link,output,nonce,"click").await?;
    for event in [InputEvent::MouseMoveAbsolute{x:32767,y:32767},InputEvent::MouseDown(0),InputEvent::MouseUp(0)]{
        link.send(SessionCommand::SourceInput{id:ID,source_revision:0,event}).await?;
        report["input_commands_enqueued"]=json!(report["input_commands_enqueued"].as_u64().unwrap()+1);
        settle(link,Duration::from_millis(120)).await?;
    }
    super::phase_receipt(output,"clicked",report)?;
    wait_approval(link,output,nonce,"text").await?;
    let text=format!("731{:05}",u16::from_str_radix(&nonce[..4],16).map_err(|_|"invalid fixture nonce")?);report["expected_text"]=text.clone().into();
    for c in text.bytes(){let code=ascii_key(c).ok_or("unsupported synthetic text")?;
        for pressed in [true,false]{
            link.send(SessionCommand::SourceInput{id:ID,source_revision:0,event:InputEvent::Key{key_code:code,pressed,modifiers:0}}).await?;
            report["input_commands_enqueued"]=json!(report["input_commands_enqueued"].as_u64().unwrap()+1);
            settle(link,Duration::from_millis(120)).await?;
        }
    }
    settle(link,Duration::from_millis(400)).await?;
    report["post_input_luma_exported"]=link.export_frame(&output.with_extension("input.pgm"))?.into();
    Ok(())
}


pub(super) fn replay_cli(args:&[String]){
 let result=(||->Result<Value>{
  if args.len()!=5||args[4]!="--skip-initial-access-unit"{return Err("replay-hevc SYNTHETIC_BOUNDARY_JSON NEW_OUTPUT_JSON --skip-initial-access-unit required".into());}
  let input=PathBuf::from(&args[2]);let output=PathBuf::from(&args[3]);
  if !input.is_absolute()||!output.is_absolute()||output.exists(){return Err("absolute input and fresh output required".into());}
  let v:Value=serde_json::from_slice(&std::fs::read(&input).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
  if v["kind"]!="coordinated-synthetic-HEVC-boundary-diagnostic"{return Err("only existing synthetic boundary sample supported".into());}
  let sample=std::fs::read(input.with_extension("hevc")).map_err(|e|e.to_string())?;
  if sample.len()>4*1024*1024{return Err("sample bound exceeded".into());}
  let rows=v["access_units"].as_array().ok_or("access-unit rows absent")?;
  let count=v["sampled_units"].as_u64().ok_or("sample count absent")? as usize;
  if count>32||count>rows.len(){return Err("invalid unit bound".into());}
  let mut offset:usize=0;let mut packets=Vec::new();
  for row in rows.iter().take(count){let n=row["payload_bytes"].as_u64().ok_or("unit length absent")? as usize;
   let end=offset.checked_add(n).ok_or("unit overflow")?;let bytes=sample.get(offset..end).ok_or("sample length mismatch")?.to_vec();offset=end;
   packets.push(protocol::RtpPacket{header:protocol::RtpHeader{version:2,payload_type:96,sequence_number:row["sequence"].as_u64().ok_or("sequence absent")? as u16,timestamp:1,ssrc:256},payload:bytes});
  }
  if offset!=sample.len(){return Err("unaccounted sample bytes".into());}
  let rt=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
  rt.block_on(async{
   let stats=Statistics::new();let media=client::ClientMediaRuntime::start_video_only(stats.clone()).map_err(|e|e.to_string())?;
   for packet in packets.into_iter().skip(1){media.decode_tx.send((packet,protocol::FrameTimingCheckpoints::default())).await.map_err(|_|"decoder closed")?;tokio::time::sleep(Duration::from_millis(80)).await;}
   tokio::time::sleep(Duration::from_millis(600)).await;
   let result=json!({"kind":"offline-native-HEVC-boundary-replay","network_or_input_used":false,"initial_access_unit_omitted":true,
    "decoded_frames":stats.video_frames_decoded.load(Relaxed),"decode_errors":stats.video_decode_errors.load(Relaxed),"decoder_status":media.decode_status.lock().unwrap().clone()});
   new_private_receipt(&output).map_err(|e|e.to_string())?.write_all(serde_json::to_vec_pretty(&result).unwrap().as_slice()).map_err(|e|e.to_string())?;
   Ok(result)
  })
 })();
 match result{Ok(v)=>println!("{v}"),Err(e)=>{eprintln!("{e}");std::process::exit(1)}}
}

#[cfg(test)]mod tests{
 use super::*;
 #[test]fn parses_three_and_four_byte_annex_b(){assert_eq!(nal_types(&[0,0,0,1,64,1,9,0,0,1,66,1,9,0,0,1,68,1,9,0,0,1,40,1]),vec![32,33,34,20]);}
 #[test]fn ascii_text_cannot_emit_shortcuts(){for c in b"abc0123456789def"{assert!(ascii_key(*c).is_some());}for c in b"rz 
	"{assert!(ascii_key(*c).is_none());}}
 #[test]fn approvals_require_exact_phase_nonce_focus_and_short_expiry(){
  let d=tempfile::tempdir().unwrap();let p=d.path().join("approval");let nonce="0123456789abcdef0123456789abcdef";
  assert!(!approval(&p,nonce,"click").unwrap());
  let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
  let mut v=json!({"fixture_nonce":nonce,"phase":"text","source":"MainDisplay","source_revision":0,"coverage_verified":true,"visual_verified":true,"fixture_pid":12,"fixture_window":13,"expires_local_unix_ms":now+10000,"foreground_is_fixture":true,"native_click_verified":true});
  let write=|v:&Value|{if p.exists(){std::fs::remove_file(&p).unwrap();}new_private_receipt(&p).unwrap().write_all(serde_json::to_vec(v).unwrap().as_slice()).unwrap();};
  write(&v);assert!(approval(&p,nonce,"text").unwrap());assert!(approval(&p,nonce,"click").is_err());assert!(approval(&p,"ffffffffffffffffffffffffffffffff","text").is_err());
  v["foreground_is_fixture"]=false.into();write(&v);assert!(approval(&p,nonce,"text").is_err());v["foreground_is_fixture"]=true.into();v["expires_local_unix_ms"]=0.into();write(&v);assert!(approval(&p,nonce,"text").is_err());
 }
 #[test]fn avcc_is_not_silently_assumed_annex_b(){assert!(nal_types(&[0,0,0,2,64,1]).is_empty());}
}
