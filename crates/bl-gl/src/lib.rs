//! Thin binding layer over the common subset of OpenGL ES 3.0, WebGL 2 and
//! desktop OpenGL 3.3 core.
//!
//! Function pointers are loaded through a `get_proc_address` closure
//! supplied by the UI toolkit (Qt's `QOpenGLContext::getProcAddress`, which
//! also covers WebGL 2 under Emscripten).
//!
//! Shaders are written in GLSL ES 3.00; [`Gl::shader_header`] returns the
//! correct `#version` line for the current context.

#![allow(clippy::too_many_arguments)]

pub mod consts;
pub use consts::*;

use std::ffi::c_void;

pub type GLenum = u32;
pub type GLuint = u32;
pub type GLint = i32;
pub type GLsizei = i32;
pub type GLfloat = f32;
pub type GLboolean = u8;
pub type GLbitfield = u32;
pub type GLsizeiptr = isize;
pub type GLintptr = isize;

macro_rules! gl_functions {
    ($( $field:ident = $sym:literal : fn( $($arg:ident : $ty:ty),* ) $(-> $ret:ty)? ; )*) => {
        /// Raw GL entry points.
        #[allow(non_snake_case)]
        pub struct Fns {
            $( pub $field: unsafe extern "system" fn($($ty),*) $(-> $ret)?, )*
        }

        impl Fns {
            /// Loads all entry points. `get` must return the address of the
            /// named function (null if missing).
            ///
            /// # Safety
            /// The returned addresses must be valid GL functions with the C
            /// signatures of the respective GL specification.
            pub unsafe fn load(get: &mut dyn FnMut(&str) -> *const c_void) -> Result<Fns, String> {
                Ok(Fns {
                    $( $field: {
                        let p = get($sym);
                        if p.is_null() {
                            return Err(format!("missing OpenGL function {}", $sym));
                        }
                        std::mem::transmute::<*const c_void, unsafe extern "system" fn($($ty),*) $(-> $ret)?>(p)
                    }, )*
                })
            }
        }
    };
}

gl_functions! {
    get_error = "glGetError": fn() -> GLenum;
    get_string = "glGetString": fn(name: GLenum) -> *const u8;
    get_integerv = "glGetIntegerv": fn(pname: GLenum, data: *mut GLint);
    viewport = "glViewport": fn(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    scissor = "glScissor": fn(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    enable = "glEnable": fn(cap: GLenum);
    disable = "glDisable": fn(cap: GLenum);
    blend_func_separate = "glBlendFuncSeparate": fn(src_rgb: GLenum, dst_rgb: GLenum, src_a: GLenum, dst_a: GLenum);
    blend_equation = "glBlendEquation": fn(mode: GLenum);
    clear_color = "glClearColor": fn(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat);
    clear = "glClear": fn(mask: GLbitfield);
    color_mask = "glColorMask": fn(r: GLboolean, g: GLboolean, b: GLboolean, a: GLboolean);
    pixel_storei = "glPixelStorei": fn(pname: GLenum, param: GLint);
    read_pixels = "glReadPixels": fn(x: GLint, y: GLint, w: GLsizei, h: GLsizei, format: GLenum, ty: GLenum, data: *mut c_void);
    flush = "glFlush": fn();
    finish = "glFinish": fn();

    create_shader = "glCreateShader": fn(ty: GLenum) -> GLuint;
    delete_shader = "glDeleteShader": fn(shader: GLuint);
    shader_source = "glShaderSource": fn(shader: GLuint, count: GLsizei, strings: *const *const u8, lengths: *const GLint);
    compile_shader = "glCompileShader": fn(shader: GLuint);
    get_shaderiv = "glGetShaderiv": fn(shader: GLuint, pname: GLenum, out: *mut GLint);
    get_shader_info_log = "glGetShaderInfoLog": fn(shader: GLuint, max: GLsizei, len: *mut GLsizei, log: *mut u8);
    create_program = "glCreateProgram": fn() -> GLuint;
    delete_program = "glDeleteProgram": fn(program: GLuint);
    attach_shader = "glAttachShader": fn(program: GLuint, shader: GLuint);
    bind_attrib_location = "glBindAttribLocation": fn(program: GLuint, index: GLuint, name: *const u8);
    link_program = "glLinkProgram": fn(program: GLuint);
    get_programiv = "glGetProgramiv": fn(program: GLuint, pname: GLenum, out: *mut GLint);
    get_program_info_log = "glGetProgramInfoLog": fn(program: GLuint, max: GLsizei, len: *mut GLsizei, log: *mut u8);
    use_program = "glUseProgram": fn(program: GLuint);
    get_uniform_location = "glGetUniformLocation": fn(program: GLuint, name: *const u8) -> GLint;
    uniform1i = "glUniform1i": fn(loc: GLint, v: GLint);
    uniform1f = "glUniform1f": fn(loc: GLint, v: GLfloat);
    uniform2f = "glUniform2f": fn(loc: GLint, x: GLfloat, y: GLfloat);
    uniform3f = "glUniform3f": fn(loc: GLint, x: GLfloat, y: GLfloat, z: GLfloat);
    uniform4f = "glUniform4f": fn(loc: GLint, x: GLfloat, y: GLfloat, z: GLfloat, w: GLfloat);
    uniform1fv = "glUniform1fv": fn(loc: GLint, count: GLsizei, v: *const GLfloat);
    uniform4fv = "glUniform4fv": fn(loc: GLint, count: GLsizei, v: *const GLfloat);
    uniform_matrix4fv = "glUniformMatrix4fv": fn(loc: GLint, count: GLsizei, transpose: GLboolean, v: *const GLfloat);

    gen_buffers = "glGenBuffers": fn(n: GLsizei, out: *mut GLuint);
    delete_buffers = "glDeleteBuffers": fn(n: GLsizei, names: *const GLuint);
    bind_buffer = "glBindBuffer": fn(target: GLenum, buffer: GLuint);
    buffer_data = "glBufferData": fn(target: GLenum, size: GLsizeiptr, data: *const c_void, usage: GLenum);
    buffer_sub_data = "glBufferSubData": fn(target: GLenum, offset: GLintptr, size: GLsizeiptr, data: *const c_void);
    gen_vertex_arrays = "glGenVertexArrays": fn(n: GLsizei, out: *mut GLuint);
    delete_vertex_arrays = "glDeleteVertexArrays": fn(n: GLsizei, names: *const GLuint);
    bind_vertex_array = "glBindVertexArray": fn(vao: GLuint);
    enable_vertex_attrib_array = "glEnableVertexAttribArray": fn(index: GLuint);
    vertex_attrib_pointer = "glVertexAttribPointer": fn(index: GLuint, size: GLint, ty: GLenum, normalized: GLboolean, stride: GLsizei, offset: *const c_void);
    vertex_attrib_divisor = "glVertexAttribDivisor": fn(index: GLuint, divisor: GLuint);
    draw_arrays = "glDrawArrays": fn(mode: GLenum, first: GLint, count: GLsizei);
    draw_elements = "glDrawElements": fn(mode: GLenum, count: GLsizei, ty: GLenum, offset: *const c_void);
    draw_arrays_instanced = "glDrawArraysInstanced": fn(mode: GLenum, first: GLint, count: GLsizei, instances: GLsizei);

    gen_textures = "glGenTextures": fn(n: GLsizei, out: *mut GLuint);
    delete_textures = "glDeleteTextures": fn(n: GLsizei, names: *const GLuint);
    bind_texture = "glBindTexture": fn(target: GLenum, texture: GLuint);
    active_texture = "glActiveTexture": fn(unit: GLenum);
    tex_image_2d = "glTexImage2D": fn(target: GLenum, level: GLint, internal: GLint, w: GLsizei, h: GLsizei, border: GLint, format: GLenum, ty: GLenum, data: *const c_void);
    tex_sub_image_2d = "glTexSubImage2D": fn(target: GLenum, level: GLint, x: GLint, y: GLint, w: GLsizei, h: GLsizei, format: GLenum, ty: GLenum, data: *const c_void);
    tex_parameteri = "glTexParameteri": fn(target: GLenum, pname: GLenum, param: GLint);
    generate_mipmap = "glGenerateMipmap": fn(target: GLenum);

    gen_framebuffers = "glGenFramebuffers": fn(n: GLsizei, out: *mut GLuint);
    delete_framebuffers = "glDeleteFramebuffers": fn(n: GLsizei, names: *const GLuint);
    bind_framebuffer = "glBindFramebuffer": fn(target: GLenum, fb: GLuint);
    framebuffer_texture_2d = "glFramebufferTexture2D": fn(target: GLenum, attachment: GLenum, textarget: GLenum, texture: GLuint, level: GLint);
    check_framebuffer_status = "glCheckFramebufferStatus": fn(target: GLenum) -> GLenum;
}

/// Kind of context, which determines the shader dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// OpenGL ES 3.x (Android, Linux EGL).
    Gles3,
    /// WebGL 2 in a browser.
    WebGl2,
    /// Desktop OpenGL 3.3+ core profile.
    Core33,
}

/// A loaded GL context binding. All methods must be called on the thread
/// that owns the current context.
pub struct Gl {
    f: Fns,
    flavor: Flavor,
}

impl Gl {
    /// # Safety
    /// See [`Fns::load`]; a context of the given flavor must be current.
    pub unsafe fn load(
        flavor: Flavor,
        get: &mut dyn FnMut(&str) -> *const c_void,
    ) -> Result<Gl, String> {
        Ok(Gl {
            f: Fns::load(get)?,
            flavor,
        })
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    /// Raw function table for anything not wrapped here.
    pub fn raw(&self) -> &Fns {
        &self.f
    }

    /// `#version` line plus default precision for GLSL ES 3.00 style shaders.
    pub fn shader_header(&self) -> &'static str {
        match self.flavor {
            Flavor::Gles3 | Flavor::WebGl2 => {
                "#version 300 es\nprecision highp float;\nprecision highp int;\n"
            }
            Flavor::Core33 => "#version 330 core\n",
        }
    }

    pub fn get_error(&self) -> GLenum {
        unsafe { (self.f.get_error)() }
    }

    pub fn get_string(&self, name: GLenum) -> String {
        unsafe {
            let p = (self.f.get_string)(name);
            if p.is_null() {
                return String::new();
            }
            let mut len = 0;
            while *p.add(len) != 0 && len < 4096 {
                len += 1;
            }
            String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
        }
    }

    pub fn get_integer(&self, pname: GLenum) -> i32 {
        let mut v = 0;
        unsafe { (self.f.get_integerv)(pname, &mut v) };
        v
    }

    pub fn viewport(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { (self.f.viewport)(x, y, w, h) }
    }
    pub fn scissor(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { (self.f.scissor)(x, y, w, h) }
    }
    pub fn enable(&self, cap: GLenum) {
        unsafe { (self.f.enable)(cap) }
    }
    pub fn disable(&self, cap: GLenum) {
        unsafe { (self.f.disable)(cap) }
    }
    pub fn blend_func_separate(
        &self,
        src_rgb: GLenum,
        dst_rgb: GLenum,
        src_a: GLenum,
        dst_a: GLenum,
    ) {
        unsafe { (self.f.blend_func_separate)(src_rgb, dst_rgb, src_a, dst_a) }
    }
    pub fn blend_func(&self, src: GLenum, dst: GLenum) {
        self.blend_func_separate(src, dst, src, dst)
    }
    pub fn blend_equation(&self, mode: GLenum) {
        unsafe { (self.f.blend_equation)(mode) }
    }
    pub fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) {
        unsafe { (self.f.clear_color)(r, g, b, a) }
    }
    pub fn clear(&self, mask: GLbitfield) {
        unsafe { (self.f.clear)(mask) }
    }
    pub fn color_mask(&self, r: bool, g: bool, b: bool, a: bool) {
        unsafe { (self.f.color_mask)(r as u8, g as u8, b as u8, a as u8) }
    }
    pub fn pixel_store(&self, pname: GLenum, param: i32) {
        unsafe { (self.f.pixel_storei)(pname, param) }
    }
    /// Reads RGBA8 pixels of the bound read framebuffer (bottom row first).
    pub fn read_pixels_rgba(&self, x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
        let mut buf = vec![0u8; (w.max(0) * h.max(0) * 4) as usize];
        unsafe {
            (self.f.read_pixels)(
                x,
                y,
                w,
                h,
                RGBA,
                UNSIGNED_BYTE,
                buf.as_mut_ptr() as *mut c_void,
            )
        };
        buf
    }
    pub fn flush(&self) {
        unsafe { (self.f.flush)() }
    }
    pub fn finish(&self) {
        unsafe { (self.f.finish)() }
    }

    // ---- shaders and programs -------------------------------------------

    /// Compiles a shader; on failure returns the info log.
    pub fn compile_shader(&self, ty: GLenum, source: &str) -> Result<GLuint, String> {
        unsafe {
            let s = (self.f.create_shader)(ty);
            let ptr = source.as_ptr();
            let len = source.len() as GLint;
            (self.f.shader_source)(s, 1, &ptr, &len);
            (self.f.compile_shader)(s);
            let mut ok = 0;
            (self.f.get_shaderiv)(s, COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut log = vec![0u8; 4096];
                let mut n = 0;
                (self.f.get_shader_info_log)(s, log.len() as GLsizei, &mut n, log.as_mut_ptr());
                log.truncate(n.max(0) as usize);
                (self.f.delete_shader)(s);
                return Err(String::from_utf8_lossy(&log).into_owned());
            }
            Ok(s)
        }
    }

    /// Compiles and links a program. `attribs` binds attribute names to
    /// locations before linking.
    pub fn create_program(
        &self,
        vs: &str,
        fs: &str,
        attribs: &[(u32, &str)],
    ) -> Result<GLuint, String> {
        let header = self.shader_header();
        let v = self
            .compile_shader(VERTEX_SHADER, &format!("{header}{vs}"))
            .map_err(|e| format!("vertex shader: {e}"))?;
        let f = match self.compile_shader(FRAGMENT_SHADER, &format!("{header}{fs}")) {
            Ok(f) => f,
            Err(e) => {
                unsafe { (self.f.delete_shader)(v) };
                return Err(format!("fragment shader: {e}"));
            }
        };
        unsafe {
            let p = (self.f.create_program)();
            (self.f.attach_shader)(p, v);
            (self.f.attach_shader)(p, f);
            for (loc, name) in attribs {
                let c = cstr(name);
                (self.f.bind_attrib_location)(p, *loc, c.as_ptr());
            }
            (self.f.link_program)(p);
            (self.f.delete_shader)(v);
            (self.f.delete_shader)(f);
            let mut ok = 0;
            (self.f.get_programiv)(p, LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut log = vec![0u8; 4096];
                let mut n = 0;
                (self.f.get_program_info_log)(p, log.len() as GLsizei, &mut n, log.as_mut_ptr());
                log.truncate(n.max(0) as usize);
                (self.f.delete_program)(p);
                return Err(format!("link: {}", String::from_utf8_lossy(&log)));
            }
            Ok(p)
        }
    }

    pub fn delete_program(&self, p: GLuint) {
        unsafe { (self.f.delete_program)(p) }
    }
    pub fn use_program(&self, p: GLuint) {
        unsafe { (self.f.use_program)(p) }
    }
    pub fn uniform_location(&self, p: GLuint, name: &str) -> GLint {
        let c = cstr(name);
        unsafe { (self.f.get_uniform_location)(p, c.as_ptr()) }
    }
    pub fn uniform1i(&self, loc: GLint, v: i32) {
        unsafe { (self.f.uniform1i)(loc, v) }
    }
    pub fn uniform1f(&self, loc: GLint, v: f32) {
        unsafe { (self.f.uniform1f)(loc, v) }
    }
    pub fn uniform2f(&self, loc: GLint, x: f32, y: f32) {
        unsafe { (self.f.uniform2f)(loc, x, y) }
    }
    pub fn uniform3f(&self, loc: GLint, x: f32, y: f32, z: f32) {
        unsafe { (self.f.uniform3f)(loc, x, y, z) }
    }
    pub fn uniform4f(&self, loc: GLint, x: f32, y: f32, z: f32, w: f32) {
        unsafe { (self.f.uniform4f)(loc, x, y, z, w) }
    }
    pub fn uniform1fv(&self, loc: GLint, v: &[f32]) {
        unsafe { (self.f.uniform1fv)(loc, v.len() as GLsizei, v.as_ptr()) }
    }
    pub fn uniform4fv(&self, loc: GLint, v: &[f32]) {
        unsafe { (self.f.uniform4fv)(loc, (v.len() / 4) as GLsizei, v.as_ptr()) }
    }
    /// Column-major 4x4 matrix.
    pub fn uniform_matrix4(&self, loc: GLint, m: &[f32; 16]) {
        unsafe { (self.f.uniform_matrix4fv)(loc, 1, 0, m.as_ptr()) }
    }

    // ---- buffers -----------------------------------------------------------

    pub fn create_buffer(&self) -> GLuint {
        let mut b = 0;
        unsafe { (self.f.gen_buffers)(1, &mut b) };
        b
    }
    pub fn delete_buffer(&self, b: GLuint) {
        unsafe { (self.f.delete_buffers)(1, &b) }
    }
    pub fn bind_buffer(&self, target: GLenum, b: GLuint) {
        unsafe { (self.f.bind_buffer)(target, b) }
    }
    pub fn buffer_data<T: Copy>(&self, target: GLenum, data: &[T], usage: GLenum) {
        unsafe {
            (self.f.buffer_data)(
                target,
                std::mem::size_of_val(data) as GLsizeiptr,
                data.as_ptr() as *const c_void,
                usage,
            )
        }
    }
    /// Allocates an uninitialised buffer of `bytes` bytes.
    pub fn buffer_alloc(&self, target: GLenum, bytes: usize, usage: GLenum) {
        unsafe { (self.f.buffer_data)(target, bytes as GLsizeiptr, std::ptr::null(), usage) }
    }
    pub fn buffer_sub_data<T: Copy>(&self, target: GLenum, offset_bytes: usize, data: &[T]) {
        unsafe {
            (self.f.buffer_sub_data)(
                target,
                offset_bytes as GLintptr,
                std::mem::size_of_val(data) as GLsizeiptr,
                data.as_ptr() as *const c_void,
            )
        }
    }
    pub fn create_vertex_array(&self) -> GLuint {
        let mut v = 0;
        unsafe { (self.f.gen_vertex_arrays)(1, &mut v) };
        v
    }
    pub fn delete_vertex_array(&self, v: GLuint) {
        unsafe { (self.f.delete_vertex_arrays)(1, &v) }
    }
    pub fn bind_vertex_array(&self, v: GLuint) {
        unsafe { (self.f.bind_vertex_array)(v) }
    }
    /// Enables attribute `index` and points it at the bound ARRAY_BUFFER.
    pub fn vertex_attrib(
        &self,
        index: u32,
        size: i32,
        ty: GLenum,
        normalized: bool,
        stride: i32,
        offset: usize,
    ) {
        unsafe {
            (self.f.enable_vertex_attrib_array)(index);
            (self.f.vertex_attrib_pointer)(
                index,
                size,
                ty,
                normalized as u8,
                stride,
                offset as *const c_void,
            );
        }
    }
    pub fn vertex_attrib_divisor(&self, index: u32, divisor: u32) {
        unsafe { (self.f.vertex_attrib_divisor)(index, divisor) }
    }
    pub fn draw_arrays(&self, mode: GLenum, first: i32, count: i32) {
        unsafe { (self.f.draw_arrays)(mode, first, count) }
    }
    pub fn draw_elements_u16(&self, mode: GLenum, count: i32, offset_bytes: usize) {
        unsafe {
            (self.f.draw_elements)(mode, count, UNSIGNED_SHORT, offset_bytes as *const c_void)
        }
    }
    pub fn draw_elements_u32(&self, mode: GLenum, count: i32, offset_bytes: usize) {
        unsafe { (self.f.draw_elements)(mode, count, UNSIGNED_INT, offset_bytes as *const c_void) }
    }
    pub fn draw_arrays_instanced(&self, mode: GLenum, first: i32, count: i32, instances: i32) {
        unsafe { (self.f.draw_arrays_instanced)(mode, first, count, instances) }
    }

    // ---- textures ----------------------------------------------------------

    pub fn create_texture(&self) -> GLuint {
        let mut t = 0;
        unsafe { (self.f.gen_textures)(1, &mut t) };
        t
    }
    pub fn delete_texture(&self, t: GLuint) {
        unsafe { (self.f.delete_textures)(1, &t) }
    }
    pub fn active_texture(&self, unit: u32) {
        unsafe { (self.f.active_texture)(TEXTURE0 + unit) }
    }
    pub fn bind_texture(&self, target: GLenum, t: GLuint) {
        unsafe { (self.f.bind_texture)(target, t) }
    }
    pub fn tex_parameter(&self, target: GLenum, pname: GLenum, param: GLenum) {
        unsafe { (self.f.tex_parameteri)(target, pname, param as GLint) }
    }
    /// Uploads (or allocates when `data` is `None`) a 2D texture level.
    pub fn tex_image_2d(
        &self,
        internal: GLenum,
        w: i32,
        h: i32,
        format: GLenum,
        ty: GLenum,
        data: Option<&[u8]>,
    ) {
        unsafe {
            (self.f.tex_image_2d)(
                TEXTURE_2D,
                0,
                internal as GLint,
                w,
                h,
                0,
                format,
                ty,
                data.map(|d| d.as_ptr() as *const c_void)
                    .unwrap_or(std::ptr::null()),
            )
        }
    }
    pub fn tex_sub_image_2d(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        format: GLenum,
        ty: GLenum,
        data: &[u8],
    ) {
        unsafe {
            (self.f.tex_sub_image_2d)(
                TEXTURE_2D,
                0,
                x,
                y,
                w,
                h,
                format,
                ty,
                data.as_ptr() as *const c_void,
            )
        }
    }
    pub fn generate_mipmap(&self, target: GLenum) {
        unsafe { (self.f.generate_mipmap)(target) }
    }

    // ---- framebuffers ------------------------------------------------------

    pub fn create_framebuffer(&self) -> GLuint {
        let mut f = 0;
        unsafe { (self.f.gen_framebuffers)(1, &mut f) };
        f
    }
    pub fn delete_framebuffer(&self, f: GLuint) {
        unsafe { (self.f.delete_framebuffers)(1, &f) }
    }
    pub fn bind_framebuffer(&self, f: GLuint) {
        unsafe { (self.f.bind_framebuffer)(FRAMEBUFFER, f) }
    }
    pub fn framebuffer_texture(&self, texture: GLuint) {
        unsafe {
            (self.f.framebuffer_texture_2d)(FRAMEBUFFER, COLOR_ATTACHMENT0, TEXTURE_2D, texture, 0)
        }
    }
    pub fn framebuffer_complete(&self) -> bool {
        unsafe { (self.f.check_framebuffer_status)(FRAMEBUFFER) == FRAMEBUFFER_COMPLETE }
    }
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}
