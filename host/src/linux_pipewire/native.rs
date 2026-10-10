//! Linux SDK-backed implementation. No default PipeWire socket is opened.
//! All native objects and borrowed buffers live on one dedicated worker thread.
use super::{CopyContract, MappedChunk, WorkerError, validate_chunk_flags};
use crate::linux_frame::{
    Crop, FrameFormat, FrameMailbox, MAX_FRAME_BYTES, OwnedFrame, PixelFormat, SourceColor,
    Transform,
};
use crate::linux_portal::runtime::{PortalCallError, PortalCaptureLease, PreparedPortalCapture};
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::buffer::{
    DataFlags, DataType,
    meta::{MetaHeader, MetaHeaderFlags, MetaVideoCrop, MetaVideoTransform},
};
use std::{
    io::Cursor,
    os::fd::OwnedFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const RAW_FRAME_MAX_AGE: Duration = Duration::from_millis(250);

struct Worker {
    stop: Arc<AtomicBool>,
    mailbox: Arc<FrameMailbox>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.mailbox.close();
        // The worker's 20ms loop timer observes closure even without new pixels.
        // Joining never blocks the GUI/async executor. Native fixture acceptance
        // must additionally measure actual disconnect/FD reclamation on Linux.
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = std::thread::Builder::new()
                .name("rp-pw-reap".into())
                .spawn(move || {
                    let _ = thread.join();
                });
        }
    }
}

async fn revoked(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

/// Connect only the portal's restricted FD/node, validate the first actual pixel
/// format, then return a connection-scoped candidate. No network commit occurs.
pub async fn prepare_pipewire_capture(
    mut lease: PortalCaptureLease,
) -> Result<PreparedPortalCapture, PortalCallError> {
    let generation = lease.selected_source().generation;
    let mut revoked_rx = lease.revocation();
    let (node, fd) = lease.take_pipewire_remote()?;
    let mailbox = Arc::new(
        FrameMailbox::native_stream(generation, RAW_FRAME_MAX_AGE)
            .map_err(|_| PortalCallError::Generation)?,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (first_tx, first_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let worker_mailbox = mailbox.clone();
    let worker_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("rp-pipewire".into())
        .spawn(move || {
            let _ = run(
                fd,
                node,
                generation,
                worker_mailbox.clone(),
                worker_stop,
                first_tx,
            );
            worker_mailbox.close();
            let _ = done_tx.send(());
        })
        .map_err(|_| PortalCallError::Transport)?;
    let worker = Worker {
        stop,
        mailbox: mailbox.clone(),
        thread: Mutex::new(Some(thread)),
    };
    let first = tokio::select! {
        biased;
        _ = revoked(&mut revoked_rx) => return Err(PortalCallError::Revoked),
        result = tokio::time::timeout(FIRST_FRAME_TIMEOUT, first_rx) => result
            .map_err(|_| PortalCallError::Timeout)?
            .map_err(|_| PortalCallError::InvalidSource)?,
    };
    let prepared = lease.prepare_with_mailbox(first, mailbox)?;
    prepared.attach_worker(Box::new(worker), done_rx);
    Ok(prepared)
}

struct Data {
    contract: CopyContract,
    first: Option<oneshot::Sender<OwnedFrame>>,
    loop_: pw::main_loop::MainLoopRc,
    mailbox: Arc<FrameMailbox>,
    connected: bool,
}
impl Data {
    fn fail(&mut self) {
        self.mailbox.close();
        self.first.take();
        self.loop_.quit();
    }
}

fn format(raw: spa::param::video::VideoInfoRaw) -> Result<FrameFormat, WorkerError> {
    let pixel_format = match raw.format() {
        spa::param::video::VideoFormat::NV12 => PixelFormat::Nv12,
        spa::param::video::VideoFormat::I420 => PixelFormat::I420,
        _ => return Err(WorkerError::Format),
    };
    if raw
        .flags()
        .contains(spa::param::video::VideoFlags::MODIFIER)
        || raw.interlace_mode() != spa::param::video::VideoInterlaceMode::Progressive
        || raw.views() > 1
        || raw.as_raw().chroma_site != 0
    {
        return Err(WorkerError::Format);
    }
    let raw = raw.as_raw();
    Ok(FrameFormat {
        width: raw.size.width,
        height: raw.size.height,
        pixel_format,
        color: SourceColor {
            range: raw.color_range,
            matrix: raw.color_matrix,
            transfer: raw.transfer_function,
            primaries: raw.color_primaries,
        },
        crop: None,
        transform: Transform::Identity,
    })
}

fn run(
    fd: OwnedFd,
    node: u32,
    generation: u64,
    mailbox: Arc<FrameMailbox>,
    stop: Arc<AtomicBool>,
    first: oneshot::Sender<OwnedFrame>,
) -> Result<(), Box<dyn std::error::Error>> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(pw::init);
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let error_loop = mainloop.clone();
    let error_mailbox = mailbox.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |_, _, _, _| {
            error_mailbox.close();
            error_loop.quit();
        })
        .register();
    let stream = pw::stream::StreamBox::new(
        &core,
        "RemotePlay local share",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video", *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(Data {
            contract: CopyContract::new(generation, mailbox.clone()),
            first: Some(first),
            loop_: mainloop.clone(),
            mailbox: mailbox.clone(),
            connected: false,
        })
        .state_changed(|_, data, _, new| {
            if matches!(new, pw::stream::StreamState::Error(_))
                || (data.connected && matches!(new, pw::stream::StreamState::Unconnected))
            {
                data.fail();
            }
            if matches!(
                new,
                pw::stream::StreamState::Connecting
                    | pw::stream::StreamState::Streaming
                    | pw::stream::StreamState::Paused
            ) {
                data.connected = true;
            }
        })
        .param_changed(|stream, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let result = (|| {
                let Some(param) = param else {
                    return data.contract.renegotiate(None).map(|_| ());
                };
                let (media, subtype) = spa::param::format_utils::parse_format(param)
                    .map_err(|_| WorkerError::Format)?;
                if media != spa::param::format::MediaType::Video
                    || subtype != spa::param::format::MediaSubtype::Raw
                {
                    return Err(WorkerError::Format);
                }
                let mut raw = spa::param::video::VideoInfoRaw::default();
                raw.parse(param).map_err(|_| WorkerError::Format)?;
                data.contract.renegotiate(Some(format(raw)?))?;
                request_cpu_buffers(stream).map_err(|_| WorkerError::Buffer)
            })();
            if result.is_err() {
                data.fail();
            }
        })
        .process(|stream, data| {
            // Buffer::drop queues exactly once on every return/error path. Never hold
            // a PipeWire buffer during mailbox wait, FFmpeg write or GUI rendering.
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if data.mailbox.is_closed()
                || data.mailbox.is_paused()
                || !data.contract.is_negotiated()
            {
                return;
            }
            let header = buffer.find_meta::<MetaHeader>();
            if header.is_some_and(|header| {
                header
                    .flags()
                    .intersects(MetaHeaderFlags::CORRUPTED | MetaHeaderFlags::GAP)
            }) {
                return;
            }
            let discontinuity =
                header.is_some_and(|header| header.flags().contains(MetaHeaderFlags::DISCONT));
            let sequence = header.map(|header| header.seq());
            let pts = header.and_then(|header| (header.pts() != i64::MIN).then_some(header.pts()));
            let crop = buffer
                .find_meta::<MetaVideoCrop>()
                .filter(|crop| crop.meta_region().is_valid())
                .map(|crop| {
                    let region = crop.meta_region();
                    Crop {
                        x: region.position().x as u32,
                        y: region.position().y as u32,
                        width: region.size().width,
                        height: region.size().height,
                    }
                });
            let transform =
                buffer
                    .find_meta::<MetaVideoTransform>()
                    .map_or(Transform::Identity, |meta| {
                        match meta.transform().as_raw() {
                            0 => Transform::Identity,
                            value => Transform::Unsupported(value),
                        }
                    });
            let result = (|| {
                if discontinuity {
                    data.contract.discontinuity()?;
                }
                let datas = buffer.datas_mut();
                if datas.is_empty() || datas.len() > 3 {
                    return Err(WorkerError::Buffer);
                }
                let mut chunks = Vec::with_capacity(datas.len());
                for data in datas {
                    let raw = data.as_raw();
                    if !matches!(data.type_(), DataType::MemPtr | DataType::MemFd)
                        || !data.flags().contains(DataFlags::READABLE)
                        || raw.data.is_null()
                        || raw.chunk.is_null()
                        || raw.maxsize as usize > MAX_FRAME_BYTES
                    {
                        return Err(WorkerError::Buffer);
                    }
                    let chunk = data.chunk();
                    // Reject neutral/recycled storage before forming a byte
                    // slice, even when size and stride look like a real frame.
                    let flags = chunk.flags().bits();
                    validate_chunk_flags(flags)?;
                    // MAP_BUFFERS supplies data.data at the mapping's mapoffset.
                    // Read-only slices allow legitimate overlapping plane mappings;
                    // no unchecked offset is added and no native pointer escapes.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(raw.data.cast::<u8>(), raw.maxsize as usize)
                    };
                    chunks.push(MappedChunk {
                        bytes,
                        flags,
                        mapping_offset: raw.mapoffset,
                        offset: chunk.offset(),
                        size: chunk.size(),
                        stride: chunk.stride(),
                    });
                }
                data.contract.copy(
                    data.contract.revision(),
                    &chunks,
                    crop,
                    transform,
                    sequence,
                    pts,
                    remote_core::quanta_now_us(),
                )
            })();
            // Return buffer before handing owned bytes across the thread boundary.
            drop(buffer);
            match result {
                Ok(frame) => {
                    if let Some(first) = data.first.take() {
                        if first.send(frame).is_err() {
                            data.fail();
                        }
                    } else {
                        data.mailbox.publish(frame);
                    }
                }
                Err(_) => data.fail(),
            }
        })
        .register()?;
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::NV12,
            spa::param::video::VideoFormat::NV12,
            spa::param::video::VideoFormat::I420
        )
    );
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("invalid format pod")?;
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [pod],
    )?;
    let timer_loop = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop.load(Ordering::Acquire) || mailbox.is_closed() {
            timer_loop.quit();
        }
    });
    timer
        .update_timer(
            Some(Duration::from_millis(1)),
            Some(Duration::from_millis(20)),
        )
        .into_result()?;
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

fn request_cpu_buffers(stream: &pw::stream::Stream) -> Result<(), Box<dyn std::error::Error>> {
    use spa::pod::{ChoiceValue, Object, Property, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id};
    let mask = ((1u32 << DataType::MemPtr.as_raw()) | (1u32 << DataType::MemFd.as_raw())) as i32;
    let mut objects = vec![Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa::sys::SPA_PARAM_Buffers,
        properties: vec![Property::new(
            spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Flags {
                    default: mask,
                    flags: vec![mask],
                },
            ))),
        )],
    }];
    for (kind, size) in [
        (
            spa::sys::SPA_META_Header,
            std::mem::size_of::<spa::sys::spa_meta_header>(),
        ),
        (
            spa::sys::SPA_META_VideoCrop,
            std::mem::size_of::<spa::sys::spa_meta_region>(),
        ),
        (
            spa::sys::SPA_META_VideoTransform,
            std::mem::size_of::<spa::sys::spa_meta_videotransform>(),
        ),
    ] {
        objects.push(Object {
            type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
            id: spa::sys::SPA_PARAM_Meta,
            properties: vec![
                Property::new(spa::sys::SPA_PARAM_META_type, Value::Id(Id(kind))),
                Property::new(spa::sys::SPA_PARAM_META_size, Value::Int(size as i32)),
            ],
        });
    }
    let bytes = objects
        .into_iter()
        .map(|object| {
            spa::pod::serialize::PodSerializer::serialize(
                Cursor::new(Vec::new()),
                &Value::Object(object),
            )
            .map(|result| result.0.into_inner())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pods = bytes
        .iter()
        .map(|bytes| spa::pod::Pod::from_bytes(bytes).ok_or("invalid buffers pod"))
        .collect::<Result<Vec<_>, _>>()?;
    stream.update_params(&mut pods)?;
    Ok(())
}
