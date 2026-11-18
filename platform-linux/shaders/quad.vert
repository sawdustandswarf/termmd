#version 450
// One instanced quad per rectangle; corners come from gl_VertexIndex
// (triangle strip of four). Layout matches termmd_core::render::quads::Quad.

layout(location = 0) in uvec2 in_pos;
layout(location = 1) in uvec2 in_size;
layout(location = 2) in uvec2 in_uv;
layout(location = 3) in vec4 in_color; // rgb, coverage for solid quads
layout(location = 4) in uint in_kind;  // 0 solid, 1 atlas mask, 2 atlas image

layout(push_constant) uniform Push {
    vec2 inv_viewport; // 1 / buffer size in pixels
} pc;

layout(location = 0) flat out vec4 v_color;
layout(location = 1) flat out ivec2 v_texel_offset; // atlas texel minus framebuffer pixel
layout(location = 2) flat out uint v_kind;

void main() {
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    vec2 p = vec2(in_pos) + corner * vec2(in_size);
    gl_Position = vec4(p * pc.inv_viewport * 2.0 - 1.0, 0.0, 1.0);
    v_color = in_color;
    v_texel_offset = ivec2(in_uv) - ivec2(in_pos);
    v_kind = in_kind;
}
