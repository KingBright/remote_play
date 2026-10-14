/* Explicit local fixture validation. No display, network or user input. */
#define _POSIX_C_SOURCE 200809L
#include "linux_video_bridge.h"
#include <libavcodec/avcodec.h>
#include <libavutil/mem.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <time.h>
#include <dirent.h>
static RpLinuxFrame *retained[3];
static unsigned frames=0;
static uint64_t max_map_ns=0;
static uint64_t ns(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return (uint64_t)t.tv_sec*1000000000+t.tv_nsec;}
static int fd_count(void){DIR*d=opendir("/proc/self/fd");if(!d)return -1;int n=0;struct dirent*e;while((e=readdir(d)))if(e->d_name[0]!='.')++n;closedir(d);return n;}
static void descriptor(const RpLinuxFrameInfo *i) {
 printf("{\"bits\":%d,\"visible\":[%u,%u],\"allocation\":[%u,%u],\"matrix\":%d,\"primaries\":%d,\"transfer\":%d,\"range\":%d,\"objects\":[",i->pixel_format,i->visible_width,i->visible_height,i->allocation_width,i->allocation_height,i->color_matrix,i->color_primaries,i->color_transfer,i->color_range);
 for(unsigned j=0;j<i->object_count;++j){if(j)printf(",");printf("{\"fd_valid\":%s,\"bytes\":%llu,\"modifier\":\"0x%llx\"}",fcntl(i->objects[j].fd,F_GETFD)>=0?"true":"false",(unsigned long long)i->objects[j].bytes,(unsigned long long)i->objects[j].modifier);}
 printf("],\"layers\":[");
 for(unsigned j=0;j<i->layer_count;++j){if(j)printf(",");printf("{\"fourcc\":%u,\"planes\":[",i->layers[j].fourcc);for(unsigned k=0;k<i->layers[j].plane_count;++k){if(k)printf(",");RpDmaPlane p=i->layers[j].planes[k];printf("{\"object\":%u,\"offset\":%llu,\"pitch\":%llu}",p.object_index,(unsigned long long)p.offset,(unsigned long long)p.pitch);}printf("]}");}
 printf("]}");
}
static int drain(RpLinuxDecoder*d) {
 for(;;){RpLinuxFrame*f=NULL;uint64_t start=ns();int r=rp_linux_decoder_receive(d,&f);uint64_t elapsed=ns()-start;if(r<=0)return r;
  if(elapsed>max_map_ns)max_map_ns=elapsed;
  const RpLinuxFrameInfo*i=rp_linux_frame_info(f);
  for(unsigned j=0;j<i->object_count;++j)if(fcntl(i->objects[j].fd,F_GETFD)<0){rp_linux_frame_release(&f);return AVERROR(EBADF);}
  if(frames==0){printf("NATIVE_DRM_FIRST ");descriptor(i);printf("\n");}
  unsigned slot=frames%3;rp_linux_frame_release(&retained[slot]);retained[slot]=f;++frames;
  if(frames>1000)return AVERROR(E2BIG);
 }
}
static int packet(RpLinuxDecoder*d,uint8_t*p,int n,int64_t pts) {
 int r=rp_linux_decoder_submit(d,p,(size_t)n,pts);
 if(r==AVERROR(EAGAIN)){r=drain(d);if(r<0)return r;r=rp_linux_decoder_submit(d,p,(size_t)n,pts);}
 return r<0?r:drain(d);
}
int main(int argc,char**argv) {
 if(argc!=3){fprintf(stderr,"usage: native-drm-probe explicit-synthetic.hevc expected-frame-count\n");return 2;}
 unsigned expected=(unsigned)strtoul(argv[2],NULL,10);if(expected==0||expected>1000)return 2;
 int before=fd_count(),r=0;FILE*input=fopen(argv[1],"rb");if(!input){perror("fixture");return 2;}
 AVCodecParserContext*parser=av_parser_init(AV_CODEC_ID_HEVC);AVCodecContext*parse_context=avcodec_alloc_context3(NULL);RpLinuxDecoder*d=NULL;
 if(!parser||!parse_context){r=AVERROR(ENOMEM);goto done;}
 r=rp_linux_decoder_create("/dev/dri/renderD128",&d);if(r<0)goto done;
 uint8_t buffer[32768+AV_INPUT_BUFFER_PADDING_SIZE];unsigned packets=0;
 for(;;){size_t size=fread(buffer,1,32768,input);if(ferror(input)){r=AVERROR(EIO);goto done;}if(!size)break;memset(buffer+size,0,AV_INPUT_BUFFER_PADDING_SIZE);uint8_t*p=buffer;
  while(size){uint8_t*out=NULL;int bytes=0;int used=av_parser_parse2(parser,parse_context,&out,&bytes,p,(int)size,AV_NOPTS_VALUE,AV_NOPTS_VALUE,0);if(used<0){r=used;goto done;}if(used==0&&bytes==0){r=AVERROR_INVALIDDATA;goto done;}p+=used;size-=used;if(bytes){r=packet(d,out,bytes,(int64_t)packets++*33);if(r<0)goto done;}}
 }
 {uint8_t*out=NULL;int bytes=0;r=av_parser_parse2(parser,parse_context,&out,&bytes,NULL,0,AV_NOPTS_VALUE,AV_NOPTS_VALUE,0);if(r<0)goto done;if(bytes){r=packet(d,out,bytes,(int64_t)packets++*33);if(r<0)goto done;}}
 r=rp_linux_decoder_flush(d);if(r<0)goto done;r=drain(d);
 if(r>=0&&frames!=expected)r=AVERROR_INVALIDDATA;
done:
 for(unsigned i=0;i<3;++i)rp_linux_frame_release(&retained[i]);
 rp_linux_decoder_destroy(&d);if(parser)av_parser_close(parser);avcodec_free_context(&parse_context);fclose(input);
 if(r<0){char text[128];rp_linux_error_string(r,text,sizeof(text));fprintf(stderr,"NATIVE_DRM_FAILED code=%d %s frames=%u\n",r,text,frames);return 1;}
 int after=fd_count();printf("NATIVE_DRM_RESULT {\"passed\":true,\"hardware_frames\":%u,\"direct_mapped_frames\":%u,\"retained_frame_limit\":3,\"map_max_us\":%llu,\"fd_before\":%d,\"fd_after\":%d,\"pixel_readback\":false,\"gpui_presented\":false,\"network\":false}\n",frames,frames,(unsigned long long)(max_map_ns/1000),before,after);
 return 0;
}
