#version 450
// Compile: glslc -O quad.frag -o quad.frag.spv
layout(push_constant) uniform Push {
    vec4 rect;
    vec4 color;
} pc;
layout(location = 0) out vec4 out_color;
void main() {
    out_color = pc.color;
}
