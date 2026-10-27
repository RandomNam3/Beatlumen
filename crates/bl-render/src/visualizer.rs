//! Renders an [`bl_fx::FrameView`]: background scenes (image plane +
//! particles + motion blur), the transition compositor and the global
//! filter chain.

use crate::gpu::{Fullscreen, Program, RenderTarget, Target, Texture, FULLSCREEN_VS};
use crate::math::Mat4;
use crate::shaders::*;
use bl_fx::{FilterView, FrameView, SceneView, TransitionKind, TransitionView};
use bl_gl::*;
use std::collections::HashMap;

/// Reference output height for resolution independent blur radii.
const REFERENCE_HEIGHT: f32 = 1080.0;

struct Programs {
    copy: Program,
    blend: Program,
    blur: Program,
    rgb_shift: Program,
    vignette: Program,
    hue_shift: Program,
    kaleidoscope: Program,
    flash: Program,
    zoom_blur: Program,
    grain: Program,
    vignette_blend: Program,
    motion_blur: Program,
    glitch: Program,
    wipe: Program,
    slide: Program,
    blur_transition: Program,
    plane: Program,
    particle: Program,
}

pub struct Visualizer {
    p: Programs,
    fullscreen: Fullscreen,
    plane_vao: GLuint,
    plane_vbo: GLuint,
    particle_vao: GLuint,
    corner_vbo: GLuint,
    instance_vbo: GLuint,
    instance_capacity: usize,
    /// Scene buffers (current, previous) and scratch targets.
    scene: [RenderTarget; 2],
    scene_tmp: RenderTarget,
    ping: RenderTarget,
    pong: RenderTarget,
    blur_tmp: RenderTarget,
    effect_tmp: RenderTarget,
    images: HashMap<u64, Texture>,
    prev_view_proj: HashMap<u64, Mat4>,
    width: u32,
    height: u32,
    /// Fallback colour of the plane without an image.
    pub empty_color: [f32; 3],
}

fn prog(gl: &Gl, fs: &str) -> Result<Program, String> {
    Program::new(gl, FULLSCREEN_VS, fs, &[])
}

impl Visualizer {
    pub fn new(gl: &Gl) -> Result<Visualizer, String> {
        let p = Programs {
            copy: prog(gl, COPY_FS)?,
            blend: prog(gl, BLEND_FS)?,
            blur: prog(gl, BLUR_FS)?,
            rgb_shift: prog(gl, RGB_SHIFT_FS)?,
            vignette: prog(gl, VIGNETTE_FS)?,
            hue_shift: prog(gl, HUE_SHIFT_FS)?,
            kaleidoscope: prog(gl, KALEIDOSCOPE_FS)?,
            flash: prog(gl, FLASH_FS)?,
            zoom_blur: prog(gl, ZOOM_BLUR_FS)?,
            grain: prog(gl, GRAIN_FS)?,
            vignette_blend: prog(gl, VIGNETTE_BLEND_FS)?,
            motion_blur: prog(gl, MOTION_BLUR_FS)?,
            glitch: prog(gl, &glitch_fs())?,
            wipe: prog(gl, WIPE_FS)?,
            slide: prog(gl, SLIDE_FS)?,
            blur_transition: prog(gl, BLUR_TRANSITION_FS)?,
            plane: Program::new(gl, PLANE_VS, PLANE_FS, &[(0, "position"), (1, "uv")])?,
            particle: Program::new(
                gl,
                PARTICLE_VS,
                PARTICLE_FS,
                &[
                    (0, "corner"),
                    (1, "position"),
                    (2, "size"),
                    (3, "gradient"),
                    (4, "opacity"),
                    (5, "color"),
                ],
            )?,
        };
        // Unit plane (-1..1) with UVs, drawn as a triangle strip.
        let plane_vao = gl.create_vertex_array();
        gl.bind_vertex_array(plane_vao);
        let plane_vbo = gl.create_buffer();
        gl.bind_buffer(ARRAY_BUFFER, plane_vbo);
        let verts: [f32; 16] = [
            -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        ];
        gl.buffer_data(ARRAY_BUFFER, &verts, STATIC_DRAW);
        gl.vertex_attrib(0, 2, FLOAT, false, 16, 0);
        gl.vertex_attrib(1, 2, FLOAT, false, 16, 8);

        let particle_vao = gl.create_vertex_array();
        gl.bind_vertex_array(particle_vao);
        let corner_vbo = gl.create_buffer();
        gl.bind_buffer(ARRAY_BUFFER, corner_vbo);
        let corners: [f32; 8] = [-0.5, -0.5, 0.5, -0.5, -0.5, 0.5, 0.5, 0.5];
        gl.buffer_data(ARRAY_BUFFER, &corners, STATIC_DRAW);
        gl.vertex_attrib(0, 2, FLOAT, false, 8, 0);
        let instance_vbo = gl.create_buffer();
        gl.bind_buffer(ARRAY_BUFFER, instance_vbo);
        let stride = 36;
        gl.vertex_attrib(1, 3, FLOAT, false, stride, 0);
        gl.vertex_attrib(2, 1, FLOAT, false, stride, 12);
        gl.vertex_attrib(3, 1, FLOAT, false, stride, 16);
        gl.vertex_attrib(4, 1, FLOAT, false, stride, 20);
        gl.vertex_attrib(5, 3, FLOAT, false, stride, 24);
        for i in 1..=5 {
            gl.vertex_attrib_divisor(i, 1);
        }
        gl.bind_vertex_array(0);

        let rt = || RenderTarget::new(gl, 16, 16);
        Ok(Visualizer {
            p,
            fullscreen: Fullscreen::new(gl),
            plane_vao,
            plane_vbo,
            particle_vao,
            corner_vbo,
            instance_vbo,
            instance_capacity: 0,
            scene: [rt(), rt()],
            scene_tmp: rt(),
            ping: rt(),
            pong: rt(),
            blur_tmp: rt(),
            effect_tmp: rt(),
            images: HashMap::new(),
            prev_view_proj: HashMap::new(),
            width: 16,
            height: 16,
            empty_color: [0.06, 0.055, 0.07],
        })
    }

    /// Uploads a background image (RGBA8, top row first).
    pub fn set_image(&mut self, gl: &Gl, id: u64, width: u32, height: u32, rgba: &[u8]) {
        if let Some(old) = self.images.remove(&id) {
            old.delete(gl);
        }
        self.images
            .insert(id, Texture::from_rgba(gl, width, height, rgba, false));
    }

    pub fn has_image(&self, id: u64) -> bool {
        self.images.contains_key(&id)
    }

    pub fn remove_image(&mut self, gl: &Gl, id: u64) {
        if let Some(t) = self.images.remove(&id) {
            t.delete(gl);
        }
    }

    /// Removes all images not in `keep`.
    pub fn retain_images(&mut self, gl: &Gl, keep: &[u64]) {
        let drop: Vec<u64> = self
            .images
            .keys()
            .filter(|k| !keep.contains(k))
            .copied()
            .collect();
        for id in drop {
            self.remove_image(gl, id);
        }
    }

    fn resize(&mut self, gl: &Gl, w: u32, h: u32) {
        if w == self.width && h == self.height {
            return;
        }
        self.width = w;
        self.height = h;
        let [s0, s1] = &mut self.scene;
        for rt in [
            s0,
            s1,
            &mut self.scene_tmp,
            &mut self.ping,
            &mut self.pong,
            &mut self.blur_tmp,
            &mut self.effect_tmp,
        ] {
            rt.resize(gl, w, h);
        }
    }

    fn view_proj(&self, s: &SceneView) -> Mat4 {
        let aspect = self.width as f32 / self.height.max(1) as f32;
        let proj = Mat4::perspective(s.camera.fov_deg, aspect, 0.001, 2000.0);
        let c = s.camera.position;
        let world = Mat4::translation(c[0], c[1], c[2]).mul(&Mat4::rotation_z(s.camera.rotation));
        proj.mul(&world.inverse())
    }

    /// Renders a scene into `self.scene[slot]` (applying motion blur).
    fn render_scene(
        &mut self,
        gl: &Gl,
        s: &SceneView,
        slot: usize,
        pixel_scale: f32,
        seen: &mut Vec<(u64, Mat4)>,
    ) {
        let vp = self.view_proj(s);
        let target = &self.scene[slot];
        target.bind(gl);
        gl.disable(BLEND);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(COLOR_BUFFER_BIT);
        // Image plane.
        let p = &self.p.plane;
        p.use_program(gl);
        p.set_mat4(gl, "viewProj", &vp);
        p.set_2f(gl, "halfSize", s.plane[0] / 2.0, s.plane[1] / 2.0);
        p.set_3f(
            gl,
            "fallback",
            self.empty_color[0],
            self.empty_color[1],
            self.empty_color[2],
        );
        match s.image.and_then(|i| self.images.get(&i.id)) {
            Some(tex) => {
                p.set_texture(gl, "map", 0, tex.id);
                p.set_i(gl, "hasMap", 1);
            }
            None => p.set_i(gl, "hasMap", 0),
        }
        gl.bind_vertex_array(self.plane_vao);
        gl.draw_arrays(TRIANGLE_STRIP, 0, 4);
        // Particles.
        if !s.particles.is_empty() {
            gl.bind_vertex_array(self.particle_vao);
            gl.bind_buffer(ARRAY_BUFFER, self.instance_vbo);
            let bytes = std::mem::size_of_val(s.particles);
            if bytes > self.instance_capacity {
                self.instance_capacity = bytes.next_power_of_two();
                gl.buffer_alloc(ARRAY_BUFFER, self.instance_capacity, DYNAMIC_DRAW);
            }
            let floats: Vec<f32> = s
                .particles
                .iter()
                .flat_map(|i| {
                    [
                        i.position[0],
                        i.position[1],
                        i.position[2],
                        i.size,
                        i.gradient,
                        i.opacity,
                        i.color[0],
                        i.color[1],
                        i.color[2],
                    ]
                })
                .collect();
            gl.buffer_sub_data(ARRAY_BUFFER, 0, &floats);
            gl.enable(BLEND);
            gl.blend_func_separate(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA);
            let pp = &self.p.particle;
            pp.use_program(gl);
            pp.set_mat4(gl, "viewProj", &vp);
            pp.set_2f(gl, "viewport", self.width as f32, self.height as f32);
            pp.set_f(gl, "pixelScale", pixel_scale);
            gl.draw_arrays_instanced(TRIANGLE_STRIP, 0, 4, s.particles.len() as i32);
            gl.disable(BLEND);
        }
        gl.bind_vertex_array(0);
        // Motion blur on the scene layer.
        let prev = self.prev_view_proj.get(&s.id).copied().unwrap_or(vp);
        seen.push((s.id, vp));
        if let Some(mb) = s.motion_blur {
            if mb.samples > 1 && mb.intensity > 0.0 {
                let p = &self.p.motion_blur;
                self.scene_tmp.bind(gl);
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, self.scene[slot].texture);
                p.set_mat4(gl, "invViewProj", &vp.inverse());
                p.set_mat4(gl, "prevViewProj", &prev);
                p.set_f(gl, "intensity", mb.intensity);
                p.set_i(gl, "samples", mb.samples.min(64) as i32);
                self.fullscreen.draw(gl);
                std::mem::swap(&mut self.scene[slot], &mut self.scene_tmp);
            }
        }
    }

    fn copy(&self, gl: &Gl, src: GLuint, dst: Target) {
        dst.bind(gl);
        let p = &self.p.copy;
        p.use_program(gl);
        p.set_texture(gl, "tDiffuse", 0, src);
        p.set_f(gl, "opacity", 1.0);
        self.fullscreen.draw(gl);
    }

    /// Separable gaussian blur `src` → `dst` (via blur_tmp), `passes` times.
    fn blur(&self, gl: &Gl, src: GLuint, dst: &RenderTarget, radius: f32, passes: u32) {
        let p = &self.p.blur;
        let scale = self.height as f32 / REFERENCE_HEIGHT;
        p.use_program(gl);
        for i in 0..passes.max(1) {
            self.blur_tmp.bind(gl);
            p.set_texture(gl, "tDiffuse", 0, if i == 0 { src } else { dst.texture });
            p.set_f(gl, "radius", radius * scale);
            p.set_f(gl, "resolution", self.width as f32);
            p.set_2f(gl, "direction", 1.0, 0.0);
            self.fullscreen.draw(gl);
            dst.bind(gl);
            p.set_texture(gl, "tDiffuse", 0, self.blur_tmp.texture);
            p.set_f(gl, "resolution", self.height as f32);
            p.set_2f(gl, "direction", 0.0, 1.0);
            self.fullscreen.draw(gl);
        }
    }

    fn apply_filter(&self, gl: &Gl, f: &FilterView, src: &RenderTarget, dst: &RenderTarget) {
        match *f {
            FilterView::Blur { radius, passes } => {
                if passes == 0 {
                    self.copy(gl, src.texture, Target::Offscreen(dst));
                } else {
                    self.blur(gl, src.texture, dst, radius, passes);
                }
            }
            FilterView::Bloom {
                opacity,
                radius,
                passes,
            } => {
                self.blur(gl, src.texture, &self.effect_tmp, radius, passes);
                dst.bind(gl);
                let p = &self.p.blend;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse1", 0, src.texture);
                p.set_texture(gl, "tDiffuse2", 1, self.effect_tmp.texture);
                p.set_f(gl, "opacity", opacity);
                p.set_f(gl, "mixRatio", 0.5);
                self.fullscreen.draw(gl);
            }
            FilterView::RgbShift { amount, angle } => {
                dst.bind(gl);
                let p = &self.p.rgb_shift;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "amount", amount);
                p.set_f(gl, "angle", angle);
                self.fullscreen.draw(gl);
            }
            FilterView::Vignette { offset, darkness } => {
                dst.bind(gl);
                let p = &self.p.vignette;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "offset", offset);
                p.set_f(gl, "darkness", darkness);
                self.fullscreen.draw(gl);
            }
            FilterView::VignetteBlur {
                size,
                radius,
                passes,
            } => {
                self.blur(gl, src.texture, &self.effect_tmp, radius, passes);
                dst.bind(gl);
                let p = &self.p.vignette_blend;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse1", 0, src.texture);
                p.set_texture(gl, "tDiffuse2", 1, self.effect_tmp.texture);
                p.set_f(gl, "size", size);
                self.fullscreen.draw(gl);
            }
            FilterView::Glitch { amount, seed } => {
                self.blur(gl, src.texture, &self.effect_tmp, 3.0, 2);
                dst.bind(gl);
                let p = &self.p.glitch;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse1", 0, src.texture);
                p.set_texture(gl, "tDiffuse2", 1, self.effect_tmp.texture);
                p.set_f(gl, "amount", amount);
                p.set_f(gl, "seed", seed);
                p.set_2f(gl, "resolution", self.width as f32, self.height as f32);
                self.fullscreen.draw(gl);
            }
            FilterView::HueShift { degrees } => {
                dst.bind(gl);
                let p = &self.p.hue_shift;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "angle", degrees.to_radians());
                self.fullscreen.draw(gl);
            }
            FilterView::Kaleidoscope {
                amount,
                segments,
                angle,
            } => {
                dst.bind(gl);
                let p = &self.p.kaleidoscope;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "amount", amount);
                p.set_f(gl, "segments", segments as f32);
                p.set_f(gl, "angle", angle);
                p.set_2f(gl, "resolution", self.width as f32, self.height as f32);
                self.fullscreen.draw(gl);
            }
            FilterView::Flash { amount, color } => {
                dst.bind(gl);
                let p = &self.p.flash;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "amount", amount);
                p.set_3f(gl, "color", color[0], color[1], color[2]);
                self.fullscreen.draw(gl);
            }
            FilterView::ZoomBlur { strength } => {
                dst.bind(gl);
                let p = &self.p.zoom_blur;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "strength", strength);
                self.fullscreen.draw(gl);
            }
            FilterView::Grain {
                amount,
                scanlines,
                time,
            } => {
                dst.bind(gl);
                let p = &self.p.grain;
                p.use_program(gl);
                p.set_texture(gl, "tDiffuse", 0, src.texture);
                p.set_f(gl, "amount", amount);
                p.set_f(gl, "scanlines", scanlines);
                p.set_f(gl, "time", time);
                p.set_2f(gl, "resolution", self.width as f32, self.height as f32);
                self.fullscreen.draw(gl);
            }
        }
    }

    fn transition(
        &self,
        gl: &Gl,
        t: &TransitionView,
        prev: GLuint,
        cur: GLuint,
        dst: &RenderTarget,
    ) {
        dst.bind(gl);
        let bind_pair = |p: &Program| {
            p.use_program(gl);
            p.set_texture(gl, "tDiffuse1", 0, prev);
            p.set_texture(gl, "tDiffuse2", 1, cur);
        };
        match t.kind {
            TransitionKind::Blend => {
                let p = &self.p.blend;
                bind_pair(p);
                p.set_f(gl, "opacity", 1.0);
                p.set_f(gl, "mixRatio", t.amount);
            }
            TransitionKind::Blur { intensity, samples } => {
                let p = &self.p.blur_transition;
                bind_pair(p);
                p.set_f(gl, "amount", t.amount);
                p.set_f(gl, "prevAmount", t.prev_amount);
                p.set_f(gl, "intensity", intensity);
                p.set_i(gl, "samples", samples.min(64) as i32);
            }
            TransitionKind::Wipe {
                direction,
                gradient,
                angle_deg,
            } => {
                let p = &self.p.wipe;
                bind_pair(p);
                p.set_f(gl, "amount", t.amount);
                p.set_f(gl, "gradient", gradient);
                p.set_i(gl, "direction", direction.clamp(0, 3));
                p.set_f(gl, "angle", angle_deg.to_radians());
                p.set_f(gl, "aspect", self.width as f32 / self.height.max(1) as f32);
            }
            TransitionKind::Slide {
                direction,
                slides,
                intensity,
                samples,
            } => {
                let p = &self.p.slide;
                bind_pair(p);
                p.set_i(gl, "slides", slides.max(0));
                p.set_f(gl, "amount", t.amount);
                p.set_f(gl, "prevAmount", t.prev_amount);
                p.set_f(gl, "intensity", intensity);
                p.set_i(gl, "direction", direction.clamp(0, 3));
                p.set_i(gl, "samples", samples.min(64) as i32);
            }
            TransitionKind::Glitch { seed } => {
                let p = &self.p.glitch;
                bind_pair(p);
                p.set_f(gl, "amount", t.amount);
                p.set_f(gl, "seed", seed);
                p.set_2f(gl, "resolution", self.width as f32, self.height as f32);
            }
        }
        self.fullscreen.draw(gl);
    }

    /// Renders a frame. `render_size` is the internal resolution (may be
    /// smaller than the output for performance); `pixel_scale` converts
    /// logical pixels (particle sizes) to render pixels.
    pub fn render(
        &mut self,
        gl: &Gl,
        frame: &FrameView,
        render_size: (u32, u32),
        out: Target,
        pixel_scale: f32,
    ) {
        self.resize(gl, render_size.0.max(1), render_size.1.max(1));
        gl.disable(BLEND);
        gl.disable(SCISSOR_TEST);
        gl.disable(DEPTH_TEST);
        gl.disable(CULL_FACE);
        let mut seen = Vec::new();
        self.render_scene(gl, &frame.current, 0, pixel_scale, &mut seen);
        let mut src_is_scene = true;
        if let (Some(prev), Some(t)) = (&frame.previous, &frame.transition) {
            self.render_scene(gl, prev, 1, pixel_scale, &mut seen);
            let (a, b) = (self.scene[1].texture, self.scene[0].texture);
            self.transition(gl, t, a, b, &self.ping);
            src_is_scene = false;
        }
        self.prev_view_proj.clear();
        self.prev_view_proj.extend(seen);

        // Global filter chain (ping-pong between ping and pong).
        let mut src = if src_is_scene { 0 } else { 1 };
        for f in &frame.filters {
            let dst = if src == 1 { 2 } else { 1 };
            self.apply_filter(gl, f, self.target_ref(src), self.target_ref(dst));
            src = dst;
        }
        self.copy(gl, self.target_ref(src).texture, out);
    }

    fn target_ref(&self, i: usize) -> &RenderTarget {
        match i {
            0 => &self.scene[0],
            1 => &self.ping,
            _ => &self.pong,
        }
    }

    /// Frees all GPU resources.
    pub fn delete(&mut self, gl: &Gl) {
        for t in self.images.values() {
            t.delete(gl);
        }
        self.images.clear();
        for rt in [
            &self.scene[0],
            &self.scene[1],
            &self.scene_tmp,
            &self.ping,
            &self.pong,
            &self.blur_tmp,
            &self.effect_tmp,
        ] {
            rt.delete(gl);
        }
        gl.delete_vertex_array(self.plane_vao);
        gl.delete_vertex_array(self.particle_vao);
        gl.delete_buffer(self.plane_vbo);
        gl.delete_buffer(self.corner_vbo);
        gl.delete_buffer(self.instance_vbo);
        self.fullscreen.delete(gl);
        for p in [
            &self.p.copy,
            &self.p.blend,
            &self.p.blur,
            &self.p.rgb_shift,
            &self.p.vignette,
            &self.p.vignette_blend,
            &self.p.motion_blur,
            &self.p.glitch,
            &self.p.wipe,
            &self.p.slide,
            &self.p.blur_transition,
            &self.p.plane,
            &self.p.particle,
        ] {
            p.delete(gl);
        }
    }
}
