#include "linux_video_bridge.h"
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_drm.h>
#include <libavutil/error.h>
#include <libavutil/version.h>
#include <errno.h>
#include <limits.h>
#include <string.h>
#include <stdlib.h>

/* Compile against the explicitly verified headers and reject another major ABI.
 * Struct offsets must never be guessed from the presence of a shared library. */
#if LIBAVCODEC_VERSION_MAJOR != 62 || LIBAVUTIL_VERSION_MAJOR != 60
#error "This bridge requires an audited FFmpeg 8.x public ABI (codec62/util60)"
#endif
struct RpLinuxDecoder {AVCodecContext *codec; AVBufferRef *device;};
struct RpLinuxFrame {AVFrame *mapped; AVFrame *decoded; RpLinuxFrameInfo info;};
static enum AVPixelFormat choose_hardware(AVCodecContext *ctx,const enum AVPixelFormat *formats) {
    (void)ctx;
    for(const enum AVPixelFormat *p=formats;*p!=AV_PIX_FMT_NONE;++p)
        if(*p==AV_PIX_FMT_VAAPI)return *p;
    return AV_PIX_FMT_NONE; /* Never accept an unrequested software fallback. */
}
int rp_linux_decoder_create(const char *render_node,RpLinuxDecoder **out) {
    if(!out||!render_node||render_node[0]!='/')return AVERROR(EINVAL);
    *out=NULL;
    if((avcodec_version()>>16)!=LIBAVCODEC_VERSION_MAJOR||(avutil_version()>>16)!=LIBAVUTIL_VERSION_MAJOR)return AVERROR(ENOSYS);
    const AVCodec *codec=avcodec_find_decoder(AV_CODEC_ID_HEVC);
    if(!codec)return AVERROR_DECODER_NOT_FOUND;
    int supported=0;
    for(int i=0;;++i) {
        const AVCodecHWConfig *cfg=avcodec_get_hw_config(codec,i);
        if(!cfg)break;
        if(cfg->device_type==AV_HWDEVICE_TYPE_VAAPI&&cfg->pix_fmt==AV_PIX_FMT_VAAPI&&(cfg->methods&AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX)){supported=1;break;}
    }
    if(!supported)return AVERROR(ENOSYS);
    RpLinuxDecoder *d=calloc(1,sizeof(*d));if(!d)return AVERROR(ENOMEM);
    int result=av_hwdevice_ctx_create(&d->device,AV_HWDEVICE_TYPE_VAAPI,render_node,NULL,0);
    if(result<0){free(d);return result;}
    d->codec=avcodec_alloc_context3(codec);
    if(!d->codec){rp_linux_decoder_destroy(&d);return AVERROR(ENOMEM);}
    d->codec->get_format=choose_hardware;
    d->codec->hw_device_ctx=av_buffer_ref(d->device);
    d->codec->thread_count=1;
    d->codec->pkt_timebase=(AVRational){1,1000};
    if(!d->codec->hw_device_ctx){rp_linux_decoder_destroy(&d);return AVERROR(ENOMEM);}
    result=avcodec_open2(d->codec,codec,NULL);
    if(result<0){rp_linux_decoder_destroy(&d);return result;}
    *out=d;return 0;
}
void rp_linux_decoder_destroy(RpLinuxDecoder **ptr) {
    if(!ptr||!*ptr)return;
    RpLinuxDecoder *d=*ptr;*ptr=NULL;
    avcodec_free_context(&d->codec);av_buffer_unref(&d->device);free(d);
}
int rp_linux_decoder_submit(RpLinuxDecoder *d,const uint8_t *compressed,size_t bytes,int64_t pts) {
    if(!d||!compressed||!bytes||bytes>(8u<<20)||bytes>INT_MAX)return AVERROR(EINVAL);
    AVPacket *packet=av_packet_alloc();if(!packet)return AVERROR(ENOMEM);
    int result=av_new_packet(packet,(int)bytes);
    if(result>=0) {
        /* Copy compressed NAL bytes with the decoder's required tail padding.
         * This is not an uncompressed image copy or a CPU colorspace conversion. */
        memcpy(packet->data,compressed,bytes);packet->pts=pts;packet->dts=AV_NOPTS_VALUE;
        result=avcodec_send_packet(d->codec,packet);
    }
    av_packet_free(&packet);return result;
}
int rp_linux_decoder_flush(RpLinuxDecoder *d) {return d?avcodec_send_packet(d->codec,NULL):AVERROR(EINVAL);}
int rp_linux_decoder_reset(RpLinuxDecoder *d) {if(!d)return AVERROR(EINVAL);avcodec_flush_buffers(d->codec);return 0;}
void rp_linux_frame_release(RpLinuxFrame **ptr) {
    if(!ptr||!*ptr)return;
    RpLinuxFrame *f=*ptr;*ptr=NULL;
    av_frame_free(&f->mapped);av_frame_free(&f->decoded);free(f);
}
const RpLinuxFrameInfo *rp_linux_frame_info(const RpLinuxFrame *f) {return f?&f->info:NULL;}
static int describe(RpLinuxFrame *f) {
    AVFrame *v=f->decoded,*map=f->mapped;
    if(v->format!=AV_PIX_FMT_VAAPI||map->format!=AV_PIX_FMT_DRM_PRIME||!map->data[0]||!v->hw_frames_ctx)return AVERROR(EINVAL);
    AVHWFramesContext *hw=(void*)v->hw_frames_ctx->data;
    if(hw->sw_format!=AV_PIX_FMT_NV12&&hw->sw_format!=AV_PIX_FMT_P010LE)return AVERROR(ENOSYS);
    if(v->width<2||v->height<2||hw->width<v->width||hw->height<v->height||hw->width>16384||hw->height>16384)return AVERROR(EINVAL);
    if(v->crop_left>=(uint64_t)v->width||v->crop_top>=(uint64_t)v->height||
       v->crop_right>=(uint64_t)v->width-v->crop_left||
       v->crop_bottom>=(uint64_t)v->height-v->crop_top)return AVERROR(EINVAL);
    RpLinuxFrameInfo *out=&f->info;
    out->abi_version=1;out->visible_width=v->width;out->visible_height=v->height;
    out->allocation_width=hw->width;out->allocation_height=hw->height;
    out->pixel_format=hw->sw_format==AV_PIX_FMT_NV12?8:10;
    out->crop_left=v->crop_left;out->crop_right=v->crop_right;out->crop_top=v->crop_top;out->crop_bottom=v->crop_bottom;
    out->color_range=v->color_range;out->color_primaries=v->color_primaries;out->color_transfer=v->color_trc;out->color_matrix=v->colorspace;out->chroma_location=v->chroma_location;
    out->aspect_num=v->sample_aspect_ratio.num;out->aspect_den=v->sample_aspect_ratio.den;out->pts=v->pts;
    const AVDRMFrameDescriptor *desc=(const void*)map->data[0];
    if(desc->nb_objects<1||desc->nb_objects>4||desc->nb_layers<1||desc->nb_layers>4)return AVERROR(EINVAL);
    out->object_count=desc->nb_objects;out->layer_count=desc->nb_layers;
    for(int i=0;i<desc->nb_objects;++i) {
        const AVDRMObjectDescriptor *object=&desc->objects[i];
        if(object->fd<0||object->size==0)return AVERROR(EINVAL);
        out->objects[i]=(RpDmaObject){object->fd,object->size,object->format_modifier};
    }
    int total_planes=0;
    for(int i=0;i<desc->nb_layers;++i) {
        const AVDRMLayerDescriptor *layer=&desc->layers[i];
        if(layer->nb_planes<1||layer->nb_planes>4||total_planes+layer->nb_planes>4)return AVERROR(EINVAL);
        total_planes+=layer->nb_planes;
        out->layers[i].fourcc=layer->format;out->layers[i].plane_count=layer->nb_planes;
        for(int j=0;j<layer->nb_planes;++j) {
            const AVDRMPlaneDescriptor *p=&layer->planes[j];
            if(p->object_index<0||p->object_index>=desc->nb_objects||p->offset<0||p->pitch<=0)return AVERROR(EINVAL);
            if((uint64_t)p->offset>=out->objects[p->object_index].bytes)return AVERROR(EINVAL);
            out->layers[i].planes[j]=(RpDmaPlane){p->object_index,(uint64_t)p->offset,(uint64_t)p->pitch};
        }
    }
    return 0;
}
int rp_linux_decoder_receive(RpLinuxDecoder *d,RpLinuxFrame **out) {
    if(!d||!out)return AVERROR(EINVAL);
    *out=NULL;
    RpLinuxFrame *f=calloc(1,sizeof(*f));if(!f)return AVERROR(ENOMEM);
    f->decoded=av_frame_alloc();f->mapped=av_frame_alloc();
    if(!f->decoded||!f->mapped){rp_linux_frame_release(&f);return AVERROR(ENOMEM);}
    int result=avcodec_receive_frame(d->codec,f->decoded);
    if(result==AVERROR(EAGAIN)||result==AVERROR_EOF){rp_linux_frame_release(&f);return 0;}
    if(result<0){rp_linux_frame_release(&f);return result;}
    if(f->decoded->format!=AV_PIX_FMT_VAAPI){rp_linux_frame_release(&f);return AVERROR(ENOSYS);}
    f->mapped->format=AV_PIX_FMT_DRM_PRIME;
    /* DIRECT rejects a mapping that requires copying. READ makes FFmpeg wait for
     * decoder completion before exporting. Call this only on the native worker,
     * never a network/GUI thread. No CPU pixel buffer is requested. */
    result=av_hwframe_map(f->mapped,f->decoded,AV_HWFRAME_MAP_READ|AV_HWFRAME_MAP_DIRECT);
    if(result>=0)result=describe(f);
    if(result<0){rp_linux_frame_release(&f);return result;}
    *out=f;return 1;
}
void rp_linux_error_string(int error,char *buffer,size_t cap) {if(buffer&&cap)av_strerror(error,buffer,cap);}

size_t rp_linux_frame_info_size(void){return sizeof(RpLinuxFrameInfo);}
