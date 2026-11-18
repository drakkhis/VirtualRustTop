//! Zero-copy DMA-BUF -> EGLImageKHR -> CUDA -> NVENC hardware encoder.
//!
//! Links dynamically at runtime to `libEGL.so.1`, `libcuda.so.1`, and `libnvidia-encode.so.1`
//! so no compile-time NVIDIA Video Codec SDK headers are required.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use anyhow::{anyhow, bail, Context, Result};
use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::os::fd::RawFd;
use std::ptr;
use tracing::{debug, info};
use vdesk_protocol::VideoCodec;

// ---------------------------------------------------------------------------
// EGL FFI & Constants
// ---------------------------------------------------------------------------

type EGLDisplay = *mut c_void;
type EGLContext = *mut c_void;
type EGLImageKHR = *mut c_void;
type EGLDeviceEXT = *mut c_void;
type EGLBoolean = c_uint;
type EGLint = i32;
type EGLenum = c_uint;

const EGL_NO_DISPLAY: EGLDisplay = ptr::null_mut();
const EGL_NO_CONTEXT: EGLContext = ptr::null_mut();
const EGL_NO_IMAGE_KHR: EGLImageKHR = ptr::null_mut();
const EGL_DEFAULT_DISPLAY: *mut c_void = ptr::null_mut();
const EGL_PLATFORM_DEVICE_EXT: EGLenum = 0x313F;

const EGL_LINUX_DMA_BUF_EXT: EGLenum = 0x3270;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_HEIGHT: EGLint = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: EGLint = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: EGLint = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: EGLint = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: EGLint = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: EGLint = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: EGLint = 0x3444;
const EGL_IMAGE_PRESERVED_KHR: EGLint = 0x30D2;
const EGL_NONE: EGLint = 0x3038;
const EGL_TRUE: EGLBoolean = 1;
const EGL_OPENGL_API: EGLenum = 0x30A2;
const EGL_NO_SURFACE: *mut c_void = ptr::null_mut();

const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_NEAREST: c_int = 0x2600;
const GL_RGBA8: c_int = 0x8058;
const GL_BGRA: c_uint = 0x80E1;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_READ_FRAMEBUFFER: c_uint = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: c_uint = 0x8CA9;
const GL_COLOR_ATTACHMENT0: c_uint = 0x8CE0;
const GL_COLOR_BUFFER_BIT: c_uint = 0x0000_4000;

pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
pub const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258; // 'XR24'
pub const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241; // 'AR24'
pub const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258; // 'XB24'
pub const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241; // 'AB24'

pub struct EglDmaBufImporter {
    _lib: Library,
    _gl_lib: Library,
    display: EGLDisplay,
    egl_context: EGLContext,
    egl_destroy_context: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
    egl_create_image_khr: unsafe extern "C" fn(
        EGLDisplay,
        EGLContext,
        EGLenum,
        *mut c_void,
        *const EGLint,
    ) -> EGLImageKHR,
    egl_destroy_image_khr: unsafe extern "C" fn(EGLDisplay, EGLImageKHR) -> EGLBoolean,
    egl_query_dmabuf_modifiers_ext: Option<
        unsafe extern "C" fn(
            EGLDisplay,
            EGLint,
            EGLint,
            *mut u64,
            *mut EGLBoolean,
            *mut EGLint,
        ) -> EGLBoolean,
    >,
    gl_gen_textures: unsafe extern "C" fn(c_int, *mut c_uint),
    gl_delete_textures: unsafe extern "C" fn(c_int, *const c_uint),
    gl_bind_texture: unsafe extern "C" fn(c_uint, c_uint),
    gl_tex_parameteri: unsafe extern "C" fn(c_uint, c_uint, c_int),
    gl_tex_image_2d: unsafe extern "C" fn(
        c_uint,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
        *const c_void,
    ),
    gl_finish: unsafe extern "C" fn(),
    gl_get_error: unsafe extern "C" fn() -> c_uint,
    gl_egl_image_target_texture_2d_oes: unsafe extern "C" fn(c_uint, EGLImageKHR),
    gl_gen_framebuffers: unsafe extern "C" fn(c_int, *mut c_uint),
    gl_delete_framebuffers: unsafe extern "C" fn(c_int, *const c_uint),
    gl_bind_framebuffer: unsafe extern "C" fn(c_uint, c_uint),
    gl_framebuffer_texture_2d: unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint, c_int),
    gl_blit_framebuffer: unsafe extern "C" fn(
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
    ),
    read_fbo: c_uint,
    draw_fbo: c_uint,
}

impl EglDmaBufImporter {
    pub fn new() -> Result<Self> {
        unsafe {
            let lib = Library::new("libEGL.so.1").context("Failed to load libEGL.so.1")?;
            let gl_lib = Library::new("libGL.so.1").context("Failed to load libGL.so.1")?;
            let egl_get_display: Symbol<unsafe extern "C" fn(*mut c_void) -> EGLDisplay> =
                lib.get(b"eglGetDisplay")?;
            let egl_initialize: Symbol<
                unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean,
            > = lib.get(b"eglInitialize")?;
            let egl_get_proc_address: Symbol<
                unsafe extern "C" fn(*const c_char) -> *mut c_void,
            > = lib.get(b"eglGetProcAddress")?;
            let egl_bind_api: Symbol<unsafe extern "C" fn(EGLenum) -> EGLBoolean> =
                lib.get(b"eglBindAPI")?;
            let egl_create_context: Symbol<
                unsafe extern "C" fn(
                    EGLDisplay,
                    *mut c_void,
                    EGLContext,
                    *const EGLint,
                ) -> EGLContext,
            > = lib.get(b"eglCreateContext")?;
            let egl_destroy_context: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean =
                *lib.get(b"eglDestroyContext")?;
            let egl_make_current: Symbol<
                unsafe extern "C" fn(EGLDisplay, *mut c_void, *mut c_void, EGLContext) -> EGLBoolean,
            > = lib.get(b"eglMakeCurrent")?;

            // Prefer EGL_PLATFORM_DEVICE_EXT so NVIDIA's EGLDevice is selected directly
            // without hitting Mesa DRI2 probe warnings on headless/Wayland processes.
            let mut display = EGL_NO_DISPLAY;
            let query_devices_ptr = egl_get_proc_address(c"eglQueryDevicesEXT".as_ptr());
            let get_platform_display_ptr =
                egl_get_proc_address(c"eglGetPlatformDisplayEXT".as_ptr());

            if !query_devices_ptr.is_null() && !get_platform_display_ptr.is_null() {
                let egl_query_devices: unsafe extern "C" fn(
                    EGLint,
                    *mut EGLDeviceEXT,
                    *mut EGLint,
                ) -> EGLBoolean = std::mem::transmute(query_devices_ptr);
                let egl_get_platform_display: unsafe extern "C" fn(
                    EGLenum,
                    *mut c_void,
                    *const EGLint,
                ) -> EGLDisplay = std::mem::transmute(get_platform_display_ptr);

                let mut devices: [EGLDeviceEXT; 8] = [ptr::null_mut(); 8];
                let mut num_devices: EGLint = 0;
                if egl_query_devices(8, devices.as_mut_ptr(), &mut num_devices) == EGL_TRUE {
                    for &dev in devices.iter().take(num_devices.max(0) as usize) {
                        let dpy =
                            egl_get_platform_display(EGL_PLATFORM_DEVICE_EXT, dev, ptr::null());
                        if dpy != EGL_NO_DISPLAY {
                            let mut maj = 0;
                            let mut min = 0;
                            if egl_initialize(dpy, &mut maj, &mut min) == EGL_TRUE {
                                display = dpy;
                                info!("Initialized NVIDIA/Platform EGL {}.{} via EGLDevice", maj, min);
                                break;
                            }
                        }
                    }
                }
            }

            if display == EGL_NO_DISPLAY {
                display = egl_get_display(EGL_DEFAULT_DISPLAY);
                if display == EGL_NO_DISPLAY {
                    bail!("eglGetDisplay(EGL_DEFAULT_DISPLAY) returned EGL_NO_DISPLAY");
                }
                let mut major = 0;
                let mut minor = 0;
                if egl_initialize(display, &mut major, &mut minor) != EGL_TRUE {
                    bail!("eglInitialize failed");
                }
                info!("Initialized EGL {}.{}", major, minor);
            }

            if egl_bind_api(EGL_OPENGL_API) != EGL_TRUE {
                bail!("eglBindAPI(EGL_OPENGL_API) failed");
            }
            let egl_context =
                egl_create_context(display, ptr::null_mut(), EGL_NO_CONTEXT, ptr::null());
            if egl_context == EGL_NO_CONTEXT {
                bail!("eglCreateContext failed");
            }
            if egl_make_current(display, EGL_NO_SURFACE, EGL_NO_SURFACE, egl_context) != EGL_TRUE {
                egl_destroy_context(display, egl_context);
                bail!("eglMakeCurrent failed");
            }

            let create_ptr = egl_get_proc_address(c"eglCreateImageKHR".as_ptr());
            let destroy_ptr = egl_get_proc_address(c"eglDestroyImageKHR".as_ptr());
            if create_ptr.is_null() || destroy_ptr.is_null() {
                egl_destroy_context(display, egl_context);
                bail!("eglCreateImageKHR / eglDestroyImageKHR unavailable");
            }

            let mods_ptr = egl_get_proc_address(c"eglQueryDmaBufModifiersEXT".as_ptr());
            let egl_query_dmabuf_modifiers_ext = if mods_ptr.is_null() {
                None
            } else {
                Some(std::mem::transmute(mods_ptr))
            };

            let target_tex_ptr = egl_get_proc_address(c"glEGLImageTargetTexture2DOES".as_ptr());
            let gen_fb_ptr = egl_get_proc_address(c"glGenFramebuffers".as_ptr());
            let del_fb_ptr = egl_get_proc_address(c"glDeleteFramebuffers".as_ptr());
            let bind_fb_ptr = egl_get_proc_address(c"glBindFramebuffer".as_ptr());
            let fb_tex_ptr = egl_get_proc_address(c"glFramebufferTexture2D".as_ptr());
            let blit_fb_ptr = egl_get_proc_address(c"glBlitFramebuffer".as_ptr());
            if target_tex_ptr.is_null()
                || gen_fb_ptr.is_null()
                || del_fb_ptr.is_null()
                || bind_fb_ptr.is_null()
                || fb_tex_ptr.is_null()
                || blit_fb_ptr.is_null()
            {
                egl_destroy_context(display, egl_context);
                bail!("Required GL/EGL extension functions unavailable");
            }

            let gl_gen_textures = *gl_lib.get(b"glGenTextures")?;
            let gl_delete_textures = *gl_lib.get(b"glDeleteTextures")?;
            let gl_bind_texture = *gl_lib.get(b"glBindTexture")?;
            let gl_tex_parameteri = *gl_lib.get(b"glTexParameteri")?;
            let gl_tex_image_2d = *gl_lib.get(b"glTexImage2D")?;
            let gl_finish = *gl_lib.get(b"glFinish")?;
            let gl_get_error = *gl_lib.get(b"glGetError")?;

            let gl_gen_framebuffers: unsafe extern "C" fn(c_int, *mut c_uint) =
                std::mem::transmute(gen_fb_ptr);
            let mut fbos = [0u32; 2];
            gl_gen_framebuffers(2, fbos.as_mut_ptr());

            Ok(Self {
                _lib: lib,
                _gl_lib: gl_lib,
                display,
                egl_context,
                egl_destroy_context,
                egl_create_image_khr: std::mem::transmute(create_ptr),
                egl_destroy_image_khr: std::mem::transmute(destroy_ptr),
                egl_query_dmabuf_modifiers_ext,
                gl_gen_textures,
                gl_delete_textures,
                gl_bind_texture,
                gl_tex_parameteri,
                gl_tex_image_2d,
                gl_finish,
                gl_get_error,
                gl_egl_image_target_texture_2d_oes: std::mem::transmute(target_tex_ptr),
                gl_gen_framebuffers,
                gl_delete_framebuffers: std::mem::transmute(del_fb_ptr),
                gl_bind_framebuffer: std::mem::transmute(bind_fb_ptr),
                gl_framebuffer_texture_2d: std::mem::transmute(fb_tex_ptr),
                gl_blit_framebuffer: std::mem::transmute(blit_fb_ptr),
                read_fbo: fbos[0],
                draw_fbo: fbos[1],
            })
        }
    }

    pub fn query_dmabuf_modifiers(&self, fourcc: u32) -> Vec<u64> {
        let Some(query_fn) = self.egl_query_dmabuf_modifiers_ext else {
            return Vec::new();
        };
        unsafe {
            let mut count: EGLint = 0;
            if query_fn(
                self.display,
                fourcc as EGLint,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut count,
            ) != EGL_TRUE
                || count <= 0
            {
                return Vec::new();
            }
            let mut mods = vec![0u64; count as usize];
            if query_fn(
                self.display,
                fourcc as EGLint,
                count,
                mods.as_mut_ptr(),
                ptr::null_mut(),
                &mut count,
            ) != EGL_TRUE
            {
                return Vec::new();
            }
            mods.truncate(count.max(0) as usize);
            // Retain only uncompressed single-plane modifiers (c == 0 in bits 25:23, or linear 0).
            mods.retain(|&m| m == 0 || ((m >> 23) & 0x7) == 0);
            mods
        }
    }

    pub fn create_cuda_target_texture(&self, width: u32, height: u32) -> Result<c_uint> {
        unsafe {
            let mut tex: c_uint = 0;
            (self.gl_gen_textures)(1, &mut tex);
            (self.gl_bind_texture)(GL_TEXTURE_2D, tex);
            (self.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            (self.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
            (self.gl_tex_image_2d)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA8,
                width as c_int,
                height as c_int,
                0,
                GL_BGRA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            (self.gl_bind_texture)(GL_TEXTURE_2D, 0);
            let err = (self.gl_get_error)();
            if err != 0 || tex == 0 {
                if tex != 0 {
                    (self.gl_delete_textures)(1, &tex);
                }
                bail!("create_cuda_target_texture failed with GL error 0x{:x}", err);
            }
            Ok(tex)
        }
    }

    pub fn import_dmabuf_texture(
        &self,
        fd: RawFd,
        width: u32,
        height: u32,
        fourcc: u32,
        stride: u32,
        offset: u32,
        modifier: u64,
    ) -> Result<(EGLImageKHR, c_uint)> {
        let egl_img = self.import_dmabuf(fd, width, height, fourcc, stride, offset, modifier)?;
        unsafe {
            let mut tex: c_uint = 0;
            (self.gl_gen_textures)(1, &mut tex);
            (self.gl_bind_texture)(GL_TEXTURE_2D, tex);
            (self.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            (self.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
            (self.gl_egl_image_target_texture_2d_oes)(GL_TEXTURE_2D, egl_img);
            (self.gl_bind_texture)(GL_TEXTURE_2D, 0);
            let err = (self.gl_get_error)();
            if err != 0 || tex == 0 {
                if tex != 0 {
                    (self.gl_delete_textures)(1, &tex);
                }
                self.destroy_image(egl_img);
                bail!(
                    "glEGLImageTargetTexture2DOES failed with GL error 0x{:x}",
                    err
                );
            }
            Ok((egl_img, tex))
        }
    }

    pub fn blit_to_target(
        &self,
        src_tex: c_uint,
        dst_tex: c_uint,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe {
            (self.gl_bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read_fbo);
            (self.gl_framebuffer_texture_2d)(
                GL_READ_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                src_tex,
                0,
            );
            (self.gl_bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.draw_fbo);
            (self.gl_framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                dst_tex,
                0,
            );
            (self.gl_blit_framebuffer)(
                0,
                0,
                width as c_int,
                height as c_int,
                0,
                0,
                width as c_int,
                height as c_int,
                GL_COLOR_BUFFER_BIT,
                GL_NEAREST as c_uint,
            );
            (self.gl_bind_framebuffer)(GL_READ_FRAMEBUFFER, 0);
            (self.gl_bind_framebuffer)(GL_DRAW_FRAMEBUFFER, 0);
            (self.gl_finish)();
            let err = (self.gl_get_error)();
            if err != 0 {
                bail!("glBlitFramebuffer failed with GL error 0x{:x}", err);
            }
            Ok(())
        }
    }

    pub fn destroy_dmabuf_texture(&self, img: EGLImageKHR, tex: c_uint) {
        self.destroy_texture(tex);
        self.destroy_image(img);
    }

    pub fn destroy_texture(&self, tex: c_uint) {
        if tex != 0 {
            unsafe {
                (self.gl_delete_textures)(1, &tex);
            }
        }
    }

    pub fn import_dmabuf(
        &self,
        fd: RawFd,
        width: u32,
        height: u32,
        fourcc: u32,
        stride: u32,
        offset: u32,
        modifier: u64,
    ) -> Result<EGLImageKHR> {
        let mut attribs = vec![
            EGL_WIDTH,
            width as EGLint,
            EGL_HEIGHT,
            height as EGLint,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as EGLint,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            offset as EGLint,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            stride as EGLint,
            EGL_IMAGE_PRESERVED_KHR,
            EGL_TRUE as EGLint,
        ];

        if modifier != DRM_FORMAT_MOD_INVALID {
            attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT);
            attribs.push((modifier & 0xFFFF_FFFF) as EGLint);
            attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT);
            attribs.push((modifier >> 32) as EGLint);
        }
        attribs.push(EGL_NONE);

        let img = unsafe {
            (self.egl_create_image_khr)(
                self.display,
                EGL_NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        if img == EGL_NO_IMAGE_KHR {
            bail!(
                "eglCreateImageKHR failed for fd={} {}x{} fourcc=0x{:08x} mod=0x{:016x}",
                fd,
                width,
                height,
                fourcc,
                modifier
            );
        }
        Ok(img)
    }

    pub fn destroy_image(&self, img: EGLImageKHR) {
        if !img.is_null() {
            unsafe {
                (self.egl_destroy_image_khr)(self.display, img);
            }
        }
    }
}

impl Drop for EglDmaBufImporter {
    fn drop(&mut self) {
        unsafe {
            let fbos = [self.read_fbo, self.draw_fbo];
            (self.gl_delete_framebuffers)(2, fbos.as_ptr());
            if !self.egl_context.is_null() {
                (self.egl_destroy_context)(self.display, self.egl_context);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CUDA Driver API & EGL Interop FFI
// ---------------------------------------------------------------------------

type CUresult = c_int;
type CUdevice = c_int;
pub type CUcontext = *mut c_void;
pub type CUarray = *mut c_void;
pub type CUgraphicsResource = *mut c_void;

const CUDA_SUCCESS: CUresult = 0;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: c_uint = 1;
const CU_EGL_FRAME_TYPE_ARRAY: c_int = 0;
const CU_EGL_FRAME_TYPE_PITCH: c_int = 1;
const MAX_PLANES: usize = 3;

#[repr(C)]
union CUeglFrameUnion {
    pArray: [CUarray; MAX_PLANES],
    pPitch: [*mut c_void; MAX_PLANES],
}

#[repr(C)]
pub struct CUeglFrame {
    frame: CUeglFrameUnion,
    pub width: c_uint,
    pub height: c_uint,
    pub depth: c_uint,
    pub pitch: c_uint,
    pub planeCount: c_uint,
    pub numChannels: c_uint,
    pub frameType: c_int,
    pub eglColorFormat: c_int,
    pub cuFormat: c_int,
}

pub enum CudaMappedFrame {
    Array(CUarray),
    Pitch { ptr: *mut c_void, pitch: u32 },
}

pub struct CudaEglInterop {
    _lib: Library,
    pub ctx: CUcontext,
    cu_graphics_egl_register_image: unsafe extern "C" fn(
        *mut CUgraphicsResource,
        EGLImageKHR,
        c_uint,
    ) -> CUresult,
    cu_graphics_gl_register_image: unsafe extern "C" fn(
        *mut CUgraphicsResource,
        c_uint,
        c_uint,
        c_uint,
    ) -> CUresult,
    cu_graphics_map_resources: unsafe extern "C" fn(
        c_uint,
        *mut CUgraphicsResource,
        *mut c_void,
    ) -> CUresult,
    cu_graphics_unmap_resources: unsafe extern "C" fn(
        c_uint,
        *mut CUgraphicsResource,
        *mut c_void,
    ) -> CUresult,
    cu_graphics_sub_resource_get_mapped_array: unsafe extern "C" fn(
        *mut CUarray,
        CUgraphicsResource,
        c_uint,
        c_uint,
    ) -> CUresult,
    cu_graphics_unregister_resource: unsafe extern "C" fn(CUgraphicsResource) -> CUresult,
    cu_graphics_resource_get_mapped_egl_frame: unsafe extern "C" fn(
        *mut CUeglFrame,
        CUgraphicsResource,
        c_uint,
        c_uint,
    ) -> CUresult,
    cu_ctx_push_current: unsafe extern "C" fn(CUcontext) -> CUresult,
}

impl CudaEglInterop {
    pub fn new() -> Result<Self> {
        unsafe {
            let lib = Library::new("libcuda.so.1").context("Failed to load libcuda.so.1")?;
            let cu_init: Symbol<unsafe extern "C" fn(c_uint) -> CUresult> = lib.get(b"cuInit")?;
            let cu_device_get: Symbol<unsafe extern "C" fn(*mut CUdevice, c_int) -> CUresult> =
                lib.get(b"cuDeviceGet")?;
            let cu_ctx_create: Symbol<
                unsafe extern "C" fn(*mut CUcontext, c_uint, CUdevice) -> CUresult,
            > = lib.get(b"cuCtxCreate_v2")?;
            let cu_ctx_push_current: unsafe extern "C" fn(CUcontext) -> CUresult =
                *lib.get(b"cuCtxPushCurrent_v2")?;

            let res = cu_init(0);
            if res != CUDA_SUCCESS {
                bail!("cuInit failed with error {}", res);
            }

            let mut dev: CUdevice = 0;
            let res = cu_device_get(&mut dev, 0);
            if res != CUDA_SUCCESS {
                bail!("cuDeviceGet(0) failed with error {}", res);
            }

            let mut ctx: CUcontext = ptr::null_mut();
            let res = cu_ctx_create(&mut ctx, 0, dev);
            if res != CUDA_SUCCESS {
                bail!("cuCtxCreate_v2 failed with error {}", res);
            }

            let cu_graphics_egl_register_image = *lib.get(b"cuGraphicsEGLRegisterImage")?;
            let cu_graphics_gl_register_image = *lib.get(b"cuGraphicsGLRegisterImage")?;
            let cu_graphics_map_resources = *lib.get(b"cuGraphicsMapResources")?;
            let cu_graphics_unmap_resources = *lib.get(b"cuGraphicsUnmapResources")?;
            let cu_graphics_sub_resource_get_mapped_array =
                *lib.get(b"cuGraphicsSubResourceGetMappedArray")?;
            let cu_graphics_unregister_resource = *lib.get(b"cuGraphicsUnregisterResource")?;
            let cu_graphics_resource_get_mapped_egl_frame =
                *lib.get(b"cuGraphicsResourceGetMappedEglFrame")?;

            info!("Initialized CUDA context {:?} for EGL/GL interop", ctx);

            Ok(Self {
                _lib: lib,
                ctx,
                cu_graphics_egl_register_image,
                cu_graphics_gl_register_image,
                cu_graphics_map_resources,
                cu_graphics_unmap_resources,
                cu_graphics_sub_resource_get_mapped_array,
                cu_graphics_unregister_resource,
                cu_graphics_resource_get_mapped_egl_frame,
                cu_ctx_push_current,
            })
        }
    }

    pub fn push_current(&self) -> Result<()> {
        let res = unsafe { (self.cu_ctx_push_current)(self.ctx) };
        if res != CUDA_SUCCESS {
            bail!("cuCtxPushCurrent_v2 failed: {}", res);
        }
        Ok(())
    }

    pub fn register_gl_texture(&self, gl_tex: c_uint) -> Result<(CUgraphicsResource, CUarray)> {
        self.push_current()?;
        let mut resource: CUgraphicsResource = ptr::null_mut();
        let res = unsafe {
            (self.cu_graphics_gl_register_image)(
                &mut resource,
                gl_tex,
                GL_TEXTURE_2D,
                CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY,
            )
        };
        if res != CUDA_SUCCESS {
            bail!("cuGraphicsGLRegisterImage failed: {}", res);
        }

        let arr = match self.map_resource(resource) {
            Ok(arr) => arr,
            Err(e) => {
                self.unregister_resource(resource);
                return Err(e);
            }
        };
        self.unmap_resource(resource)?;
        Ok((resource, arr))
    }

    pub fn map_resource(&self, mut resource: CUgraphicsResource) -> Result<CUarray> {
        self.push_current()?;
        let res = unsafe { (self.cu_graphics_map_resources)(1, &mut resource, ptr::null_mut()) };
        if res != CUDA_SUCCESS {
            bail!("cuGraphicsMapResources failed: {}", res);
        }
        let mut arr: CUarray = ptr::null_mut();
        let res = unsafe {
            (self.cu_graphics_sub_resource_get_mapped_array)(&mut arr, resource, 0, 0)
        };
        if res != CUDA_SUCCESS {
            unsafe {
                (self.cu_graphics_unmap_resources)(1, &mut resource, ptr::null_mut());
            }
            bail!("cuGraphicsSubResourceGetMappedArray failed: {}", res);
        }
        Ok(arr)
    }

    pub fn unmap_resource(&self, mut resource: CUgraphicsResource) -> Result<()> {
        if !resource.is_null() {
            self.push_current()?;
            let res =
                unsafe { (self.cu_graphics_unmap_resources)(1, &mut resource, ptr::null_mut()) };
            if res != CUDA_SUCCESS {
                bail!("cuGraphicsUnmapResources failed: {}", res);
            }
        }
        Ok(())
    }

    pub fn register_egl_image(
        &self,
        egl_image: EGLImageKHR,
    ) -> Result<(CUgraphicsResource, CudaMappedFrame)> {
        self.push_current()?;
        let mut resource: CUgraphicsResource = ptr::null_mut();
        let res = unsafe {
            (self.cu_graphics_egl_register_image)(
                &mut resource,
                egl_image,
                CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY,
            )
        };
        if res != CUDA_SUCCESS {
            bail!("cuGraphicsEGLRegisterImage failed: {}", res);
        }

        let mut egl_frame: CUeglFrame = unsafe { std::mem::zeroed() };
        let res = unsafe {
            (self.cu_graphics_resource_get_mapped_egl_frame)(&mut egl_frame, resource, 0, 0)
        };
        if res != CUDA_SUCCESS {
            unsafe {
                (self.cu_graphics_unregister_resource)(resource);
            }
            bail!("cuGraphicsResourceGetMappedEglFrame failed: {}", res);
        }

        let mapped = if egl_frame.frameType == CU_EGL_FRAME_TYPE_ARRAY {
            unsafe { CudaMappedFrame::Array(egl_frame.frame.pArray[0]) }
        } else if egl_frame.frameType == CU_EGL_FRAME_TYPE_PITCH {
            unsafe {
                CudaMappedFrame::Pitch {
                    ptr: egl_frame.frame.pPitch[0],
                    pitch: egl_frame.pitch,
                }
            }
        } else {
            unsafe {
                (self.cu_graphics_unregister_resource)(resource);
            }
            bail!("Unsupported CUeglFrameType: {}", egl_frame.frameType);
        };

        Ok((resource, mapped))
    }

    pub fn unregister_resource(&self, resource: CUgraphicsResource) {
        if !resource.is_null() {
            unsafe {
                let _ = (self.cu_ctx_push_current)(self.ctx);
                (self.cu_graphics_unregister_resource)(resource);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// NVENC v12.2 FFI
// ---------------------------------------------------------------------------

type NVENCSTATUS = c_int;
const NV_ENC_SUCCESS: NVENCSTATUS = 0;

// NVENCAPI_MAJOR_VERSION = 12, NVENCAPI_MINOR_VERSION = 2
const NVENCAPI_MAJOR_VERSION: u32 = 12;
const NVENCAPI_MINOR_VERSION: u32 = 2;
const NVENCAPI_VERSION: u32 = NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24);

const fn nvenc_struct_version(ver: u32) -> u32 {
    NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}

const fn nvenc_struct_version_ex(ver: u32) -> u32 {
    nvenc_struct_version(ver) | (1 << 31)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

const NV_ENC_CODEC_HEVC_GUID: GUID = GUID {
    Data1: 0x790cdc88,
    Data2: 0x4522,
    Data3: 0x4d7b,
    Data4: [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
};

const NV_ENC_CODEC_AV1_GUID: GUID = GUID {
    Data1: 0x0a352289,
    Data2: 0x0aa7,
    Data3: 0x4759,
    Data4: [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
};

// NV_ENC_PRESET_P1_GUID: {FC0A8D3E-45F8-4CF8-80C7-298871590EBF}
const NV_ENC_PRESET_P1_GUID: GUID = GUID {
    Data1: 0xfc0a8d3e,
    Data2: 0x45f8,
    Data3: 0x4cf8,
    Data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};

const NV_ENC_DEVICE_TYPE_CUDA: u32 = 1;
const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
const NV_ENC_PARAMS_RC_CBR: u32 = 2;
const NVENC_INFINITE_GOPLENGTH: u32 = 0xFFFF_FFFF;

const NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR: u32 = 1;
const NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY: u32 = 2;

pub const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x0100_0000;
pub const NV_ENC_BUFFER_FORMAT_ABGR: u32 = 0x1000_0000;

const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x01;
const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x02;
const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x04;

#[repr(C)]
struct NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
    version: u32,
    deviceType: u32,
    device: *mut c_void,
    reserved: *mut c_void,
    apiVersion: u32,
    reserved1: [u32; 253],
    reserved2: [*mut c_void; 64],
}

#[repr(C)]
struct NV_ENC_RC_PARAMS {
    version: u32,
    rateControlMode: u32,
    constQP: [u32; 3],
    averageBitRate: u32,
    maxBitRate: u32,
    vbvBufferSize: u32,
    vbvInitialDelay: u32,
    bitfields: u32,
    minQP: [u32; 3],
    maxQP: [u32; 3],
    initialRCQP: [u32; 3],
    temporallayerIdxMask: u32,
    temporalLayerQP: [u8; 8],
    targetQuality: u8,
    targetQualityLSB: u8,
    lookaheadDepth: u16,
    lowDelayKeyFrameScale: u8,
    yDcQPIndexOffset: i8,
    uDcQPIndexOffset: i8,
    vDcQPIndexOffset: i8,
    qpMapMode: u32,
    multiPass: u32,
    alphaLayerBitrateRatio: u32,
    cbQPIndexOffset: i8,
    crQPIndexOffset: i8,
    reserved2: u16,
    reserved: [u32; 4],
}

#[repr(C)]
struct NV_ENC_CONFIG {
    version: u32,
    profileGUID: GUID,
    gopLength: u32,
    frameIntervalP: i32,
    monoChromeEncoding: u32,
    frameFieldMode: u32,
    mvPrecision: u32,
    rcParams: NV_ENC_RC_PARAMS,
    // Codec-specific union + reserved padding (320 * 4 bytes is generous for HEVC/AV1)
    encodeCodecConfig: [u32; 320],
    reserved: [u32; 278],
    reserved2: [*mut c_void; 64],
}

#[repr(C)]
struct NV_ENC_PRESET_CONFIG {
    version: u32,
    reserved: u32,
    presetCfg: NV_ENC_CONFIG,
    reserved1: [u32; 255],
    reserved2: [*mut c_void; 64],
}

#[repr(C)]
struct NV_ENC_INITIALIZE_PARAMS {
    version: u32,
    encodeGUID: GUID,
    presetGUID: GUID,
    encodeWidth: u32,
    encodeHeight: u32,
    darWidth: u32,
    darHeight: u32,
    frameRateNum: u32,
    frameRateDen: u32,
    enableEncodeAsync: u32,
    enablePTD: u32,
    bitfields: u32,
    privDataSize: u32,
    reserved: u32,
    privData: *mut c_void,
    encodeConfig: *mut NV_ENC_CONFIG,
    maxEncodeWidth: u32,
    maxEncodeHeight: u32,
    maxMEHintCountsPerBlock: [u32; 2],
    tuningInfo: u32,
    bufferFormat: u32,
    numStateBuffers: u32,
    outputStatsLevel: u32,
    reserved1: [u32; 284],
    reserved2: [*mut c_void; 64],
}

#[repr(C)]
struct NV_ENC_CREATE_BITSTREAM_BUFFER {
    version: u32,
    size: u32,
    memoryHeap: u32,
    reserved: u32,
    bitstreamBuffer: *mut c_void,
    bitstreamBufferPtr: *mut c_void,
    reserved1: [u32; 58],
    reserved2: [*mut c_void; 64],
}

#[repr(C)]
struct NV_ENC_REGISTER_RESOURCE {
    version: u32,
    resourceType: u32,
    width: u32,
    height: u32,
    pitch: u32,
    subResourceIndex: u32,
    resourceToRegister: *mut c_void,
    registeredResource: *mut c_void,
    bufferFormat: u32,
    bufferUsage: u32,
    pInputFencePoint: *mut c_void,
    chromaOffset: [u32; 2],
    chromaOffsetIn: [u32; 2],
    reserved1: [u32; 244],
    reserved2: [*mut c_void; 62],
}

#[repr(C)]
struct NV_ENC_MAP_INPUT_RESOURCE {
    version: u32,
    subResourceIndex: u32,
    inputResource: *mut c_void,
    registeredResource: *mut c_void,
    mappedResource: *mut c_void,
    mappedBufferFmt: u32,
    reserved1: [u32; 251],
    reserved2: [*mut c_void; 63],
}

#[repr(C)]
struct NV_ENC_PIC_PARAMS {
    version: u32,
    inputWidth: u32,
    inputHeight: u32,
    inputPitch: u32,
    encodePicFlags: u32,
    frameIdx: u32,
    inputTimeStamp: u64,
    inputDuration: u64,
    inputBuffer: *mut c_void,
    outputBitstream: *mut c_void,
    completionEvent: *mut c_void,
    bufferFmt: u32,
    pictureStruct: u32,
    pictureType: u32,
    codecPicParams: [u32; 256],
    meHintCountsPerBlock: [u32; 2],
    meExternalHints: *mut c_void,
    reserved1: [u32; 6],
    reserved2: [*mut c_void; 2],
    qpDeltaMap: *mut i8,
    qpDeltaMapSize: u32,
    reservedBitFields: u32,
    meHintRefPicDist: [u16; 2],
    reserved4: u32,
    alphaBuffer: *mut c_void,
    meExternalSbHints: *mut c_void,
    stateBufferIdx: u32,
    outputReconBuffer: *mut c_void,
    reserved3: [u32; 284],
    reserved5: [*mut c_void; 57],
}

#[repr(C)]
struct NV_ENC_LOCK_BITSTREAM {
    version: u32,
    bitfields: u32,
    outputBitstream: *mut c_void,
    sliceOffsets: *mut u32,
    frameIdx: u32,
    hwEncodeStatus: u32,
    numSlices: u32,
    bitstreamSizeInBytes: u32,
    outputTimeStamp: u64,
    outputDuration: u64,
    bitstreamBufferPtr: *mut c_void,
    pictureType: u32,
    pictureStruct: u32,
    frameAvgQP: u32,
    frameSatd: u32,
    ltrFrameIdx: u32,
    ltrFrameBitmap: u32,
    temporalId: u32,
    intraMBCount: u32,
    interMBCount: u32,
    averageMVX: i32,
    averageMVY: i32,
    outputStatsPtrSize: u32,
    reserved: [u32; 218],
    outputStatsPtr: *mut c_void,
    reserved2: [u32; 219],
    reserved1: [*mut c_void; 63],
}

#[repr(C)]
struct NV_ENCODE_API_FUNCTION_LIST {
    version: u32,
    reserved: u32,
    nvEncOpenEncodeSession: Option<unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void) -> NVENCSTATUS>,
    nvEncGetEncodeGUIDCount: Option<unsafe extern "C" fn(*mut c_void, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodeProfileGUIDCount: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodeProfileGUIDs: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut GUID, u32, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodeGUIDs: Option<unsafe extern "C" fn(*mut c_void, *mut GUID, u32, *mut u32) -> NVENCSTATUS>,
    nvEncGetInputFormatCount: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS>,
    nvEncGetInputFormats: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut u32, u32, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodeCaps: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut c_void, *mut c_int) -> NVENCSTATUS>,
    nvEncGetEncodePresetCount: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodePresetGUIDs: Option<unsafe extern "C" fn(*mut c_void, GUID, *mut GUID, u32, *mut u32) -> NVENCSTATUS>,
    nvEncGetEncodePresetConfig: Option<unsafe extern "C" fn(*mut c_void, GUID, GUID, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS>,
    nvEncInitializeEncoder: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NVENCSTATUS>,
    nvEncCreateInputBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncDestroyInputBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncCreateBitstreamBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NVENCSTATUS>,
    nvEncDestroyBitstreamBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncEncodePicture: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS>,
    nvEncLockBitstream: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS>,
    nvEncUnlockBitstream: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncLockInputBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncUnlockInputBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncGetEncodeStats: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncGetSequenceParams: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncRegisterAsyncEvent: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncUnregisterAsyncEvent: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncMapInputResource: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_MAP_INPUT_RESOURCE) -> NVENCSTATUS>,
    nvEncUnmapInputResource: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncDestroyEncoder: Option<unsafe extern "C" fn(*mut c_void) -> NVENCSTATUS>,
    nvEncInvalidateRefFrames: Option<unsafe extern "C" fn(*mut c_void, u64) -> NVENCSTATUS>,
    nvEncOpenEncodeSessionEx: Option<unsafe extern "C" fn(*mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, *mut *mut c_void) -> NVENCSTATUS>,
    nvEncRegisterResource: Option<unsafe extern "C" fn(*mut c_void, *mut NV_ENC_REGISTER_RESOURCE) -> NVENCSTATUS>,
    nvEncUnregisterResource: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncReconfigureEncoder: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    reserved1: *mut c_void,
    nvEncCreateMVBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncDestroyMVBuffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncRunMotionEstimationOnly: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncGetLastErrorString: Option<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    nvEncSetIOCudaStreams: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> NVENCSTATUS>,
    nvEncGetEncodePresetConfigEx: Option<unsafe extern "C" fn(*mut c_void, GUID, GUID, u32, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS>,
    nvEncGetSequenceParamEx: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> NVENCSTATUS>,
    reserved2: [*mut c_void; 279],
}

pub struct NvEncSession {
    _lib: Library,
    fn_list: Box<NV_ENCODE_API_FUNCTION_LIST>,
    encoder: *mut c_void,
    bitstream_buffer: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub buffer_format: u32,
    frame_idx: u32,
    seq_header: Vec<u8>,
}

/// Extract the `OBU_SEQUENCE_HEADER` OBU (type 1) from an AV1 Annex-B/low-overhead OBU stream.
pub fn extract_av1_seq_header(bitstream: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    while pos < bitstream.len() {
        let start = pos;
        let hdr = bitstream[pos];
        let obu_type = (hdr >> 3) & 0x0F;
        let has_ext = (hdr >> 2) & 1 != 0;
        let has_size = (hdr >> 1) & 1 != 0;
        pos += 1 + usize::from(has_ext);
        if !has_size || pos >= bitstream.len() {
            return None;
        }
        let mut size: usize = 0;
        let mut shift = 0u32;
        loop {
            if pos >= bitstream.len() || shift >= 28 {
                return None;
            }
            let b = bitstream[pos];
            pos += 1;
            size |= ((b & 0x7F) as usize) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let end = pos.checked_add(size)?;
        if end > bitstream.len() {
            return None;
        }
        if obu_type == 1 {
            return Some(bitstream[start..end].to_vec());
        }
        pos = end;
    }
    None
}

impl NvEncSession {
    pub fn new(
        cuda_ctx: CUcontext,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        codec: VideoCodec,
        buffer_format: u32,
    ) -> Result<Self> {
        unsafe {
            let lib = Library::new("libnvidia-encode.so.1")
                .context("Failed to load libnvidia-encode.so.1")?;

            let get_max_ver: Symbol<unsafe extern "C" fn(*mut u32) -> NVENCSTATUS> =
                lib.get(b"NvEncodeAPIGetMaxSupportedVersion")?;
            let create_instance: Symbol<
                unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS,
            > = lib.get(b"NvEncodeAPICreateInstance")?;

            let mut max_ver: u32 = 0;
            let st = get_max_ver(&mut max_ver);
            if st != NV_ENC_SUCCESS {
                bail!("NvEncodeAPIGetMaxSupportedVersion failed: {}", st);
            }
            info!(
                "NVENC max supported API version: {}.{} (using {}.{})",
                max_ver & 0xF,
                (max_ver >> 4) & 0xF,
                NVENCAPI_MAJOR_VERSION,
                NVENCAPI_MINOR_VERSION
            );

            let mut fn_list: Box<NV_ENCODE_API_FUNCTION_LIST> = Box::new(std::mem::zeroed());
            fn_list.version = nvenc_struct_version(2);
            let st = create_instance(&mut *fn_list);
            if st != NV_ENC_SUCCESS {
                bail!("NvEncodeAPICreateInstance failed: {}", st);
            }

            let mut session_params: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
            session_params.version = nvenc_struct_version(1);
            session_params.deviceType = NV_ENC_DEVICE_TYPE_CUDA;
            session_params.device = cuda_ctx;
            session_params.apiVersion = NVENCAPI_VERSION;

            let mut encoder: *mut c_void = ptr::null_mut();
            let open_fn = fn_list
                .nvEncOpenEncodeSessionEx
                .ok_or_else(|| anyhow!("Missing nvEncOpenEncodeSessionEx"))?;
            let st = open_fn(&mut session_params, &mut encoder);
            if st != NV_ENC_SUCCESS {
                bail!("nvEncOpenEncodeSessionEx failed: {}", st);
            }

            let encode_guid = match codec {
                VideoCodec::Av1YUV420 => NV_ENC_CODEC_AV1_GUID,
                VideoCodec::HevcYUV420 | VideoCodec::HevcYUV444 => NV_ENC_CODEC_HEVC_GUID,
            };

            let mut preset_config: NV_ENC_PRESET_CONFIG = std::mem::zeroed();
            preset_config.version = nvenc_struct_version_ex(4);
            preset_config.presetCfg.version = nvenc_struct_version_ex(8);

            let preset_fn = fn_list
                .nvEncGetEncodePresetConfigEx
                .ok_or_else(|| anyhow!("Missing nvEncGetEncodePresetConfigEx"))?;
            let st = preset_fn(
                encoder,
                encode_guid,
                NV_ENC_PRESET_P1_GUID,
                NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                &mut preset_config,
            );
            if st != NV_ENC_SUCCESS {
                Self::destroy_raw(&fn_list, encoder, ptr::null_mut());
                bail!(
                    "nvEncGetEncodePresetConfigEx failed: {} ({})",
                    st,
                    Self::last_error(&fn_list, encoder)
                );
            }

            // Configure Ultra-Low-Latency desktop streaming parameters
            let cfg = &mut preset_config.presetCfg;
            cfg.gopLength = NVENC_INFINITE_GOPLENGTH;
            cfg.frameIntervalP = 1; // Zero B-frames
            cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
            cfg.rcParams.averageBitRate = bitrate_kbps * 1000;
            cfg.rcParams.maxBitRate = bitrate_kbps * 1000;
            // 200 ms VBV window so IDR keyframes are not starved/macroblock-scattered under load
            cfg.rcParams.vbvBufferSize = (bitrate_kbps * 1000) / 5;
            cfg.rcParams.vbvInitialDelay = cfg.rcParams.vbvBufferSize / 2;

            let mut init_params: NV_ENC_INITIALIZE_PARAMS = std::mem::zeroed();
            init_params.version = nvenc_struct_version_ex(6);
            init_params.encodeGUID = encode_guid;
            init_params.presetGUID = NV_ENC_PRESET_P1_GUID;
            init_params.encodeWidth = width;
            init_params.encodeHeight = height;
            init_params.darWidth = width;
            init_params.darHeight = height;
            init_params.frameRateNum = fps;
            init_params.frameRateDen = 1;
            init_params.enableEncodeAsync = 0;
            init_params.enablePTD = 1;
            init_params.encodeConfig = cfg;
            init_params.maxEncodeWidth = width;
            init_params.maxEncodeHeight = height;
            init_params.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
            init_params.bufferFormat = buffer_format;

            let init_fn = fn_list
                .nvEncInitializeEncoder
                .ok_or_else(|| anyhow!("Missing nvEncInitializeEncoder"))?;
            let st = init_fn(encoder, &mut init_params);
            if st != NV_ENC_SUCCESS {
                let err = Self::last_error(&fn_list, encoder);
                Self::destroy_raw(&fn_list, encoder, ptr::null_mut());
                bail!("nvEncInitializeEncoder failed: {} ({})", st, err);
            }

            let mut bs_params: NV_ENC_CREATE_BITSTREAM_BUFFER = std::mem::zeroed();
            bs_params.version = nvenc_struct_version(1);
            let create_bs_fn = fn_list
                .nvEncCreateBitstreamBuffer
                .ok_or_else(|| anyhow!("Missing nvEncCreateBitstreamBuffer"))?;
            let st = create_bs_fn(encoder, &mut bs_params);
            if st != NV_ENC_SUCCESS {
                let err = Self::last_error(&fn_list, encoder);
                Self::destroy_raw(&fn_list, encoder, ptr::null_mut());
                bail!("nvEncCreateBitstreamBuffer failed: {} ({})", st, err);
            }

            info!(
                "Initialized NVENC {:?} {}x{} @ {}fps ({} kbps)",
                codec, width, height, fps, bitrate_kbps
            );

            Ok(Self {
                _lib: lib,
                fn_list,
                encoder,
                bitstream_buffer: bs_params.bitstreamBuffer,
                width,
                height,
                buffer_format,
                frame_idx: 0,
                seq_header: Vec::new(),
            })
        }
    }

    pub fn register_cuda_frame(&self, mapped: &CudaMappedFrame) -> Result<*mut c_void> {
        unsafe {
            let mut reg: NV_ENC_REGISTER_RESOURCE = std::mem::zeroed();
            reg.version = nvenc_struct_version(4);
            reg.width = self.width;
            reg.height = self.height;
            reg.bufferFormat = self.buffer_format;
            reg.bufferUsage = 0; // NV_ENC_INPUT_IMAGE

            match mapped {
                CudaMappedFrame::Array(arr) => {
                    reg.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY;
                    reg.resourceToRegister = *arr;
                }
                CudaMappedFrame::Pitch { ptr, pitch } => {
                    reg.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
                    reg.resourceToRegister = *ptr;
                    reg.pitch = *pitch;
                }
            }

            let reg_fn = self
                .fn_list
                .nvEncRegisterResource
                .ok_or_else(|| anyhow!("Missing nvEncRegisterResource"))?;
            let st = reg_fn(self.encoder, &mut reg);
            if st != NV_ENC_SUCCESS {
                bail!(
                    "nvEncRegisterResource failed: {} ({})",
                    st,
                    Self::last_error(&self.fn_list, self.encoder)
                );
            }
            debug!("Registered CUDA frame with NVENC: {:?}", reg.registeredResource);
            Ok(reg.registeredResource)
        }
    }

    pub fn unregister_resource(&self, registered_resource: *mut c_void) {
        if !registered_resource.is_null() {
            if let Some(unreg_fn) = self.fn_list.nvEncUnregisterResource {
                unsafe {
                    unreg_fn(self.encoder, registered_resource);
                }
            }
        }
    }

    pub fn encode_registered_frame(
        &mut self,
        registered_resource: *mut c_void,
        timestamp_us: u64,
        force_idr: bool,
    ) -> Result<Vec<u8>> {
        unsafe {
            let mut map: NV_ENC_MAP_INPUT_RESOURCE = std::mem::zeroed();
            map.version = nvenc_struct_version(4);
            map.registeredResource = registered_resource;

            let map_fn = self
                .fn_list
                .nvEncMapInputResource
                .ok_or_else(|| anyhow!("Missing nvEncMapInputResource"))?;
            let st = map_fn(self.encoder, &mut map);
            if st != NV_ENC_SUCCESS {
                bail!(
                    "nvEncMapInputResource failed: {} ({})",
                    st,
                    Self::last_error(&self.fn_list, self.encoder)
                );
            }

            let mut pic: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            pic.version = nvenc_struct_version_ex(6);
            pic.inputWidth = self.width;
            pic.inputHeight = self.height;
            pic.inputBuffer = map.mappedResource;
            pic.outputBitstream = self.bitstream_buffer;
            pic.bufferFmt = map.mappedBufferFmt;
            pic.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
            pic.frameIdx = self.frame_idx;
            pic.inputTimeStamp = timestamp_us;
            let is_first = self.frame_idx == 0;
            if force_idr || is_first {
                pic.encodePicFlags = NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
            }
            self.frame_idx = self.frame_idx.wrapping_add(1);

            let encode_fn = self
                .fn_list
                .nvEncEncodePicture
                .ok_or_else(|| anyhow!("Missing nvEncEncodePicture"))?;
            let st = encode_fn(self.encoder, &mut pic);

            let unmap_fn = self.fn_list.nvEncUnmapInputResource;
            if st != NV_ENC_SUCCESS {
                if let Some(f) = unmap_fn {
                    f(self.encoder, map.mappedResource);
                }
                bail!(
                    "nvEncEncodePicture failed: {} ({})",
                    st,
                    Self::last_error(&self.fn_list, self.encoder)
                );
            }

            let mut lock: NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
            lock.version = nvenc_struct_version(2);
            lock.outputBitstream = self.bitstream_buffer;

            let lock_fn = self
                .fn_list
                .nvEncLockBitstream
                .ok_or_else(|| anyhow!("Missing nvEncLockBitstream"))?;
            let st = lock_fn(self.encoder, &mut lock);
            if st != NV_ENC_SUCCESS {
                if let Some(f) = unmap_fn {
                    f(self.encoder, map.mappedResource);
                }
                bail!("nvEncLockBitstream failed: {}", st);
            }

            let mut bytes = std::slice::from_raw_parts(
                lock.bitstreamBufferPtr as *const u8,
                lock.bitstreamSizeInBytes as usize,
            )
            .to_vec();

            if let Some(unlock_fn) = self.fn_list.nvEncUnlockBitstream {
                unlock_fn(self.encoder, self.bitstream_buffer);
            }
            if let Some(f) = unmap_fn {
                f(self.encoder, map.mappedResource);
            }

            if self.seq_header.is_empty() {
                if let Some(seq) = extract_av1_seq_header(&bytes) {
                    self.seq_header = seq;
                }
            } else if (force_idr || is_first) && extract_av1_seq_header(&bytes).is_none() {
                // NVENC AV1 omits OBU_SEQUENCE_HEADER on subsequent forced IDRs; splice it
                // right after the 2-byte OBU_TEMPORAL_DELIMITER (0x12, 0x00) if present.
                let insert_at = if bytes.starts_with(&[0x12, 0x00]) { 2 } else { 0 };
                let mut patched = Vec::with_capacity(bytes.len() + self.seq_header.len());
                patched.extend_from_slice(&bytes[..insert_at]);
                patched.extend_from_slice(&self.seq_header);
                patched.extend_from_slice(&bytes[insert_at..]);
                bytes = patched;
            }

            Ok(bytes)
        }
    }

    unsafe fn last_error(fn_list: &NV_ENCODE_API_FUNCTION_LIST, encoder: *mut c_void) -> String {
        if let Some(f) = fn_list.nvEncGetLastErrorString {
            let ptr = f(encoder);
            if !ptr.is_null() {
                return CStr::from_ptr(ptr).to_string_lossy().into_owned();
            }
        }
        String::from("unknown")
    }

    unsafe fn destroy_raw(
        fn_list: &NV_ENCODE_API_FUNCTION_LIST,
        encoder: *mut c_void,
        bitstream_buffer: *mut c_void,
    ) {
        if !bitstream_buffer.is_null() {
            if let Some(f) = fn_list.nvEncDestroyBitstreamBuffer {
                f(encoder, bitstream_buffer);
            }
        }
        if !encoder.is_null() {
            if let Some(f) = fn_list.nvEncDestroyEncoder {
                f(encoder);
            }
        }
    }
}

impl Drop for NvEncSession {
    fn drop(&mut self) {
        unsafe {
            Self::destroy_raw(&self.fn_list, self.encoder, self.bitstream_buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    #[test]
    fn egl_device_and_modifiers_work() {
        let egl = EglDmaBufImporter::new().expect("EGL init");
        let mods = egl.query_dmabuf_modifiers(DRM_FORMAT_XRGB8888);
        println!("EGL XRGB8888 modifiers ({}): {:x?}", mods.len(), mods);
        assert!(!mods.is_empty(), "Expected non-empty EGL DMA-BUF modifiers");
        assert!(
            mods.iter().all(|&m| m == 0 || ((m >> 23) & 0x7) == 0),
            "Expected only uncompressed modifiers"
        );
    }

    #[test]
    fn nvenc_av1_session_initializes() {
        let cuda = CudaEglInterop::new().expect("CUDA init");
        let _enc = NvEncSession::new(
            cuda.ctx,
            1920,
            1080,
            90,
            40_000,
            VideoCodec::Av1YUV420,
            NV_ENC_BUFFER_FORMAT_ABGR,
        )
        .expect("NVENC AV1 init");
    }

    #[test]
    fn dmabuf_to_nvenc_av1_encodes_frame() {
        let egl = EglDmaBufImporter::new().expect("EGL init");
        let cuda = CudaEglInterop::new().expect("CUDA init");

        let width = 1280u32;
        let height = 720u32;
        let mut nvenc = NvEncSession::new(
            cuda.ctx,
            width,
            height,
            90,
            40_000,
            VideoCodec::Av1YUV420,
            NV_ENC_BUFFER_FORMAT_ABGR,
        )
        .expect("NVENC AV1 init");

        // Allocate a real GBM DMA-BUF on /dev/dri/renderD128
        unsafe {
            let drm_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/dri/renderD128")
                .expect("open /dev/dri/renderD128");
            let gbm_lib = Library::new("libgbm.so.1").expect("load libgbm.so.1");
            let gbm_create_device: Symbol<unsafe extern "C" fn(c_int) -> *mut c_void> =
                gbm_lib.get(b"gbm_create_device").unwrap();
            let gbm_device_destroy: Symbol<unsafe extern "C" fn(*mut c_void)> =
                gbm_lib.get(b"gbm_device_destroy").unwrap();
            let gbm_bo_create_with_modifiers: Symbol<
                unsafe extern "C" fn(
                    *mut c_void,
                    c_uint,
                    c_uint,
                    c_uint,
                    *const u64,
                    c_uint,
                ) -> *mut c_void,
            > = gbm_lib.get(b"gbm_bo_create_with_modifiers").unwrap();
            let gbm_bo_destroy: Symbol<unsafe extern "C" fn(*mut c_void)> =
                gbm_lib.get(b"gbm_bo_destroy").unwrap();
            let gbm_bo_get_fd: Symbol<unsafe extern "C" fn(*mut c_void) -> c_int> =
                gbm_lib.get(b"gbm_bo_get_fd").unwrap();
            let gbm_bo_get_stride: Symbol<unsafe extern "C" fn(*mut c_void) -> c_uint> =
                gbm_lib.get(b"gbm_bo_get_stride").unwrap();
            let gbm_bo_get_modifier: Symbol<unsafe extern "C" fn(*mut c_void) -> u64> =
                gbm_lib.get(b"gbm_bo_get_modifier").unwrap();

            let gbm = gbm_create_device(drm_file.as_raw_fd());
            assert!(!gbm.is_null(), "gbm_create_device failed");

            let mod_val = 0x0300_0000_0060_6014u64;
            let bo = gbm_bo_create_with_modifiers(
                gbm,
                width,
                height,
                DRM_FORMAT_XRGB8888,
                &mod_val,
                1,
            );
            assert!(!bo.is_null(), "gbm_bo_create_with_modifiers failed");

            let dmabuf_fd = gbm_bo_get_fd(bo);
            let stride = gbm_bo_get_stride(bo);
            let actual_mod = gbm_bo_get_modifier(bo);
            assert!(dmabuf_fd >= 0);

            let (egl_img, src_tex) = egl
                .import_dmabuf_texture(
                    dmabuf_fd,
                    width,
                    height,
                    DRM_FORMAT_XRGB8888,
                    stride,
                    0,
                    actual_mod,
                )
                .expect("import_dmabuf_texture");

            let dst_tex = egl
                .create_cuda_target_texture(width, height)
                .expect("create_cuda_target_texture");
            let (cuda_res, cu_arr) = cuda
                .register_gl_texture(dst_tex)
                .expect("register_gl_texture");
            let nvenc_res = nvenc
                .register_cuda_frame(&CudaMappedFrame::Array(cu_arr))
                .expect("register_cuda_frame");

            // Frame 0 (IDR), Frame 1 (P), Frame 2 (forced mid-stream IDR)
            for idx in 0..3u64 {
                egl.blit_to_target(src_tex, dst_tex, width, height)
                    .expect("blit_to_target");
                let mapped_arr = cuda.map_resource(cuda_res).expect("map_resource");
                assert_eq!(mapped_arr, cu_arr, "CUarray changed across map");
                let force_idr = idx == 0 || idx == 2;
                let bitstream = nvenc
                    .encode_registered_frame(nvenc_res, idx * 11_111, force_idr)
                    .expect("encode_registered_frame");
                cuda.unmap_resource(cuda_res).expect("unmap_resource");
                println!("Frame {} encoded {} bytes of AV1", idx, bitstream.len());
                assert!(!bitstream.is_empty(), "Expected non-empty AV1 bitstream");
                if force_idr {
                    assert!(
                        extract_av1_seq_header(&bitstream).is_some(),
                        "Forced IDR frame {} missing OBU_SEQUENCE_HEADER",
                        idx
                    );
                }
            }

            nvenc.unregister_resource(nvenc_res);
            cuda.unregister_resource(cuda_res);
            egl.destroy_texture(dst_tex);
            egl.destroy_dmabuf_texture(egl_img, src_tex);
            libc::close(dmabuf_fd);
            gbm_bo_destroy(bo);
            gbm_device_destroy(gbm);
        }
    }
}
