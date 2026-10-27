//! Batched 2D renderer for the user interface: anti-aliased rounded
//! rectangles, outlines, soft shadows, images and glyphs from a single
//! channel atlas, with scissor clipping.

use crate::gpu::{Program, Texture};
use crate::shaders::{UI_FS, UI_VS};
use bl_gl::*;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect { x, y, w: (r - x).max(0.0), h: (b - y).max(0.0) }
    }
    pub fn shrink(&self, d: f32) -> Rect {
        Rect { x: self.x + d, y: self.y + d, w: (self.w - 2.0 * d).max(0.0), h: (self.h - 2.0 * d).max(0.0) }
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// Straight (non-premultiplied) RGBA colour, components 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    pub const WHITE: Color = Color(1.0, 1.0, 1.0, 1.0);
    pub const BLACK: Color = Color(0.0, 0.0, 0.0, 1.0);
    pub const TRANSPARENT: Color = Color(0.0, 0.0, 0.0, 0.0);
    pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
        Color(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
    }
    pub fn hex(rgb: u32) -> Color {
        Color::rgba((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 1.0)
    }
    pub fn with_alpha(self, a: f32) -> Color {
        Color(self.0, self.1, self.2, a)
    }
    pub fn mul_alpha(self, a: f32) -> Color {
        Color(self.0, self.1, self.2, self.3 * a)
    }
    pub fn lerp(self, o: Color, t: f32) -> Color {
        Color(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t, self.3 + (o.3 - self.3) * t)
    }
    fn to_u8(self) -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [c(self.0), c(self.1), c(self.2), c(self.3)]
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
    local: [f32; 2],
    shape: [f32; 4],
}

const MODE_SOLID: f32 = 0.0;
const MODE_GLYPH: f32 = 1.0;
const MODE_IMAGE: f32 = 2.0;
const MODE_SHADOW: f32 = 3.0;
const MODE_OUTLINE: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Cmd {
    start: u32,
    count: u32,
    image: GLuint,
    clip: Option<Rect>,
}

pub struct Batch2d {
    program: Program,
    vao: GLuint,
    vbo: GLuint,
    ibo: GLuint,
    vbo_capacity: usize,
    ibo_capacity: usize,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    cmds: Vec<Cmd>,
    clip_stack: Vec<Rect>,
    image: GLuint,
    white: Texture,
    pub atlas: Texture,
    screen: (f32, f32),
    scale: f32,
}

pub const ATLAS_SIZE: u32 = 2048;

impl Batch2d {
    pub fn new(gl: &Gl) -> Result<Batch2d, String> {
        let program = Program::new(gl, UI_VS, UI_FS, &[(0, "position"), (1, "uv"), (2, "color"), (3, "local"), (4, "shape")])?;
        let vao = gl.create_vertex_array();
        gl.bind_vertex_array(vao);
        let vbo = gl.create_buffer();
        gl.bind_buffer(ARRAY_BUFFER, vbo);
        let stride = std::mem::size_of::<Vertex>() as i32;
        gl.vertex_attrib(0, 2, FLOAT, false, stride, 0);
        gl.vertex_attrib(1, 2, FLOAT, false, stride, 8);
        gl.vertex_attrib(2, 4, UNSIGNED_BYTE, true, stride, 16);
        gl.vertex_attrib(3, 2, FLOAT, false, stride, 20);
        gl.vertex_attrib(4, 4, FLOAT, false, stride, 28);
        let ibo = gl.create_buffer();
        gl.bind_buffer(ELEMENT_ARRAY_BUFFER, ibo);
        gl.bind_vertex_array(0);
        let white = Texture::from_rgba(gl, 1, 1, &[255, 255, 255, 255], false);
        let atlas = Texture::new_r8(gl, ATLAS_SIZE, ATLAS_SIZE);
        Ok(Batch2d {
            program,
            vao,
            vbo,
            ibo,
            vbo_capacity: 0,
            ibo_capacity: 0,
            vertices: Vec::new(),
            indices: Vec::new(),
            cmds: Vec::new(),
            clip_stack: Vec::new(),
            image: 0,
            white,
            atlas,
            screen: (1.0, 1.0),
            scale: 1.0,
        })
    }

    /// Uploads coverage data into the glyph atlas.
    pub fn atlas_upload(&self, gl: &Gl, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        if w == 0 || h == 0 {
            return;
        }
        gl.bind_texture(TEXTURE_2D, self.atlas.id);
        gl.pixel_store(UNPACK_ALIGNMENT, 1);
        gl.tex_sub_image_2d(x as i32, y as i32, w as i32, h as i32, RED, UNSIGNED_BYTE, data);
    }

    /// Starts a frame. `width`/`height` are in logical units; `scale` is
    /// the number of physical pixels per logical unit.
    pub fn begin(&mut self, width: f32, height: f32, scale: f32) {
        self.screen = (width.max(1.0), height.max(1.0));
        self.scale = scale.max(0.1);
        self.vertices.clear();
        self.indices.clear();
        self.cmds.clear();
        self.clip_stack.clear();
        self.image = self.white.id;
    }

    pub fn screen(&self) -> (f32, f32) {
        self.screen
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn current_clip(&self) -> Option<Rect> {
        self.clip_stack.last().copied()
    }

    pub fn push_clip(&mut self, r: Rect) {
        let r = match self.clip_stack.last() {
            Some(c) => c.intersect(&r),
            None => r,
        };
        self.clip_stack.push(r);
    }

    pub fn pop_clip(&mut self) {
        self.clip_stack.pop();
    }

    fn cmd_for(&mut self, image: GLuint) {
        let clip = self.clip_stack.last().copied();
        let start = self.indices.len() as u32;
        match self.cmds.last_mut() {
            Some(c) if c.image == image && c.clip == clip => {}
            _ => self.cmds.push(Cmd { start, count: 0, image, clip }),
        }
    }

    fn quad(&mut self, r: Rect, pad: f32, uv: [f32; 4], color: Color, shape: [f32; 4], image: GLuint) {
        if let Some(c) = self.clip_stack.last() {
            if r.x - pad >= c.right() || r.y - pad >= c.bottom() || r.right() + pad <= c.x || r.bottom() + pad <= c.y {
                return;
            }
        }
        self.cmd_for(image);
        let base = self.vertices.len() as u32;
        let (cx, cy) = r.center();
        let x0 = r.x - pad;
        let y0 = r.y - pad;
        let x1 = r.right() + pad;
        let y1 = r.bottom() + pad;
        let col = color.to_u8();
        // UVs are interpolated over the padded quad for images/glyphs.
        let (u0, v0, u1, v1) = (uv[0], uv[1], uv[2], uv[3]);
        let du = if r.w > 0.0 { (u1 - u0) / r.w * pad } else { 0.0 };
        let dv = if r.h > 0.0 { (v1 - v0) / r.h * pad } else { 0.0 };
        let corners = [(x0, y0, u0 - du, v0 - dv), (x1, y0, u1 + du, v0 - dv), (x1, y1, u1 + du, v1 + dv), (x0, y1, u0 - du, v1 + dv)];
        for (x, y, u, v) in corners {
            self.vertices.push(Vertex { pos: [x, y], uv: [u, v], color: col, local: [x - cx, y - cy], shape });
        }
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        if let Some(c) = self.cmds.last_mut() {
            c.count += 6;
        }
    }

    /// Filled (rounded) rectangle.
    pub fn rect(&mut self, r: Rect, color: Color, radius: f32) {
        if color.3 <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let img = self.white.id;
        self.quad(r, 1.0, [0.0; 4], color, [r.w / 2.0, r.h / 2.0, rad, MODE_SOLID], img);
    }

    /// Rectangle outline of `width` pixels inside the rectangle bounds.
    pub fn outline(&mut self, r: Rect, color: Color, radius: f32, width: f32) {
        if color.3 <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let img = self.white.id;
        self.quad(r, 1.0, [width, 0.0, width, 0.0], color, [r.w / 2.0, r.h / 2.0, rad, MODE_OUTLINE], img);
    }

    /// Soft drop shadow around `r` with blur radius `blur`.
    pub fn shadow(&mut self, r: Rect, color: Color, blur: f32) {
        let b = blur.max(1.0);
        let outer = Rect::new(r.x - b, r.y - b, r.w + 2.0 * b, r.h + 2.0 * b);
        let img = self.white.id;
        self.quad(outer, 0.0, [0.0; 4], color, [outer.w / 2.0, outer.h / 2.0, b, MODE_SHADOW], img);
    }

    /// Draws a texture (straight alpha RGBA). `uv` = (u0, v0, u1, v1) with
    /// v = 0 at the top of the image as uploaded by `Texture::from_rgba`
    /// (which flips rows), i.e. pass (0, 1, 1, 0) for the full image.
    pub fn image(&mut self, r: Rect, texture: GLuint, uv: [f32; 4], tint: Color, radius: f32) {
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        self.quad(r, 0.0, uv, tint, [r.w / 2.0, r.h / 2.0, rad, MODE_IMAGE], texture);
    }

    /// Draws a glyph/icon from the atlas (pixel coordinates in the atlas).
    pub fn glyph(&mut self, r: Rect, atlas_rect: Rect, color: Color) {
        let s = ATLAS_SIZE as f32;
        let uv = [atlas_rect.x / s, atlas_rect.y / s, atlas_rect.right() / s, atlas_rect.bottom() / s];
        let img = self.image_for_glyph();
        self.quad(r, 0.0, uv, color, [r.w / 2.0, r.h / 2.0, 0.0, MODE_GLYPH], img);
    }

    fn image_for_glyph(&self) -> GLuint {
        // Glyphs only sample the atlas; keep batching with whatever image
        // is bound (use the white texture as neutral key).
        self.white.id
    }

    /// Flushes all queued geometry to the screen (default framebuffer).
    pub fn end(&mut self, gl: &Gl, fb_width: i32, fb_height: i32) {
        self.end_to(gl, 0, fb_width, fb_height);
    }

    /// Like [`Batch2d::end`] but draws into framebuffer `fbo`.
    pub fn end_to(&mut self, gl: &Gl, fbo: u32, fb_width: i32, fb_height: i32) {
        if self.indices.is_empty() {
            return;
        }
        gl.bind_framebuffer(fbo);
        gl.viewport(0, 0, fb_width, fb_height);
        gl.enable(BLEND);
        gl.blend_func_separate(ONE, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA);
        gl.bind_vertex_array(self.vao);
        gl.bind_buffer(ARRAY_BUFFER, self.vbo);
        let vbytes = self.vertices.len() * std::mem::size_of::<Vertex>();
        if vbytes > self.vbo_capacity {
            self.vbo_capacity = vbytes.next_power_of_two();
            gl.buffer_alloc(ARRAY_BUFFER, self.vbo_capacity, STREAM_DRAW);
        }
        // Upload vertex bytes.
        let bytes: Vec<u8> = self.vertices.iter().flat_map(vertex_bytes).collect();
        gl.buffer_sub_data(ARRAY_BUFFER, 0, &bytes);
        gl.bind_buffer(ELEMENT_ARRAY_BUFFER, self.ibo);
        let ibytes = self.indices.len() * 4;
        if ibytes > self.ibo_capacity {
            self.ibo_capacity = ibytes.next_power_of_two();
            gl.buffer_alloc(ELEMENT_ARRAY_BUFFER, self.ibo_capacity, STREAM_DRAW);
        }
        gl.buffer_sub_data(ELEMENT_ARRAY_BUFFER, 0, &self.indices);
        let p = &self.program;
        p.use_program(gl);
        p.set_2f(gl, "screen", self.screen.0, self.screen.1);
        p.set_f(gl, "pixelScale", self.scale);
        p.set_texture(gl, "atlas", 0, self.atlas.id);
        let sx = fb_width as f32 / self.screen.0;
        let sy = fb_height as f32 / self.screen.1;
        let mut scissor = false;
        for c in &self.cmds {
            if c.count == 0 {
                continue;
            }
            p.set_texture(gl, "image", 1, c.image);
            match c.clip {
                Some(r) => {
                    if !scissor {
                        gl.enable(SCISSOR_TEST);
                        scissor = true;
                    }
                    let x = (r.x * sx).floor() as i32;
                    let y = (fb_height as f32 - r.bottom() * sy).floor() as i32;
                    gl.scissor(x, y, (r.w * sx).ceil() as i32, (r.h * sy).ceil() as i32);
                }
                None => {
                    if scissor {
                        gl.disable(SCISSOR_TEST);
                        scissor = false;
                    }
                }
            }
            gl.draw_elements_u32(TRIANGLES, c.count as i32, c.start as usize * 4);
        }
        if scissor {
            gl.disable(SCISSOR_TEST);
        }
        gl.bind_vertex_array(0);
        gl.disable(BLEND);
    }

    pub fn delete(&self, gl: &Gl) {
        gl.delete_vertex_array(self.vao);
        gl.delete_buffer(self.vbo);
        gl.delete_buffer(self.ibo);
        self.white.delete(gl);
        self.atlas.delete(gl);
        self.program.delete(gl);
    }
}

fn vertex_bytes(v: &Vertex) -> [u8; 44] {
    let mut b = [0u8; 44];
    let f = |b: &mut [u8; 44], off: usize, x: f32| b[off..off + 4].copy_from_slice(&x.to_le_bytes());
    f(&mut b, 0, v.pos[0]);
    f(&mut b, 4, v.pos[1]);
    f(&mut b, 8, v.uv[0]);
    f(&mut b, 12, v.uv[1]);
    b[16..20].copy_from_slice(&v.color);
    f(&mut b, 20, v.local[0]);
    f(&mut b, 24, v.local[1]);
    for i in 0..4 {
        f(&mut b, 28 + i * 4, v.shape[i]);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_layout() {
        assert_eq!(std::mem::size_of::<Vertex>(), 44);
        let r = Rect::new(0.0, 0.0, 10.0, 10.0).intersect(&Rect::new(5.0, 5.0, 10.0, 10.0));
        assert_eq!(r, Rect::new(5.0, 5.0, 5.0, 5.0));
    }
}
