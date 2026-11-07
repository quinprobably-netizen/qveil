#version 440
// The veil: a slow, domain-warped swirl in the theme's dyes, steeped over a
// blurred thumbnail. The picture survives only as light and shade.
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    float strength;  // 0 = the picture as it is, 1 = fully steeped
    float aspect;    // width / height, so the swirl isn't stretched
    float seed;      // a different pattern per card
    float drift;     // moves the swirl; animated only while the dye drains
    float vignette;  // 0 = solid, 1 = fades out toward the edges
    float zoom;      // pattern scale: below 1 gives broader swirls (small items)
    vec4 accent;
    vec4 deep;
};
layout(binding = 1) uniform sampler2D source;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float noise(vec2 p) {
    vec2 i = floor(p), f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
    float v = 0.0, a = 0.5;
    for (int k = 0; k < 5; k++) { v += a * noise(p); p *= 2.03; a *= 0.5; }
    return v;
}

void main() {
    vec2 uv = qt_TexCoord0;
    vec4 src = texture(source, uv);
    vec3 rgb = src.a > 0.0 ? src.rgb / src.a : vec3(0.0);
    float lum = dot(rgb, vec3(0.299, 0.587, 0.114));

    vec2 p = vec2(uv.x * aspect, uv.y) * 2.2 * zoom + seed;
    vec2 q = vec2(fbm(p + drift * 0.6), fbm(p + vec2(5.2, 1.3) - drift * 0.4));
    vec2 r = vec2(fbm(p + 4.0 * q + vec2(1.7, 9.2) + drift * 0.3), fbm(p + 4.0 * q + vec2(8.3, 2.8)));
    float f = fbm(p + 4.0 * r);
    vec3 dye = mix(deep.rgb, accent.rgb, smoothstep(0.25, 0.85, f));
    dye = mix(dye, vec3(0.96, 0.82, 0.52), smoothstep(0.72, 0.98, f) * 0.4);

    vec3 col = mix(rgb, dye * (0.35 + 1.05 * lum), strength);
    float edge = 1.0;
    if (vignette > 0.0) {
        vec2 d = (uv - 0.5) * 2.0;
        edge = 1.0 - smoothstep(0.35, 1.0, length(d)) * vignette;
    }
    float a = src.a * edge * qt_Opacity;
    fragColor = vec4(col * a, a);
}
