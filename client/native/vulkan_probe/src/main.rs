//! Real VAAPI -> DRM PRIME -> Vulkan import validation. Only an explicitly
//! generated local fixture is accepted. Readback is a small test-only sample;
//! no CPU video output, screen capture or remote input path is introduced.
use ash::{vk,Entry};
use std::{ffi::{CStr,CString,c_void},os::unix::fs::MetadataExt,ptr,path::Path,time::Instant};
type E=Box<dyn std::error::Error+Send+Sync>;
#[repr(C)]#[derive(Clone,Copy,Debug,Default)]struct Object{fd:i32,bytes:u64,modifier:u64}
#[repr(C)]#[derive(Clone,Copy,Debug,Default)]struct Plane{object_index:u32,offset:u64,pitch:u64}
#[repr(C)]#[derive(Clone,Copy,Debug,Default)]struct Layer{fourcc:u32,plane_count:u32,planes:[Plane;4]}
#[repr(C)]#[derive(Clone,Copy,Debug,Default)]struct FrameInfo{
 abi_version:u32,visible_width:u32,visible_height:u32,allocation_width:u32,allocation_height:u32,
 crop_left:u64,crop_top:u64,crop_right:u64,crop_bottom:u64,pixel_format:i32,
 color_range:i32,color_primaries:i32,color_transfer:i32,color_matrix:i32,chroma_location:i32,
 aspect_num:i32,aspect_den:i32,pts:i64,object_count:u32,layer_count:u32,objects:[Object;4],layers:[Layer;4]
}
unsafe extern "C" {
 fn rp_linux_decoder_create(node:*const i8,out:*mut *mut c_void)->i32;
 fn rp_linux_decoder_destroy(decoder:*mut *mut c_void);
 fn rp_linux_decoder_submit(decoder:*mut c_void,bytes:*const u8,len:usize,pts:i64)->i32;
 fn rp_linux_decoder_flush(decoder:*mut c_void)->i32;
 fn rp_linux_decoder_receive(decoder:*mut c_void,out:*mut *mut c_void)->i32;
 fn rp_linux_frame_info(frame:*const c_void)->*const FrameInfo;
 fn rp_linux_frame_release(frame:*mut *mut c_void);
 fn rp_linux_frame_info_size()->usize;
 fn rp_linux_error_string(error:i32,text:*mut i8,len:usize);
}
fn require(test:bool,msg:&str)->Result<(),E>{if test{Ok(())}else{Err(msg.to_owned().into())}}
fn ffcheck(value:i32)->Result<i32,E>{if value<0{let mut text=[0i8;256];unsafe{rp_linux_error_string(value,text.as_mut_ptr(),text.len());}Err(format!("FFmpeg {value}: {}",unsafe{CStr::from_ptr(text.as_ptr())}.to_string_lossy()).into())}else{Ok(value)}}
struct Decoder(*mut c_void);impl Drop for Decoder{fn drop(&mut self){unsafe{rp_linux_decoder_destroy(&mut self.0);}}}
struct Frame(*mut c_void);impl Drop for Frame{fn drop(&mut self){unsafe{rp_linux_frame_release(&mut self.0);}}}
struct Gpu{_entry:Entry,instance:ash::Instance,physical:vk::PhysicalDevice,device:ash::Device,queue:vk::Queue,family:u32,pool:vk::CommandPool}
impl Drop for Gpu{fn drop(&mut self){unsafe{let _=self.device.device_wait_idle();self.device.destroy_command_pool(self.pool,None);self.device.destroy_device(None);self.instance.destroy_instance(None);}}}
struct Image<'a>{gpu:&'a Gpu,image:vk::Image,memory:vk::DeviceMemory,view:vk::ImageView,extent:vk::Extent3D}
impl Drop for Image<'_>{fn drop(&mut self){unsafe{self.gpu.device.destroy_image_view(self.view,None);self.gpu.device.destroy_image(self.image,None);self.gpu.device.free_memory(self.memory,None);}}}
impl Gpu {
 unsafe fn new(node:&Path)->Result<Self,E>{unsafe{
    let entry=Entry::load()?;let app=CString::new("Weavelo generated native video validation")?;
    let api=entry.try_enumerate_instance_version()?.unwrap_or(vk::API_VERSION_1_0);require(api>=vk::API_VERSION_1_2,"Vulkan 1.2 required")?;
    let application=vk::ApplicationInfo::default().application_name(&app).api_version(vk::API_VERSION_1_2);
    let instance=entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&application),None)?;
    let node_dev=std::fs::metadata(node)?.rdev();let major=libc::major(node_dev);let minor=libc::minor(node_dev);
    let extensions=[ash::khr::external_memory_fd::NAME,ash::ext::external_memory_dma_buf::NAME,ash::ext::image_drm_format_modifier::NAME,ash::ext::queue_family_foreign::NAME,ash::ext::physical_device_drm::NAME];
    let mut chosen=None;
    for pd in instance.enumerate_physical_devices()? {
      let props=instance.get_physical_device_properties(pd);if props.device_type==vk::PhysicalDeviceType::CPU{continue;}
      let ex=instance.enumerate_device_extension_properties(pd)?;
      if !extensions.iter().all(|wanted|ex.iter().any(|e|CStr::from_ptr(e.extension_name.as_ptr())==*wanted)){continue;}
      let mut drm=vk::PhysicalDeviceDrmPropertiesEXT::default();let mut p=vk::PhysicalDeviceProperties2::default().push_next(&mut drm);instance.get_physical_device_properties2(pd,&mut p);
      if drm.has_render==vk::FALSE||drm.render_major!=i64::from(major)||drm.render_minor!=i64::from(minor){continue;}
      let family=instance.get_physical_device_queue_family_properties(pd).iter().position(|q|q.queue_flags.contains(vk::QueueFlags::GRAPHICS)).ok_or("graphics queue unavailable")? as u32;
      chosen=Some((pd,family,CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy().into_owned()));break;
    }
    let (physical,family,name)=chosen.ok_or("no exact render-node GPU with native modifier and foreign ownership extensions")?;
    let priorities=[1f32];let queues=[vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];let names:Vec<_>=extensions.iter().map(|s|s.as_ptr()).collect();
    let device=instance.create_device(physical,&vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&names),None)?;
    let queue=device.get_device_queue(family,0);let pool=device.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),None)?;
    println!("NATIVE_VULKAN_DEVICE {{\"name\":{name:?},\"render_major\":{major},\"render_minor\":{minor},\"software\":false}}");
    Ok(Self{_entry:entry,instance,physical,device,queue,family,pool})
 }}
 unsafe fn import<'a>(&'a self,info:&FrameInfo,index:usize)->Result<Image<'a>,E>{unsafe{
    require(info.abi_version==1&&info.layer_count==2&&index<2,"this validator requires separate luma/chroma layers")?;
    let layer=info.layers[index];require(layer.plane_count==1,"auxiliary modifier planes need an explicitly supported adapter")?;
    let plane=layer.planes[0];require((plane.object_index as usize)<info.object_count as usize&&info.object_count<=4,"invalid backing object")?;
    let obj=info.objects[plane.object_index as usize];require(obj.fd>=0&&obj.bytes>0&&plane.pitch>0&&plane.offset<obj.bytes,"invalid DMA descriptor")?;
    let format=match (info.pixel_format,index,layer.fourcc.to_le_bytes()) {
      (8,0,[b'R',b'8',b' ',b' '])=>vk::Format::R8_UNORM,
      (8,1,[b'G',b'R',b'8',b'8'])=>vk::Format::R8G8_UNORM,
      (10,0,[b'R',b'1',b'6',b' '])=>vk::Format::R16_UNORM,
      (10,1,[b'G',b'R',b'3',b'2'])=>vk::Format::R16G16_UNORM,
      _=>return Err(format!("unsupported layer mapping depth={} layer={} fourcc={:x}",info.pixel_format,index,layer.fourcc).into()),
    };
    let divisor=if index==0{1}else{2};let extent=vk::Extent3D{width:info.allocation_width/divisor,height:info.allocation_height/divisor,depth:1};
    let mut mods=vk::DrmFormatModifierPropertiesListEXT::default();let mut props=vk::FormatProperties2::default().push_next(&mut mods);self.instance.get_physical_device_format_properties2(self.physical,format,&mut props);
    let mut list=vec![vk::DrmFormatModifierPropertiesEXT::default();mods.drm_format_modifier_count as usize];mods.p_drm_format_modifier_properties=list.as_mut_ptr();
    self.instance.get_physical_device_format_properties2(self.physical,format,&mut vk::FormatProperties2::default().push_next(&mut mods));
    let modifier=list.iter().find(|m|m.drm_format_modifier==obj.modifier).ok_or("decoder modifier is not supported for this Vulkan plane format")?;
    require(modifier.drm_format_modifier_plane_count==1,"memory-plane count differs from exported plane metadata")?;
    require(modifier.drm_format_modifier_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE|vk::FormatFeatureFlags::TRANSFER_SRC),"native plane is not sampleable and transferable")?;
    let mut external=vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut drm=vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default().drm_format_modifier(obj.modifier).sharing_mode(vk::SharingMode::EXCLUSIVE);
    let query=vk::PhysicalDeviceImageFormatInfo2::default().format(format).ty(vk::ImageType::TYPE_2D).tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT).usage(vk::ImageUsageFlags::SAMPLED|vk::ImageUsageFlags::TRANSFER_SRC).push_next(&mut external).push_next(&mut drm);
    let mut external_props=vk::ExternalImageFormatProperties::default();let mut result=vk::ImageFormatProperties2::default().push_next(&mut external_props);self.instance.get_physical_device_image_format_properties2(self.physical,&query,&mut result)?;
    require(extent.width<=result.image_format_properties.max_extent.width&&extent.height<=result.image_format_properties.max_extent.height,"native extent exceeds GPU capability")?;
    require(external_props.external_memory_properties.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE),"plane memory cannot be imported")?;
    let layouts=[vk::SubresourceLayout{offset:plane.offset,row_pitch:plane.pitch,size:0,array_pitch:0,depth_pitch:0}];
    let mut layout=vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(obj.modifier).plane_layouts(&layouts);
    let mut shared=vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let desc=vk::ImageCreateInfo::default().image_type(vk::ImageType::TYPE_2D).format(format).extent(extent).mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT).usage(vk::ImageUsageFlags::SAMPLED|vk::ImageUsageFlags::TRANSFER_SRC).sharing_mode(vk::SharingMode::EXCLUSIVE).initial_layout(vk::ImageLayout::UNDEFINED).push_next(&mut layout).push_next(&mut shared);
    let image=self.device.create_image(&desc,None)?;
    let imported=(||->Result<(vk::DeviceMemory,vk::ImageView),E>{
      let req=self.device.get_image_memory_requirements(image);require(req.size<=obj.bytes,"native allocation smaller than image memory requirement")?;
      let fd_loader=ash::khr::external_memory_fd::Device::new(&self.instance,&self.device);let mut fdprops=vk::MemoryFdPropertiesKHR::default();fd_loader.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,obj.fd,&mut fdprops)?;
      let bits=req.memory_type_bits&fdprops.memory_type_bits;require(bits!=0,"no compatible memory type for this imported object")?;
      let index=bits.trailing_zeros();
      let dup=libc::fcntl(obj.fd,libc::F_DUPFD_CLOEXEC,0);if dup<0{return Err(std::io::Error::last_os_error().into());}
      let mut fd=vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(dup);
      let mut dedicated=vk::MemoryDedicatedAllocateInfo::default().image(image);
      let alloc=vk::MemoryAllocateInfo::default().allocation_size(obj.bytes).memory_type_index(index).push_next(&mut fd).push_next(&mut dedicated);
      let memory=match self.device.allocate_memory(&alloc,None){Ok(m)=>m,Err(e)=>{libc::close(dup);return Err(e.into());}};
      let setup=(||->Result<vk::ImageView,E>{
        self.device.bind_image_memory(image,memory,0)?;
        Ok(self.device.create_image_view(&vk::ImageViewCreateInfo::default().image(image).view_type(vk::ImageViewType::TYPE_2D).format(format).subresource_range(vk::ImageSubresourceRange{aspect_mask:vk::ImageAspectFlags::COLOR,base_mip_level:0,level_count:1,base_array_layer:0,layer_count:1}),None)?)
      })();
      match setup{Ok(view)=>Ok((memory,view)),Err(e)=>{self.device.free_memory(memory,None);Err(e)}}
    })();
    match imported{Ok((memory,view))=>Ok(Image{gpu:self,image,memory,view,extent}),Err(e)=>{self.device.destroy_image(image,None);Err(e)}}
 }}
 unsafe fn validate_samples(&self,images:&[Image],info:&FrameInfo,reference:&[u8])->Result<(),E>{unsafe{
    require(images.len()==2,"two video planes required")?;
    let size=4096;let buffer=self.device.create_buffer(&vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_DST).sharing_mode(vk::SharingMode::EXCLUSIVE),None)?;
    let req=self.device.get_buffer_memory_requirements(buffer);let memprops=self.instance.get_physical_device_memory_properties(self.physical);
    let index=(0..memprops.memory_type_count).find(|i|req.memory_type_bits&(1<<i)!=0&&memprops.memory_types[*i as usize].property_flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE|vk::MemoryPropertyFlags::HOST_COHERENT)).ok_or("test readback memory unavailable")?;
    let memory=self.device.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(index),None)?;self.device.bind_buffer_memory(buffer,memory,0)?;
    let command=self.device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(self.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
    self.device.begin_command_buffer(command,&vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
    let range=vk::ImageSubresourceRange{aspect_mask:vk::ImageAspectFlags::COLOR,base_mip_level:0,level_count:1,base_array_layer:0,layer_count:1};
    let barriers:Vec<_>=images.iter().map(|i|vk::ImageMemoryBarrier::default().image(i.image).subresource_range(range).old_layout(vk::ImageLayout::GENERAL).new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL).src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT).dst_queue_family_index(self.family).src_access_mask(vk::AccessFlags::empty()).dst_access_mask(vk::AccessFlags::TRANSFER_READ)).collect();
    self.device.cmd_pipeline_barrier(command,vk::PipelineStageFlags::TOP_OF_PIPE,vk::PipelineStageFlags::TRANSFER,vk::DependencyFlags::empty(),&[],&[],&barriers);
    for (index,i) in images.iter().enumerate(){require(i.extent.width>=16&&i.extent.height>=16,"test sample extent too small")?;
      let divisor=if index==0 {1} else {2};let w=info.visible_width/divisor;let h=info.visible_height/divisor;
      for (sample,[x,y]) in [[4,4],[w/2-2,h/2-2],[w-8,h-8]].into_iter().enumerate(){
        let region=vk::BufferImageCopy::default().buffer_offset(((index*3+sample)*256) as u64).image_subresource(vk::ImageSubresourceLayers{aspect_mask:vk::ImageAspectFlags::COLOR,mip_level:0,base_array_layer:0,layer_count:1}).image_offset(vk::Offset3D{x:x as i32,y:y as i32,z:0}).image_extent(vk::Extent3D{width:4,height:4,depth:1});
        self.device.cmd_copy_image_to_buffer(command,i.image,vk::ImageLayout::TRANSFER_SRC_OPTIMAL,buffer,&[region]);
      }
    }
    let release:Vec<_>=images.iter().map(|i|vk::ImageMemoryBarrier::default().image(i.image).subresource_range(range).old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL).new_layout(vk::ImageLayout::GENERAL).src_queue_family_index(self.family).dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT).src_access_mask(vk::AccessFlags::TRANSFER_READ).dst_access_mask(vk::AccessFlags::empty())).collect();
    self.device.cmd_pipeline_barrier(command,vk::PipelineStageFlags::TRANSFER,vk::PipelineStageFlags::BOTTOM_OF_PIPE,vk::DependencyFlags::empty(),&[],&[],&release);
    self.device.end_command_buffer(command)?;let fence=self.device.create_fence(&vk::FenceCreateInfo::default(),None)?;
    self.device.queue_submit(self.queue,&[vk::SubmitInfo::default().command_buffers(&[command])],fence)?;
    if let Err(error)=self.device.wait_for_fences(&[fence],true,3_000_000_000){eprintln!("GPU test timeout/error {error:?}; no native resources will be recycled by this process");std::process::exit(3);}
    let data=self.device.map_memory(memory,0,size,vk::MemoryMapFlags::empty())?.cast::<u8>();
    let word=if info.pixel_format==8{1}else{2};let width=info.visible_width as usize;let height=info.visible_height as usize;
    let mut largest=0i32;
    for plane in 0..2 {let components=if plane==0{1}else{2};let pitch=width*word;let plane_offset=if plane==0{0}else{width*height*word};
      let divisor=if plane==0{1}else{2};let w=width/divisor;let h=height/divisor;
      for (sample,[x,y]) in [[4,4],[w/2-2,h/2-2],[w-8,h-8]].into_iter().enumerate(){
        for row in 0..4usize{for col in 0..4*components{let test=256*(plane*3+sample)+(row*4*components+col)*word;let expected=plane_offset+(y+row)*pitch+(x*components+col)*word;
          require(expected+word<=reference.len(),"software reference is too short")?;
          let (a,b)=if word==1{(*data.add(test) as i32,reference[expected] as i32)}else{(u16::from_le_bytes([*data.add(test),*data.add(test+1)]) as i32>>6,u16::from_le_bytes([reference[expected],reference[expected+1]]) as i32>>6)};
          largest=largest.max((a-b).abs());
        }}
      }
    }
    self.device.unmap_memory(memory);self.device.destroy_fence(fence,None);self.device.free_command_buffers(self.pool,&[command]);self.device.destroy_buffer(buffer,None);self.device.free_memory(memory,None);
    require(largest<=1,"native tiled Vulkan samples differ from decoded software reference")?;
    println!("NATIVE_TILED_GPU_SAMPLE_VERIFIED {{\"plane_count\":2,\"pixels_per_plane\":48,\"largest_code_difference\":{largest},\"modifier\":\"0x{:x}\",\"test_only_readback_bytes\":{},\"gui_presented\":false}}",info.objects[0].modifier,48*3*word);
    Ok(())
 }}
}
fn access_units(bytes:&[u8])->Result<Vec<&[u8]>,E>{
 let mut starts=Vec::new();let mut i=0;
 while i+5<=bytes.len(){let n=if bytes[i..].starts_with(&[0,0,0,1]){4}else if bytes[i..].starts_with(&[0,0,1]){3}else{i+=1;continue;};if (bytes[i+n]>>1)&63==35{starts.push(i);}i+=n+2;}
 require(starts.len()==60&&starts[0]==0,"test requires exactly 60 AUD-delimited generated access units")?;
 starts.push(bytes.len());Ok(starts.windows(2).map(|p|&bytes[p[0]..p[1]]).collect())
}
fn main()->Result<(),E>{
 require(std::env::var("RP_NATIVE_DRM_TEST").as_deref()==Ok("1"),"explicit local GPU validation flag required")?;
 require(unsafe{rp_linux_frame_info_size()}==std::mem::size_of::<FrameInfo>(),"native FFI frame metadata layout mismatch")?;
 let input=std::env::var("RP_NATIVE_DRM_FIXTURE")?;let reference=std::fs::read(std::env::var("RP_NATIVE_DRM_REFERENCE")?)?;let bytes=std::fs::read(&input)?;let units=access_units(&bytes)?;
 let node=CString::new("/dev/dri/renderD128")?;let mut raw=ptr::null_mut();ffcheck(unsafe{rp_linux_decoder_create(node.as_ptr(),&mut raw)})?;let decoder=Decoder(raw);
 let gpu=unsafe{Gpu::new(Path::new(node.to_str()?))?};let started=Instant::now();let mut total=0;
 let mut drain=||->Result<(),E>{loop{let mut raw=ptr::null_mut();let result=ffcheck(unsafe{rp_linux_decoder_receive(decoder.0,&mut raw)})?;if result==0{return Ok(());}
   let frame=Frame(raw);let info=unsafe{*rp_linux_frame_info(frame.0)};
   if total==0{require(info.color_matrix==1&&info.color_primaries==1&&info.color_transfer==1,"test source must explicitly declare BT.709")?;
     let planes=unsafe{[gpu.import(&info,0)?,gpu.import(&info,1)?]};unsafe{gpu.validate_samples(&planes,&info,&reference)?};
   }
   total+=1;
 }};
 for (i,unit) in units.iter().enumerate(){let mut status=unsafe{rp_linux_decoder_submit(decoder.0,unit.as_ptr(),unit.len(),i as i64*33)};if status==-libc::EAGAIN{drain()?;status=unsafe{rp_linux_decoder_submit(decoder.0,unit.as_ptr(),unit.len(),i as i64*33)};}ffcheck(status)?;drain()?;}
 ffcheck(unsafe{rp_linux_decoder_flush(decoder.0)})?;drain()?;drop(drain);
 require(total==60,"native fixture frame count mismatch")?;
 println!("NATIVE_VULKAN_FIXTURE_PASS {{\"decoded_direct_frames\":{total},\"sampled_frame_count\":1,\"elapsed_ms\":{},\"production_readback\":false,\"network\":false,\"screen_capture\":false}}",started.elapsed().as_millis());
 Ok(())
}
