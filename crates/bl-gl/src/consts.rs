//! OpenGL enum values used by the project (identical across GL ES 3.0,
//! WebGL 2 and desktop GL 3.3).

use crate::GLenum;

pub const NO_ERROR: GLenum = 0;
pub const VENDOR: GLenum = 0x1F00;
pub const RENDERER: GLenum = 0x1F01;
pub const VERSION: GLenum = 0x1F02;
pub const SHADING_LANGUAGE_VERSION: GLenum = 0x8B8C;
pub const MAX_TEXTURE_SIZE: GLenum = 0x0D33;

pub const DEPTH_BUFFER_BIT: GLenum = 0x0000_0100;
pub const COLOR_BUFFER_BIT: GLenum = 0x0000_4000;

pub const POINTS: GLenum = 0x0000;
pub const LINES: GLenum = 0x0001;
pub const TRIANGLES: GLenum = 0x0004;
pub const TRIANGLE_STRIP: GLenum = 0x0005;

pub const BLEND: GLenum = 0x0BE2;
pub const SCISSOR_TEST: GLenum = 0x0C11;
pub const DEPTH_TEST: GLenum = 0x0B71;
pub const CULL_FACE: GLenum = 0x0B44;
pub const FRAMEBUFFER_SRGB: GLenum = 0x8DB9;

pub const ZERO: GLenum = 0;
pub const ONE: GLenum = 1;
pub const SRC_ALPHA: GLenum = 0x0302;
pub const ONE_MINUS_SRC_ALPHA: GLenum = 0x0303;
pub const DST_ALPHA: GLenum = 0x0304;
pub const ONE_MINUS_DST_ALPHA: GLenum = 0x0305;
pub const FUNC_ADD: GLenum = 0x8006;

pub const BYTE: GLenum = 0x1400;
pub const UNSIGNED_BYTE: GLenum = 0x1401;
pub const SHORT: GLenum = 0x1402;
pub const UNSIGNED_SHORT: GLenum = 0x1403;
pub const INT: GLenum = 0x1404;
pub const UNSIGNED_INT: GLenum = 0x1405;
pub const FLOAT: GLenum = 0x1406;
pub const HALF_FLOAT: GLenum = 0x140B;

pub const RED: GLenum = 0x1903;
pub const RGB: GLenum = 0x1907;
pub const RGBA: GLenum = 0x1908;
pub const R8: GLenum = 0x8229;
pub const RGBA8: GLenum = 0x8058;
pub const RGBA16F: GLenum = 0x881A;

pub const UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub const PACK_ALIGNMENT: GLenum = 0x0D05;

pub const TEXTURE_2D: GLenum = 0x0DE1;
pub const TEXTURE0: GLenum = 0x84C0;
pub const TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const TEXTURE_WRAP_S: GLenum = 0x2802;
pub const TEXTURE_WRAP_T: GLenum = 0x2803;
pub const NEAREST: GLenum = 0x2600;
pub const LINEAR: GLenum = 0x2601;
pub const LINEAR_MIPMAP_LINEAR: GLenum = 0x2703;
pub const CLAMP_TO_EDGE: GLenum = 0x812F;
pub const REPEAT: GLenum = 0x2901;
pub const MIRRORED_REPEAT: GLenum = 0x8370;

pub const ARRAY_BUFFER: GLenum = 0x8892;
pub const ELEMENT_ARRAY_BUFFER: GLenum = 0x8893;
pub const STATIC_DRAW: GLenum = 0x88E4;
pub const DYNAMIC_DRAW: GLenum = 0x88E8;
pub const STREAM_DRAW: GLenum = 0x88E0;

pub const FRAGMENT_SHADER: GLenum = 0x8B30;
pub const VERTEX_SHADER: GLenum = 0x8B31;
pub const COMPILE_STATUS: GLenum = 0x8B81;
pub const LINK_STATUS: GLenum = 0x8B82;

pub const FRAMEBUFFER: GLenum = 0x8D40;
pub const COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub const FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
pub const FRAMEBUFFER_BINDING: GLenum = 0x8CA6;
