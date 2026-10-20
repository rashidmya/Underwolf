#version 450
// Compile: glslc -O quad.vert -o quad.vert.spv
layout(push_constant) uniform Push {
    vec4 rect;   // NDC x0, y0, x1, y1
    vec4 color;  // premultiplied RGBA
} pc;
layout(location = 0) out vec2 uv;
void main() {
    vec2 c = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    uv = c;
    gl_Position = vec4(mix(pc.rect.xy, pc.rect.zw, c), 0.0, 1.0);
}
