#version 450
// Premultiplied "source over" with ONE, ONE_MINUS_SRC_ALPHA blending.

layout(location = 0) flat in vec4 v_color;
layout(location = 1) flat in ivec2 v_texel_offset;
layout(location = 2) flat in uint v_kind;

layout(set = 0, binding = 0) uniform sampler2D atlas; // R8 coverage, or RGBA

layout(location = 0) out vec4 out_color;

void main() {
    float a = v_color.a;
    if (v_kind == 2u) {
        // A colour glyph: premultiplied texels as they are.
        out_color = texelFetch(atlas, ivec2(gl_FragCoord.xy) + v_texel_offset, 0) * a;
        return;
    }
    if (v_kind == 1u) {
        a *= texelFetch(atlas, ivec2(gl_FragCoord.xy) + v_texel_offset, 0).r;
    }
    out_color = vec4(v_color.rgb * a, a);
}
