#ifndef REMOTEPLAY_LINUX_VIDEO_BRIDGE_H
#define REMOTEPLAY_LINUX_VIDEO_BRIDGE_H
#include <stdint.h>
#include <stddef.h>
/* The bridge is only an ABI boundary. Rust owns scheduling, generations, queues,
 * input safety and presentation. It never returns a CPU pixel mapping. */
typedef struct RpLinuxDecoder RpLinuxDecoder;
typedef struct RpLinuxFrame RpLinuxFrame;
typedef struct {
    int32_t fd;
    uint64_t bytes;
    uint64_t modifier;
} RpDmaObject;
typedef struct {
    uint32_t object_index;
    uint64_t offset;
    uint64_t pitch;
} RpDmaPlane;
typedef struct {
    uint32_t fourcc;
    uint32_t plane_count;
    RpDmaPlane planes[4];
} RpDmaLayer;
typedef struct {
    uint32_t abi_version;
    uint32_t visible_width, visible_height;
    uint32_t allocation_width, allocation_height;
    uint64_t crop_left,crop_top,crop_right,crop_bottom;
    int32_t pixel_format;
    int32_t color_range,color_primaries,color_transfer,color_matrix,chroma_location;
    int32_t aspect_num,aspect_den;
    int64_t pts;
    uint32_t object_count,layer_count;
    RpDmaObject objects[4];
    RpDmaLayer layers[4];
} RpLinuxFrameInfo;
/* Errors use FFmpeg AVERROR values. The exact runtime ABI is checked first. */
int rp_linux_decoder_create(const char *render_node, RpLinuxDecoder **out);
void rp_linux_decoder_destroy(RpLinuxDecoder **decoder);
int rp_linux_decoder_submit(RpLinuxDecoder *decoder,const uint8_t *compressed,size_t bytes,int64_t pts);
int rp_linux_decoder_flush(RpLinuxDecoder *decoder);
/* 1=frame, 0=need input or drained, <0=error. A frame keeps its native decode
 * resources and exported FDs alive. The renderer duplicates FDs before import
 * and must release its GPU use before dropping this frame. */
int rp_linux_decoder_receive(RpLinuxDecoder *decoder,RpLinuxFrame **out);
const RpLinuxFrameInfo *rp_linux_frame_info(const RpLinuxFrame *frame);
void rp_linux_frame_release(RpLinuxFrame **frame);
int rp_linux_decoder_reset(RpLinuxDecoder *decoder);
size_t rp_linux_frame_info_size(void);
void rp_linux_error_string(int error,char *buffer,size_t capacity);
#endif
