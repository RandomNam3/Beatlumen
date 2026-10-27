//! GPU resource wrappers: textures, render targets, programs, full-screen
//! passes.

use oc_gl::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// A 2D texture.
#[derive(Debug)]
pub struct Texture {
    pub id: GLuint,
    pub width: u32,
    pub height: u32,
}

impl Texture {
    /// Creates an RGBA8 texture from straight-alpha pixels (top row first).
    /// The image is flipped while uploading so that texture coordinate
    /// v = 0 is the bottom row, as GL expects.
    pub fn from_rgba(gl: &Gl, width: u32, height: u32, pixels: &[u8], mipmaps: bool) -> Texture {
        let row = width as usize * 4;
        let mut flipped = Vec::with_capacity(pixels.len());
        for y in (0..height as usize).rev() {
            flipped.extend_from_slice(&pixels[y * row..(y + 1) * row]);
        }
        let id = gl.create_texture();
        gl.bind_texture(TEXTURE_2D, id);
        gl.pixel_store(UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(RGBA8, width as i32, height as i32, RGBA, UNSIGNED_BYTE, Some(&flipped));
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_MAG_FILTER, LINEAR);
        if mipmaps {
            gl.generate_mipmap(TEXTURE_2D);
            gl.tex_parameter(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR_MIPMAP_LINEAR);
        } else {
            gl.tex_parameter(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR);
        }
        Texture { id, width, height }
    }

    /// Creates a single channel (R8) texture, used for glyph atlases.
    pub fn new_r8(gl: &Gl, width: u32, height: u32) -> Texture {
        let id = gl.create_texture();
        gl.bind_texture(TEXTURE_2D, id);
        gl.pixel_store(UNPACK_ALIGNMENT, 1);
        let zeros = vec![0u8; (width * height) as usize];
        gl.tex_image_2d(R8, width as i32, height as i32, RED, UNSIGNED_BYTE, Some(&zeros));
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_MAG_FILTER, LINEAR);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR);
        Texture { id, width, height }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height.max(1) as f32
    }

    pub fn delete(&self, gl: &Gl) {
        gl.delete_texture(self.id);
    }
}

/// An offscreen colour buffer.
#[derive(Debug)]
pub struct RenderTarget {
    pub fbo: GLuint,
    pub texture: GLuint,
    pub width: u32,
    pub height: u32,
}

impl RenderTarget {
    pub fn new(gl: &Gl, width: u32, height: u32) -> RenderTarget {
        let width = width.max(1);
        let height = height.max(1);
        let texture = gl.create_texture();
        gl.bind_texture(TEXTURE_2D, texture);
        gl.tex_image_2d(RGBA8, width as i32, height as i32, RGBA, UNSIGNED_BYTE, None);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_MAG_FILTER, LINEAR);
        gl.tex_parameter(TEXTURE_2D, TEXTURE_MIN_FILTER, LINEAR);
        let fbo = gl.create_framebuffer();
        gl.bind_framebuffer(fbo);
        gl.framebuffer_texture(texture);
        gl.bind_framebuffer(0);
        RenderTarget { fbo, texture, width, height }
    }

    pub fn resize(&mut self, gl: &Gl, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return;
        }
        gl.bind_texture(TEXTURE_2D, self.texture);
        gl.tex_image_2d(RGBA8, width as i32, height as i32, RGBA, UNSIGNED_BYTE, None);
        self.width = width;
        self.height = height;
    }

    /// Binds the framebuffer and sets the viewport to its size.
    pub fn bind(&self, gl: &Gl) {
        gl.bind_framebuffer(self.fbo);
        gl.viewport(0, 0, self.width as i32, self.height as i32);
    }

    pub fn delete(&self, gl: &Gl) {
        gl.delete_framebuffer(self.fbo);
        gl.delete_texture(self.texture);
    }
}

/// Where a pass renders to.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    /// The default framebuffer (screen) with the given size.
    Screen(u32, u32),
    Offscreen(&'a RenderTarget),
}

impl Target<'_> {
    pub fn bind(&self, gl: &Gl) {
        match self {
            Target::Screen(w, h) => {
                gl.bind_framebuffer(0);
                gl.viewport(0, 0, *w as i32, *h as i32);
            }
            Target::Offscreen(rt) => rt.bind(gl),
        }
    }
    pub fn size(&self) -> (u32, u32) {
        match self {
            Target::Screen(w, h) => (*w, *h),
            Target::Offscreen(rt) => (rt.width, rt.height),
        }
    }
}

/// A linked shader program with cached uniform locations.
pub struct Program {
    pub id: GLuint,
    locations: RefCell<HashMap<&'static str, GLint>>,
}

impl Program {
    pub fn new(gl: &Gl, vs: &str, fs: &str, attribs: &[(u32, &str)]) -> Result<Program, String> {
        Ok(Program { id: gl.create_program(vs, fs, attribs)?, locations: RefCell::new(HashMap::new()) })
    }

    pub fn loc(&self, gl: &Gl, name: &'static str) -> GLint {
        if let Some(&l) = self.locations.borrow().get(name) {
            return l;
        }
        let l = gl.uniform_location(self.id, name);
        self.locations.borrow_mut().insert(name, l);
        l
    }

    pub fn use_program(&self, gl: &Gl) {
        gl.use_program(self.id);
    }

    pub fn set_f(&self, gl: &Gl, name: &'static str, v: f32) {
        gl.uniform1f(self.loc(gl, name), v);
    }
    pub fn set_i(&self, gl: &Gl, name: &'static str, v: i32) {
        gl.uniform1i(self.loc(gl, name), v);
    }
    pub fn set_2f(&self, gl: &Gl, name: &'static str, x: f32, y: f32) {
        gl.uniform2f(self.loc(gl, name), x, y);
    }
    pub fn set_3f(&self, gl: &Gl, name: &'static str, x: f32, y: f32, z: f32) {
        gl.uniform3f(self.loc(gl, name), x, y, z);
    }
    pub fn set_4f(&self, gl: &Gl, name: &'static str, x: f32, y: f32, z: f32, w: f32) {
        gl.uniform4f(self.loc(gl, name), x, y, z, w);
    }
    pub fn set_mat4(&self, gl: &Gl, name: &'static str, m: &crate::math::Mat4) {
        gl.uniform_matrix4(self.loc(gl, name), &m.0);
    }
    /// Binds `texture` to unit `unit` and points sampler `name` at it.
    pub fn set_texture(&self, gl: &Gl, name: &'static str, unit: u32, texture: GLuint) {
        gl.active_texture(unit);
        gl.bind_texture(TEXTURE_2D, texture);
        gl.uniform1i(self.loc(gl, name), unit as i32);
    }

    pub fn delete(&self, gl: &Gl) {
        gl.delete_program(self.id);
    }
}

/// Vertex shader for full-screen passes: a single triangle covering the
/// viewport, generated from gl_VertexID (no vertex buffer needed).
pub const FULLSCREEN_VS: &str = r#"
out vec2 vUv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    vUv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// Draws full-screen triangles (needs a bound, possibly empty, VAO).
pub struct Fullscreen {
    vao: GLuint,
}

impl Fullscreen {
    pub fn new(gl: &Gl) -> Fullscreen {
        Fullscreen { vao: gl.create_vertex_array() }
    }

    pub fn draw(&self, gl: &Gl) {
        gl.bind_vertex_array(self.vao);
        gl.draw_arrays(TRIANGLES, 0, 3);
    }

    pub fn delete(&self, gl: &Gl) {
        gl.delete_vertex_array(self.vao);
    }
}
