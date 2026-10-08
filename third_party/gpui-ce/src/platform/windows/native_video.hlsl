// Video coordinates and color rows come from the same validated cross-platform
// geometry/color contract used for input mapping. No hard-coded BT.601 defaults.
cbuffer VideoParams: register(b1) {
    float4 target_rect;
    float4 clip_rect;
    float4 uv_row0;
    float4 uv_row1;
    float4 visible_uv;
    float4 red_row;
    float4 green_row;
    float4 blue_row;
    float4 view_and_format; // viewport xy, RGB flag, left-sited chroma x offset
};
Texture2D<float4> plane_y : register(t0);
Texture2D<float2> plane_uv : register(t2);
SamplerState video_sampler : register(s0);
struct VideoVarying {float4 position:SV_Position;float2 unit:TEXCOORD0;float4 clip:SV_ClipDistance;};
VideoVarying video_vertex(uint vertex_id:SV_VertexID) {
    float2 unit=float2(float(vertex_id & 1u),0.5*float(vertex_id & 2u));
    float2 p=target_rect.xy+unit*target_rect.zw;
    VideoVarying o;o.position=float4(p/view_and_format.xy*float2(2,-2)+float2(-1,1),0,1);
    o.unit=unit;o.clip=float4(p.x-clip_rect.x,clip_rect.x+clip_rect.z-p.x,p.y-clip_rect.y,clip_rect.y+clip_rect.w-p.y);return o;
}
float4 video_fragment(VideoVarying i):SV_Target {
    float3 q=float3(i.unit,1);
    float2 uv=clamp(float2(dot(uv_row0.xyz,q),dot(uv_row1.xyz,q)),visible_uv.xy,visible_uv.zw);
    float4 sample_y=plane_y.SampleLevel(video_sampler,uv,0);
    if(view_and_format.z>0.5) return float4(sample_y.rgb,1);
    float2 c=plane_uv.SampleLevel(video_sampler,uv+float2(view_and_format.w,0),0);
    float4 yuv=float4(sample_y.r,c,1);
    return float4(saturate(float3(dot(red_row,yuv),dot(green_row,yuv),dot(blue_row,yuv))),1);
}
