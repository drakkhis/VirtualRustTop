use crate::nvenc::{
    CudaEglInterop, CudaMappedFrame, EglDmaBufImporter, NvEncSession, DRM_FORMAT_ABGR8888,
    DRM_FORMAT_ARGB8888, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_XBGR8888, DRM_FORMAT_XRGB8888,
    NV_ENC_BUFFER_FORMAT_ABGR,
};
use anyhow::{Context, Result};
use bytes::Bytes;
use libspa::buffer::{ChunkFlags, DataType};
use libspa::param::format::{MediaSubtype, MediaType};
use libspa::param::video::{VideoFormat, VideoInfoRaw};
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use libspa::sys::{
    SPA_DATA_DmaBuf, SPA_FORMAT_VIDEO_format, SPA_FORMAT_VIDEO_framerate,
    SPA_FORMAT_VIDEO_modifier, SPA_FORMAT_VIDEO_size, SPA_PARAM_BUFFERS_dataType,
    SPA_PARAM_Buffers, SPA_PARAM_EnumFormat, SPA_PARAM_Format, SPA_TYPE_OBJECT_Format,
    SPA_TYPE_OBJECT_ParamBuffers,
};
use libspa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Rectangle};
use pipewire::context::ContextBox;
use pipewire::main_loop::MainLoopBox;
use pipewire::properties::properties;
use pipewire::stream::{StreamBox, StreamFlags};
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::{OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use vdesk_protocol::VideoCodec;

#[derive(Debug)]
pub struct EncodedFrame {
    pub stream_id: u32,
    pub width: u32,
    pub height: u32,
    pub frame_index: u64,
    pub capture_timestamp_us: u64,
    pub is_idr: bool,
    pub payload: Bytes,
}

pub struct StreamControl {
    pub force_idr: AtomicBool,
    pub target_bitrate_kbps: AtomicU32,
    pub active: AtomicBool,
    pub default_bitrate_kbps: AtomicU32,
    pub min_bitrate_kbps: AtomicU32,
    pub max_bitrate_kbps: AtomicU32,
}

impl StreamControl {
    pub fn new(default_kbps: u32, min_kbps: u32, max_kbps: u32) -> Self {
        Self {
            force_idr: AtomicBool::new(false),
            target_bitrate_kbps: AtomicU32::new(default_kbps),
            active: AtomicBool::new(true),
            default_bitrate_kbps: AtomicU32::new(default_kbps),
            min_bitrate_kbps: AtomicU32::new(min_kbps),
            max_bitrate_kbps: AtomicU32::new(max_kbps),
        }
    }

    pub fn update_bounds(&self, default_kbps: u32, min_kbps: u32, max_kbps: u32) {
        self.default_bitrate_kbps
            .store(default_kbps, Ordering::Relaxed);
        self.min_bitrate_kbps.store(min_kbps, Ordering::Relaxed);
        self.max_bitrate_kbps.store(max_kbps, Ordering::Relaxed);
        self.target_bitrate_kbps
            .store(default_kbps, Ordering::Relaxed);
    }

    pub fn default_kbps(&self) -> u32 {
        self.default_bitrate_kbps.load(Ordering::Relaxed)
    }

    pub fn min_kbps(&self) -> u32 {
        self.min_bitrate_kbps.load(Ordering::Relaxed)
    }

    pub fn max_kbps(&self) -> u32 {
        self.max_bitrate_kbps.load(Ordering::Relaxed)
    }
}

pub fn default_bitrate_bounds(
    width: u32,
    height: u32,
    override_kbps: Option<u32>,
) -> (u32, u32, u32) {
    let pixels = u64::from(width.max(1)) * u64::from(height.max(1));
    let default = match override_kbps {
        Some(v) => v.clamp(6_000, 120_000),
        None => ((pixels * 35_000) / (2560 * 1440)).clamp(15_000, 60_000) as u32,
    };
    let min = (default / 4).clamp(6_000, 12_000).min(default);
    let max = (default * 7 / 4).clamp(default, 80_000.max(default));
    (default, min, max)
}

struct RegisteredDmaBuf {
    egl_image: *mut c_void,
    gl_tex: u32,
    stride: u32,
    offset: u32,
}

struct TargetResources {
    dst_gl_tex: u32,
    cuda_res: *mut c_void,
    nvenc_res: *mut c_void,
}

struct CaptureState {
    stream_id: u32,
    codec: VideoCodec,
    max_fps: u32,
    bitrate_override: Option<u32>,
    bitrate_kbps: u32,
    width: u32,
    height: u32,
    drm_fourcc: u32,
    drm_modifier: u64,
    egl: EglDmaBufImporter,
    cuda: CudaEglInterop,
    nvenc: Option<NvEncSession>,
    target: Option<TargetResources>,
    registered: HashMap<RawFd, RegisteredDmaBuf>,
    frame_index: u64,
    control: Arc<StreamControl>,
    active_clients: Arc<AtomicUsize>,
    last_encoded_at: Option<Instant>,
    start_time: Instant,
    frame_tx: broadcast::Sender<Arc<EncodedFrame>>,
}

impl CaptureState {
    fn cleanup_dmabufs(&mut self) {
        for (_, reg) in self.registered.drain() {
            self.egl.destroy_dmabuf_texture(reg.egl_image, reg.gl_tex);
        }
    }

    fn cleanup_target(&mut self) {
        if let Some(target) = self.target.take() {
            if let Some(ref nvenc) = self.nvenc {
                nvenc.unregister_resource(target.nvenc_res);
            }
            self.cuda.unregister_resource(target.cuda_res);
            self.egl.destroy_texture(target.dst_gl_tex);
        }
    }
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        self.cleanup_dmabufs();
        self.cleanup_target();
    }
}

pub struct CaptureHandle {
    quit_tx: pipewire::channel::Sender<()>,
    join: Option<std::thread::JoinHandle<Result<()>>>,
}

impl CaptureHandle {
    pub fn stop_and_join(&mut self) {
        let _ = self.quit_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_capture_thread(
    pipewire_fd: Option<OwnedFd>,
    node_id: u32,
    codec: VideoCodec,
    max_fps: u32,
    bitrate_override: Option<u32>,
    control: Arc<StreamControl>,
    active_clients: Arc<AtomicUsize>,
    frame_tx: broadcast::Sender<Arc<EncodedFrame>>,
) -> CaptureHandle {
    let (quit_tx, quit_rx) = pipewire::channel::channel();
    let join = std::thread::spawn(move || {
        run_pipewire_loop(
            pipewire_fd,
            node_id,
            codec,
            max_fps,
            bitrate_override,
            control,
            active_clients,
            frame_tx,
            quit_rx,
        )
    });
    CaptureHandle {
        quit_tx,
        join: Some(join),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pipewire_loop(
    pipewire_fd: Option<OwnedFd>,
    node_id: u32,
    codec: VideoCodec,
    max_fps: u32,
    bitrate_override: Option<u32>,
    control: Arc<StreamControl>,
    active_clients: Arc<AtomicUsize>,
    frame_tx: broadcast::Sender<Arc<EncodedFrame>>,
    quit_rx: pipewire::channel::Receiver<()>,
) -> Result<()> {
    pipewire::init();

    let egl = EglDmaBufImporter::new()?;
    let modifiers = egl.query_dmabuf_modifiers(DRM_FORMAT_XRGB8888);
    info!(
        "Queried {} EGL DMA-BUF modifiers for XRGB8888",
        modifiers.len()
    );
    let cuda = CudaEglInterop::new()?;

    let mainloop =
        std::rc::Rc::new(MainLoopBox::new(None).context("Failed to create PipeWire MainLoop")?);
    let context = ContextBox::new(mainloop.loop_(), None).context("Failed to create PW Context")?;
    let core = match pipewire_fd {
        Some(fd) => context
            .connect_fd(fd, None)
            .context("Failed to connect to PipeWire remote FD")?,
        None => context
            .connect(None)
            .context("Failed to connect to PipeWire daemon")?,
    };

    let stream = StreamBox::new(
        &core,
        "vdesk-capture",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        },
    )
    .context("Failed to create PipeWire stream")?;

    let initial_kbps = control.default_kbps();
    let state = CaptureState {
        stream_id: node_id,
        codec,
        max_fps: max_fps.max(30),
        bitrate_override,
        bitrate_kbps: initial_kbps,
        width: 0,
        height: 0,
        drm_fourcc: DRM_FORMAT_XRGB8888,
        drm_modifier: DRM_FORMAT_MOD_INVALID,
        egl,
        cuda,
        nvenc: None,
        target: None,
        registered: HashMap::new(),
        frame_index: 0,
        control,
        active_clients,
        last_encoded_at: None,
        start_time: Instant::now(),
        frame_tx,
    };

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(|_stream, _state, old, new| {
            info!("PipeWire stream state: {:?} -> {:?}", old, new);
        })
        .param_changed(|stream, state, id, param| {
            if id != SPA_PARAM_Format {
                return;
            }
            let Some(param) = param else { return };

            let mut info = VideoInfoRaw::new();
            if let Err(e) = info.parse(param) {
                warn!("Failed to parse SPA_FORMAT_VIDEO_raw: {:?}", e);
                return;
            }

            let modifier = info.modifier();
            let size = info.size();
            let fourcc = match info.format() {
                VideoFormat::BGRx => DRM_FORMAT_XRGB8888,
                VideoFormat::BGRA => DRM_FORMAT_ARGB8888,
                VideoFormat::RGBx => DRM_FORMAT_XBGR8888,
                VideoFormat::RGBA => DRM_FORMAT_ABGR8888,
                other => {
                    warn!("Unsupported PipeWire VideoFormat {:?}, skipping", other);
                    return;
                }
            };

            info!(
                "PipeWire stream {} negotiated {}x{} fmt={:?} (fourcc=0x{:08x}, mod=0x{:016x})",
                state.stream_id,
                size.width,
                size.height,
                info.format(),
                fourcc,
                modifier
            );

            if state.width != size.width
                || state.height != size.height
                || state.nvenc.is_none()
                || state.target.is_none()
            {
                state.cleanup_dmabufs();
                state.cleanup_target();
                state.nvenc = None;
                state.width = size.width;
                state.height = size.height;
                state.drm_fourcc = fourcc;
                state.drm_modifier = modifier;

                let fps = if info.framerate().num > 0 && info.framerate().denom > 0 {
                    (info.framerate().num / info.framerate().denom).clamp(30, state.max_fps)
                } else {
                    state.max_fps
                };

                let (def_kbps, min_kbps, max_kbps) =
                    default_bitrate_bounds(size.width, size.height, state.bitrate_override);
                state.control.update_bounds(def_kbps, min_kbps, max_kbps);
                state.bitrate_kbps = def_kbps;

                let enc = match NvEncSession::new(
                    state.cuda.ctx,
                    size.width,
                    size.height,
                    fps,
                    state.bitrate_kbps,
                    state.codec,
                    NV_ENC_BUFFER_FORMAT_ABGR,
                ) {
                    Ok(enc) => enc,
                    Err(e) => {
                        error!("Failed to initialize NVENC session: {:#}", e);
                        return;
                    }
                };

                let dst_gl_tex = match state
                    .egl
                    .create_cuda_target_texture(size.width, size.height)
                {
                    Ok(tex) => tex,
                    Err(e) => {
                        error!("Failed to create CUDA target GL texture: {:#}", e);
                        return;
                    }
                };

                let (cuda_res, cu_arr) = match state.cuda.register_gl_texture(dst_gl_tex) {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Failed to register GL target texture with CUDA: {:#}", e);
                        state.egl.destroy_texture(dst_gl_tex);
                        return;
                    }
                };

                let nvenc_res = match enc.register_cuda_frame(&CudaMappedFrame::Array(cu_arr)) {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Failed to register CUDA array with NVENC: {:#}", e);
                        state.cuda.unregister_resource(cuda_res);
                        state.egl.destroy_texture(dst_gl_tex);
                        return;
                    }
                };

                state.nvenc = Some(enc);
                state.target = Some(TargetResources {
                    dst_gl_tex,
                    cuda_res,
                    nvenc_res,
                });

                // Request strictly DMA-BUF buffers (no MemPtr fallback)
                let mut buf = Vec::new();
                if let Ok(pod_bytes) = build_dmabuf_buffer_params(&mut buf) {
                    if let Some(pod) = Pod::from_bytes(pod_bytes) {
                        let _ = stream.update_params(&mut [pod]);
                    }
                }
            } else if state.drm_fourcc != fourcc || state.drm_modifier != modifier {
                state.cleanup_dmabufs();
                state.drm_fourcc = fourcc;
                state.drm_modifier = modifier;
            }
        })
        .remove_buffer(|_stream, state, buffer| unsafe {
            let spa_buf = (*buffer).buffer;
            if spa_buf.is_null() {
                return;
            }
            let datas = (*spa_buf).datas;
            let n_datas = (*spa_buf).n_datas;
            if n_datas > 0 && !datas.is_null() {
                let fd = (*datas).fd as RawFd;
                if let Some(reg) = state.registered.remove(&fd) {
                    state.egl.destroy_dmabuf_texture(reg.egl_image, reg.gl_tex);
                    debug!("Unregistered DMA-BUF fd={}", fd);
                }
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &datas[0];
            if data.type_() != DataType::DmaBuf {
                warn!(
                    "Ignoring non-DMA-BUF PipeWire buffer (type={:?})",
                    data.type_()
                );
                return;
            }

            let fd = data.as_raw().fd as RawFd;
            let chunk = data.chunk();
            if chunk.flags().contains(ChunkFlags::CORRUPTED) || chunk.size() == 0 {
                return;
            }
            let stride = chunk.stride() as u32;
            let offset = chunk.offset();
            if fd < 0 || stride == 0 {
                return;
            }

            let has_clients = state.active_clients.load(Ordering::Relaxed) > 0;
            let is_active = state.control.active.load(Ordering::Relaxed);
            let wants_idr = state.control.force_idr.load(Ordering::Acquire);
            let now = Instant::now();

            if !has_clients {
                let idle_keepalive = state.frame_index == 0
                    || state
                        .last_encoded_at
                        .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(2));
                if !idle_keepalive {
                    return;
                }
            } else if !is_active {
                if state.frame_index != 0 {
                    return;
                }
            } else if !wants_idr {
                let min_interval_us = (1_000_000u64 / u64::from(state.max_fps.max(1))) * 92 / 100;
                if let Some(last) = state.last_encoded_at {
                    if (now.duration_since(last).as_micros() as u64) < min_interval_us {
                        return;
                    }
                }
            }

            let (Some(ref mut nvenc), Some(ref target)) = (&mut state.nvenc, &state.target) else {
                return;
            };

            let cached_tex = match state.registered.get(&fd) {
                Some(reg) if reg.stride == stride && reg.offset == offset => Some(reg.gl_tex),
                Some(_) => {
                    if let Some(old) = state.registered.remove(&fd) {
                        state.egl.destroy_dmabuf_texture(old.egl_image, old.gl_tex);
                    }
                    None
                }
                None => None,
            };

            let src_tex = if let Some(tex) = cached_tex {
                tex
            } else {
                match state.egl.import_dmabuf_texture(
                    fd,
                    state.width,
                    state.height,
                    state.drm_fourcc,
                    stride,
                    offset,
                    state.drm_modifier,
                ) {
                    Ok((egl_image, gl_tex)) => {
                        state.registered.insert(
                            fd,
                            RegisteredDmaBuf {
                                egl_image,
                                gl_tex,
                                stride,
                                offset,
                            },
                        );
                        gl_tex
                    }
                    Err(e) => {
                        error!("EGL import_dmabuf_texture failed: {:#}", e);
                        return;
                    }
                }
            };

            if let Err(e) =
                state
                    .egl
                    .blit_to_target(src_tex, target.dst_gl_tex, state.width, state.height)
            {
                error!("GL DMA-BUF blit failed: {:#}", e);
                return;
            }

            if let Err(e) = state.cuda.map_resource(target.cuda_res) {
                error!("CUDA map_resource failed: {:#}", e);
                return;
            }

            let desired_kbps = state.control.target_bitrate_kbps.load(Ordering::Relaxed);
            if desired_kbps != state.bitrate_kbps && desired_kbps >= 2_000 {
                match nvenc.reconfigure_bitrate(desired_kbps) {
                    Ok(()) => state.bitrate_kbps = desired_kbps,
                    Err(e) => warn!(
                        "NVENC bitrate reconfigure ({} kbps) failed for stream {}: {:#}",
                        desired_kbps, state.stream_id, e
                    ),
                }
            }

            let ts_us = state.start_time.elapsed().as_micros() as u64;
            let is_idr =
                state.frame_index == 0 || state.control.force_idr.swap(false, Ordering::AcqRel);
            let encode_res = nvenc.encode_registered_frame(target.nvenc_res, ts_us, is_idr);
            if let Err(e) = state.cuda.unmap_resource(target.cuda_res) {
                error!("CUDA unmap_resource failed: {:#}", e);
            }

            match encode_res {
                Ok(payload) if !payload.is_empty() => {
                    state.last_encoded_at = Some(now);
                    let frame = Arc::new(EncodedFrame {
                        stream_id: state.stream_id,
                        width: state.width,
                        height: state.height,
                        frame_index: state.frame_index,
                        capture_timestamp_us: ts_us,
                        is_idr,
                        payload: Bytes::from(payload),
                    });
                    state.frame_index += 1;
                    let _ = state.frame_tx.send(frame);
                }
                Ok(_) => {}
                Err(e) => {
                    error!("NVENC encode failed: {:#}", e);
                }
            }
        })
        .register()
        .context("Failed to register PipeWire stream listener")?;

    let mut format_buf = Vec::new();
    let format_bytes = build_video_format_params(&mut format_buf, &modifiers, max_fps.max(30))?;
    let format_pod = Pod::from_bytes(format_bytes).context("Invalid format POD")?;

    stream
        .connect(
            libspa::utils::Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT,
            &mut [format_pod],
        )
        .context("Failed to connect PipeWire stream")?;

    let mainloop_weak = std::rc::Rc::downgrade(&mainloop);
    let _quit_listener = quit_rx.attach(mainloop.loop_(), move |_| {
        if let Some(ml) = mainloop_weak.upgrade() {
            ml.quit();
        }
    });

    info!("PipeWire capture loop running for node_id={}", node_id);
    mainloop.run();
    Ok(())
}

fn build_video_format_params<'a>(
    buf: &'a mut Vec<u8>,
    modifiers: &[u64],
    max_fps: u32,
) -> Result<&'a [u8]> {
    let mod_alts: Vec<i64> = if modifiers.is_empty() {
        vec![0, DRM_FORMAT_MOD_INVALID as i64]
    } else {
        modifiers.iter().map(|&m| m as i64).collect()
    };
    let mod_default = mod_alts[0];

    let obj = Object {
        type_: SPA_TYPE_OBJECT_Format,
        id: SPA_PARAM_EnumFormat,
        properties: vec![
            Property::new(
                libspa::sys::SPA_FORMAT_mediaType,
                Value::Id(libspa::utils::Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                libspa::sys::SPA_FORMAT_mediaSubtype,
                Value::Id(libspa::utils::Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                SPA_FORMAT_VIDEO_format,
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: libspa::utils::Id(VideoFormat::BGRx.as_raw()),
                        alternatives: vec![
                            libspa::utils::Id(VideoFormat::BGRx.as_raw()),
                            libspa::utils::Id(VideoFormat::BGRA.as_raw()),
                            libspa::utils::Id(VideoFormat::RGBx.as_raw()),
                            libspa::utils::Id(VideoFormat::RGBA.as_raw()),
                        ],
                    },
                ))),
            ),
            Property {
                key: SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::MANDATORY
                    | PropertyFlags::from_bits_retain(libspa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE),
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: mod_default,
                        alternatives: mod_alts,
                    },
                ))),
            },
            Property::new(
                SPA_FORMAT_VIDEO_size,
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: 2560,
                            height: 1440,
                        },
                        min: Rectangle {
                            width: 320,
                            height: 240,
                        },
                        max: Rectangle {
                            width: 7680,
                            height: 4320,
                        },
                    },
                ))),
            ),
            Property::new(
                SPA_FORMAT_VIDEO_framerate,
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction {
                            num: max_fps,
                            denom: 1,
                        },
                        min: Fraction { num: 0, denom: 1 },
                        max: Fraction {
                            num: max_fps,
                            denom: 1,
                        },
                    },
                ))),
            ),
        ],
    };

    PodSerializer::serialize(std::io::Cursor::new(&mut *buf), &Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("PodSerializer failed: {:?}", e))?;
    Ok(buf.as_slice())
}

fn build_dmabuf_buffer_params(buf: &mut Vec<u8>) -> Result<&[u8]> {
    let obj = Object {
        type_: SPA_TYPE_OBJECT_ParamBuffers,
        id: SPA_PARAM_Buffers,
        properties: vec![Property::new(
            SPA_PARAM_BUFFERS_dataType,
            Value::Int(1 << SPA_DATA_DmaBuf),
        )],
    };
    PodSerializer::serialize(std::io::Cursor::new(&mut *buf), &Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("PodSerializer failed: {:?}", e))?;
    Ok(buf.as_slice())
}
