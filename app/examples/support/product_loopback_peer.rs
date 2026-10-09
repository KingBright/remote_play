//! Explicit loopback Session-v2 fixture. Production transport and authentication,
//! generated encoded content; no system capture or input injector.
#[path = "../../../client/src/hevc_sequence.rs"]
mod hevc_sequence;
use protocol::{ControlMessage, PayloadType, RtpHeader, RtpPacket, session::*};
use remote_core::{
    discovery::*,
    net::{MultiplexedPacket, UdpMultiplexer, UdpSender},
    session_crypto::*,
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
pub struct Fixture {
    peers: Vec<(SocketAddr, DiscoveryAnnouncement)>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    pub async fn announce(&mut self, target: SocketAddr) -> Result<(), Error> {
        if !target.ip().is_loopback() {
            return Err("fixture discovery must remain loopback".into());
        }
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let packets: Vec<_> = self
            .peers
            .iter()
            .map(|(_, p)| p.encode())
            .collect::<Result<_, _>>()?;
        self.tasks.push(tokio::spawn(async move {
            for _ in 0..400 {
                for packet in &packets {
                    let _ = socket.send_to(packet, target).await;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }));
        Ok(())
    }
}
struct Subscription {
    request: SubscriptionRequest,
    source_revision: u32,
    video: bool,
    audio: bool,
}
struct Connection {
    id: u32,
    subscriptions: BTreeMap<u32, Subscription>,
}
struct Delayed {
    at: Instant,
    addr: SocketAddr,
    connection: u32,
    id: u32,
    request: u32,
    source: CaptureSource,
}
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Faults {
    id: u64,
    allow_retry_handshake: bool,
    delay_switch_ms: u64,
    pause_video: bool,
    close_connection: bool,
}
fn sources() -> Vec<CaptureSourceInfo> {
    [
        (101, "Blue generated source"),
        (202, "Green generated source"),
        (303, "Timeout source (no acknowledgement)"),
    ]
    .map(|(id, title)| CaptureSourceInfo {
        source: CaptureSource::Window(id),
        title: title.into(),
        application: "Loopback acceptance".into(),
        process_id: Some(std::process::id() as i32),
        width: 640,
        height: 360,
        supports_input: true,
    })
    .into()
}
async fn reply(sender: &UdpSender, addr: SocketAddr, command: SessionCommand) {
    let _ = sender
        .send_control(&ControlMessage::Session(Box::new(command)), addr)
        .await;
}
fn clip(root: &Path, color: &str) -> Result<Vec<u8>, Error> {
    let path = root.join(format!("{color}.hevc"));
    let error = std::fs::File::create(root.join(format!("encode-{color}.log")))?;
    let mut command = std::process::Command::new("/opt/homebrew/bin/ffmpeg");
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!("color=c={color}:s=640x360:r=20"))
        .args([
            "-frames:v",
            "1",
            "-c:v",
            "hevc_videotoolbox",
            "-pix_fmt",
            "nv12",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-g",
            "1",
            "-b:v",
            "2500k",
            "-f",
            "hevc",
        ])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(error);
    let mut child = command.spawn()?;
    let begun = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!("fixture encoder failed: {color}").into());
            }
            break;
        }
        if begun.elapsed() > Duration::from_secs(12) {
            child.kill()?;
            child.wait()?;
            return Err("owned fixture encoder timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let bytes = std::fs::read(&path)?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err("invalid bounded encoded fixture".into());
    }
    let sequence = hevc_sequence::sequence_info(&bytes)?.ok_or("fixture SPS missing")?;
    let signal = sequence.signal.ok_or("fixture color metadata missing")?;
    if signal.primaries != Some(1)
        || signal.transfer != Some(1)
        || signal.matrix != Some(1)
        || signal.full_range
    {
        return Err("fixture SPS must explicitly carry limited BT.709".into());
    }
    std::fs::write(
        root.join(format!("color-{color}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "matrix":signal.matrix,"primaries":signal.primaries,"transfer":signal.transfer,"full_range":signal.full_range,
            "coded":sequence.coded,"visible":sequence.visible,"encoded_bytes":bytes.len(),"encoder":"hevc_videotoolbox"
        }))?,
    )?;
    Ok(bytes)
}
pub async fn start(root: &Path, network: &str) -> Result<Fixture, Error> {
    let location = root.to_owned();
    let clips = Arc::new(
        tokio::task::spawn_blocking(move || {
            Ok::<_, Error>([clip(&location, "blue")?, clip(&location, "green")?])
        })
        .await??,
    );
    let psk = Arc::new(load_session_psk().ok_or("isolated paired test identity not initialized")?);
    let mut fixture = Fixture {
        peers: vec![],
        tasks: vec![],
    };
    for (label, id, initially_silent) in [
        (
            "Protocol video peer",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            false,
        ),
        (
            "Retry handshake peer",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            true,
        ),
    ] {
        let mux = UdpMultiplexer::bind("127.0.0.1:0").await?;
        let address = mux.local_addr()?;
        let (sender, receiver) = mux.split();
        fixture.peers.push((
            address,
            DiscoveryAnnouncement {
                network_name: network.into(),
                device_id: id.into(),
                display_name: label.into(),
                control_port: address.port(),
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_stream: true,
                    can_view: true,
                    ..Default::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: Duration::from_secs(4),
            },
        ));
        let root = root.to_owned();
        let psk = psk.clone();
        let clips = clips.clone();
        fixture.tasks.push(tokio::spawn(async move {
            if let Err(error) = serve(
                root.clone(),
                id.into(),
                initially_silent,
                sender,
                receiver,
                psk,
                clips,
            )
            .await
            {
                let _ =
                    std::fs::write(root.join(format!("peer-{id}-error.txt")), error.to_string());
            }
        }));
    }
    Ok(fixture)
}
async fn serve(
    root: PathBuf,
    label: String,
    silent: bool,
    sender: UdpSender,
    receiver: remote_core::net::UdpReceiver,
    psk: Arc<Vec<u8>>,
    clips: Arc<[Vec<u8>; 2]>,
) -> Result<(), Error> {
    let begun = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut faults = Faults::default();
    let mut authenticated = BTreeMap::new();
    let mut connections: BTreeMap<SocketAddr, Connection> = BTreeMap::new();
    let mut delayed: Vec<Delayed> = vec![];
    let mut events = vec![];
    let mut input = vec![];
    let mut hello_count = 0u64;
    let mut open_count = 0u64;
    let mut packets = 0u64;
    let mut sequence = 0u16;
    let mut last_command = 0u64;
    loop {
        if begun.elapsed() > Duration::from_secs(240) {
            break;
        }
        tokio::select! {
            _=tick.tick()=>{
                if let Ok(bytes)=std::fs::read(root.join("faults.json")) && bytes.len()<4096
                    && let Ok(next)=serde_json::from_slice::<Faults>(&bytes) && next.id>last_command {
                    last_command=next.id; faults=next; events.push(serde_json::json!({"fault_command":last_command}));
                    if faults.close_connection {
                        for (addr,c) in &connections { reply(&sender,*addr,SessionCommand::Closed{connection_id:c.id,reason:"explicit loopback disconnect".into()}).await; }
                        connections.clear(); delayed.clear();
                    }
                }
                let now=Instant::now(); let mut waiting=vec![];
                for response in delayed.drain(..) {
                    if response.at>now { waiting.push(response); continue; }
                    if let Some(c)=connections.get_mut(&response.addr) && c.id==response.connection
                        && let Some(s)=c.subscriptions.get_mut(&response.id) {
                        s.request.source=response.source; s.source_revision=response.request;
                        reply(&sender,response.addr,SessionCommand::SourceSwitched{id:response.id,request_id:response.request,source:response.source,supports_input:true}).await;
                    }
                }
                delayed=waiting;
                if !faults.pause_video {
                    for (addr,c) in &connections { for (id,s) in &c.subscriptions { if !s.video {continue;}
                        let green=matches!(s.request.source,CaptureSource::Window(202));
                        let packet=RtpPacket{header:RtpHeader{version:2,payload_type:PayloadType::VideoH265 as u8,
                            sequence_number:sequence,timestamp:now_unix_ms() as u32,ssrc:*id},payload:clips[usize::from(green)].clone()};
                        sequence=sequence.wrapping_add(1); sender.send_rtp(&packet,*addr).await?; packets+=1;
                    }}
                }
                let receipt=serde_json::json!({"elapsed_ms":begun.elapsed().as_millis(),"hello_count":hello_count,"open_count":open_count,
                    "packets":packets,"fault_command":last_command,"input":input,"events":events,
                    "connections":connections.values().map(|c|serde_json::json!({"connection_id":c.id,
                        "subscriptions":c.subscriptions.values().map(|s|serde_json::json!({"id":s.request.id,"source":format!("{:?}",s.request.source),
                            "source_revision":s.source_revision,"video":s.video,"audio":s.audio})).collect::<Vec<_>>() })).collect::<Vec<_>>()});
                std::fs::write(root.join(format!("peer-{label}.json")),serde_json::to_vec_pretty(&receipt)?)?;
            }
            received=receiver.recv()=>{
                let packet=match received {Ok(p)=>p,Err(_)=>continue};
                match packet {
                    MultiplexedPacket::Control(ControlMessage::SessionHello{nonce,timestamp_ms,mac},addr) if addr.ip().is_loopback()=>{
                        hello_count+=1;
                        if (silent && !faults.allow_retry_handshake) || (!silent && hello_count==1) {continue;}
                        verify_session_mac(&mac_session_hello(&psk,&nonce,timestamp_ms),&mac,timestamp_ms,now_unix_ms())?;
                        let (salt,ts)=if let Some((old,salt,ts))=authenticated.get(&addr) {
                            if *old!=nonce && connections.contains_key(&addr) {continue;} (*salt,*ts)
                        } else { let salt=random_bytes_16();let ts=now_unix_ms();sender.install_peer_crypto(addr,SessionCrypto::from_psk(&psk,&salt)?);authenticated.insert(addr,(nonce,salt,ts));(salt,ts) };
                        sender.send_control(&ControlMessage::SessionAccept{salt,timestamp_ms:ts,mac:mac_session_accept(&psk,&salt,ts)},addr).await?;
                    }
                    MultiplexedPacket::Control(ControlMessage::Ping{client_send_ts},addr) if authenticated.contains_key(&addr)=>{
                        let ts=now_unix_ms();sender.send_control(&ControlMessage::Pong{client_send_ts,host_recv_ts:ts,host_send_ts:ts},addr).await?;
                    }
                    MultiplexedPacket::Control(ControlMessage::Session(command),addr) if authenticated.contains_key(&addr)=>{
                        let command=*command; events.push(serde_json::json!({"at_ms":begun.elapsed().as_millis(),"command":format!("{command:?}")}));
                        if events.len()>256 {events.remove(0);}
                        match command {
                            SessionCommand::Open{connection_id,version} if version==SESSION_VERSION=>{
                                open_count+=1;connections.entry(addr).or_insert(Connection{id:connection_id,subscriptions:BTreeMap::new()});
                                if !silent && open_count==1 {continue;}
                                reply(&sender,addr,SessionCommand::Opened{connection_id,version,max_subscriptions:8,files:false,clipboard:false,window_capture:true}).await;
                            }
                            SessionCommand::ListSources{request_id}=>reply(&sender,addr,SessionCommand::Sources{request_id,sources:sources()}).await,
                            SessionCommand::Subscribe(request)=>{
                                if let Some(c)=connections.get_mut(&addr) {
                                    let id=request.id;let audio=request.audio;
                                    c.subscriptions.entry(id).or_insert(Subscription{request,source_revision:0,video:true,audio});
                                    reply(&sender,addr,SessionCommand::Subscribed{id,audio_owner:None,supports_input:true}).await;
                                }
                            }
                            SessionCommand::SwitchSource{id,request_id,source}=>{
                                if matches!(source,CaptureSource::Window(303)) {continue;}
                                if let Some(c)=connections.get(&addr) {
                                    delayed.push(Delayed{at:Instant::now()+Duration::from_millis(faults.delay_switch_ms.min(2000)),addr,connection:c.id,id,request:request_id,source});
                                }
                            }
                            SessionCommand::Configure{id,revision,..}=>reply(&sender,addr,SessionCommand::Configured{id,revision}).await,
                            SessionCommand::SetActivity{id,revision,video,audio}=>{
                                if let Some(s)=connections.get_mut(&addr).and_then(|c|c.subscriptions.get_mut(&id)) {s.video=video;s.audio=audio;}
                                reply(&sender,addr,SessionCommand::Activity{id,revision,video,audio}).await;
                            }
                            SessionCommand::SourceInput{id,source_revision,event}=>{
                                let accepted=connections.get(&addr).and_then(|c|c.subscriptions.get(&id))
                                    .is_some_and(|s|s.source_revision==source_revision && s.video && matches!(s.request.source,CaptureSource::Window(_)));
                                input.push(serde_json::json!({"at_ms":begun.elapsed().as_millis(),"id":id,"revision":source_revision,"event":format!("{event:?}"),"accepted":accepted}));
                                if input.len()>128 {input.remove(0);}
                            }
                            SessionCommand::Input{id,event}=>{
                                let accepted=connections.get(&addr).and_then(|c|c.subscriptions.get(&id)).is_some_and(|s|s.video && !matches!(s.request.source,CaptureSource::Window(_)));
                                input.push(serde_json::json!({"id":id,"event":format!("{event:?}"),"accepted":accepted}));
                            }
                            SessionCommand::ReleaseInput{id}=>reply(&sender,addr,SessionCommand::InputReleased{id}).await,
                            SessionCommand::Unsubscribe{id}=>{
                                if let Some(c)=connections.get_mut(&addr) {c.subscriptions.remove(&id);}
                                reply(&sender,addr,SessionCommand::Unsubscribed{id}).await;
                            }
                            SessionCommand::Close{connection_id}=>{
                                if connections.get(&addr).is_some_and(|c|c.id==connection_id) {connections.remove(&addr);}
                            }
                            _=>{}
                        }
                    }
                    _=>{}
                }
            }
        }
    }
    Ok(())
}
