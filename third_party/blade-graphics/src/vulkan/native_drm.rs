//! Narrow native-video interop for the existing Vulkan backend. No raw device
//! handle is exposed, and no ordinary Texture creation/destruction is changed.
use ash::vk;
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, BorrowedFd, IntoRawFd},
};

pub(super) const EXTENSIONS: &[&CStr] = &[
    vk::KHR_EXTERNAL_MEMORY_FD_NAME,
    vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME,
    vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME,
    vk::EXT_QUEUE_FAMILY_FOREIGN_NAME,
    vk::EXT_PHYSICAL_DEVICE_DRM_NAME,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneFormat {
    R8,
    Rg8,
    R16,
    Rg16,
}
impl PlaneFormat {
    fn vk(self) -> vk::Format {
        match self {
            Self::R8 => vk::Format::R8_UNORM,
            Self::Rg8 => vk::Format::R8G8_UNORM,
            Self::R16 => vk::Format::R16_UNORM,
            Self::Rg16 => vk::Format::R16G16_UNORM,
        }
    }
    fn bytes(self) -> u32 {
        match self {
            Self::R8 => 1,
            Self::Rg8 | Self::R16 => 2,
            Self::Rg16 => 4,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct DmaBufPlaneDesc<'a> {
    pub fd: BorrowedFd<'a>,
    pub allocation_size: u64,
    pub modifier: u64,
    pub offset: u64,
    pub pitch: u64,
    pub width: u32,
    pub height: u32,
    pub format: PlaneFormat,
    /// Actual exporting render node, not a vendor name or guessed GPU index.
    pub render_node: (i64, i64),
}
#[derive(Debug)]
pub struct ImportError(pub String);
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ImportError {}
impl From<vk::Result> for ImportError {
    fn from(e: vk::Result) -> Self {
        Self(format!("native DRM Vulkan error: {e:?}"))
    }
}
fn ensure(ok: bool, s: &str) -> Result<(), ImportError> {
    if ok {
        Ok(())
    } else {
        Err(ImportError(s.into()))
    }
}
/// An imported plane, never a copy. Destroy it only after submitted uses finish.
/// This follows Blade's resource-lifetime contract; native producers must also
/// retain their immutable frame leases until the corresponding SyncPoint.
pub struct DmaBufImage {
    device: ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: super::TextureView,
    family: u32,
}
impl std::fmt::Debug for DmaBufImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmaBufImage")
            .field("view", &self.view)
            .finish()
    }
}
impl DmaBufImage {
    pub fn view(&self) -> super::TextureView {
        self.view
    }
}
impl Drop for DmaBufImage {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_image_view(self.view.raw, None);
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}
fn validate(desc: &DmaBufPlaneDesc<'_>) -> Result<(), ImportError> {
    ensure(
        desc.width > 0 && desc.height > 0 && desc.width <= 16384 && desc.height <= 16384,
        "native plane dimensions outside supported bounds",
    )?;
    ensure(
        desc.allocation_size > 0
            && desc.offset < desc.allocation_size
            && desc.pitch >= u64::from(desc.width) * u64::from(desc.format.bytes()),
        "native plane size/pitch/offset invalid",
    )?;
    ensure(
        desc.modifier != u64::MAX && desc.render_node.0 >= 0 && desc.render_node.1 >= 0,
        "native modifier or GPU identity missing",
    )?;
    // Tiled size is driver-defined. Only linear layouts have this direct span rule.
    if desc.modifier == 0 {
        let end = u64::from(desc.height - 1)
            .checked_mul(desc.pitch)
            .and_then(|s| s.checked_add(desc.offset))
            .and_then(|s| s.checked_add(u64::from(desc.width) * u64::from(desc.format.bytes())));
        ensure(
            end.is_some_and(|end| end <= desc.allocation_size),
            "linear plane exceeds allocation",
        )?;
    }
    Ok(())
}
impl super::Context {
    pub fn native_drm_render_node(&self) -> Option<(i64, i64)> {
        if !self.device.native_drm {
            return None;
        }
        let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
        unsafe {
            self.instance
                .get_physical_device_properties2
                .get_physical_device_properties2(self.physical_device, &mut properties);
        }
        (drm.has_render != 0).then_some((drm.render_major, drm.render_minor))
    }
    /// Import validated explicit modifier metadata into this very renderer device.
    /// # Safety
    /// The FD must name the declared allocation/layout. The producer must finish
    /// its writes and release foreign ownership before acquisition. Keep the native
    /// frame alive through GPU completion; do not recycle/mutate it while sampled.
    /// The image must be dropped before Context and after its last SyncPoint.
    pub unsafe fn import_dma_buf_plane(
        &self,
        desc: &DmaBufPlaneDesc<'_>,
    ) -> Result<DmaBufImage, ImportError> {
        unsafe {
            validate(desc)?;
            ensure(
                self.native_drm_render_node() == Some(desc.render_node),
                "native-video render-node mismatch or missing interop capability",
            )?;
            let format = desc.format.vk();
            let ext = &self.instance.get_physical_device_properties2;
            let mut mods = vk::DrmFormatModifierPropertiesListEXT::default();
            ext.get_physical_device_format_properties2(
                self.physical_device,
                format,
                &mut vk::FormatProperties2::default().push_next(&mut mods),
            );
            ensure(
                mods.drm_format_modifier_count > 0 && mods.drm_format_modifier_count <= 4096,
                "invalid modifier capability count",
            )?;
            let mut list = vec![
                vk::DrmFormatModifierPropertiesEXT::default();
                mods.drm_format_modifier_count as usize
            ];
            mods.p_drm_format_modifier_properties = list.as_mut_ptr();
            ext.get_physical_device_format_properties2(
                self.physical_device,
                format,
                &mut vk::FormatProperties2::default().push_next(&mut mods),
            );
            let modifier = list
                .iter()
                .find(|m| m.drm_format_modifier == desc.modifier)
                .ok_or_else(|| {
                    ImportError("exported DRM modifier unsupported for plane format".into())
                })?;
            ensure(
                modifier.drm_format_modifier_plane_count == 1,
                "auxiliary modifier planes need an explicit supported adapter",
            )?;
            ensure(
                modifier.drm_format_modifier_tiling_features.contains(
                    vk::FormatFeatureFlags::SAMPLED_IMAGE
                        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
                ),
                "video plane cannot be sampled with the required filter",
            )?;
            let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let mut drm = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
                .drm_format_modifier(desc.modifier)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let query = vk::PhysicalDeviceImageFormatInfo2::default()
                .format(format)
                .ty(vk::ImageType::TYPE_2D)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(vk::ImageUsageFlags::SAMPLED)
                .push_next(&mut external)
                .push_next(&mut drm);
            let mut external_props = vk::ExternalImageFormatProperties::default();
            let mut result = vk::ImageFormatProperties2::default().push_next(&mut external_props);
            ext.get_physical_device_image_format_properties2(
                self.physical_device,
                &query,
                &mut result,
            )?;
            ensure(
                desc.width <= result.image_format_properties.max_extent.width
                    && desc.height <= result.image_format_properties.max_extent.height,
                "native plane extent exceeds capability",
            )?;
            ensure(
                external_props
                    .external_memory_properties
                    .external_memory_features
                    .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE),
                "native memory not importable",
            )?;
            let layouts = [vk::SubresourceLayout {
                offset: desc.offset,
                row_pitch: desc.pitch,
                ..Default::default()
            }];
            let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
                .drm_format_modifier(desc.modifier)
                .plane_layouts(&layouts);
            let mut handles = vk::ExternalMemoryImageCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let extent = vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            };
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(extent)
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut explicit)
                .push_next(&mut handles);
            let image = self.device.core.create_image(&info, None)?;
            let mut memory = vk::DeviceMemory::null();
            let setup = (|| -> Result<vk::ImageView, ImportError> {
                let req = self.device.core.get_image_memory_requirements(image);
                ensure(
                    req.size <= desc.allocation_size,
                    "native allocation smaller than driver image requirement",
                )?;
                let loader =
                    self.device.external_memory.as_ref().ok_or_else(|| {
                        ImportError("native FD import extension unavailable".into())
                    })?;
                let mut fd_props = vk::MemoryFdPropertiesKHR::default();
                loader.get_memory_fd_properties(
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    desc.fd.as_raw_fd(),
                    &mut fd_props,
                )?;
                let bits = req.memory_type_bits & fd_props.memory_type_bits;
                ensure(bits != 0, "native allocation has no compatible memory type")?;
                let fd = desc
                    .fd
                    .try_clone_to_owned()
                    .map_err(|e| ImportError(e.to_string()))?;
                let mut import = vk::ImportMemoryFdInfoKHR::default()
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                    .fd(fd.as_raw_fd());
                let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
                let allocate = vk::MemoryAllocateInfo::default()
                    .allocation_size(desc.allocation_size)
                    .memory_type_index(bits.trailing_zeros())
                    .push_next(&mut import)
                    .push_next(&mut dedicated);
                memory = self.device.core.allocate_memory(&allocate, None)?;
                let _ = fd.into_raw_fd(); // Successful import transfers this duplicated FD.
                self.device.core.bind_image_memory(image, memory, 0)?;
                Ok(self.device.core.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                )?)
            })();
            match setup {
                Ok(raw) => Ok(DmaBufImage {
                    device: self.device.core.clone(),
                    image,
                    memory,
                    view: super::TextureView {
                        raw,
                        target_size: [desc.width as u16, desc.height as u16],
                        aspects: crate::TexelAspects::COLOR,
                    },
                    family: self.queue_family_index,
                }),
                Err(e) => {
                    self.device.core.destroy_image(image, None);
                    if memory != vk::DeviceMemory::null() {
                        self.device.core.free_memory(memory, None);
                    }
                    Err(e)
                }
            }
        }
    }
}
impl super::CommandEncoder {
    /// # Safety
    /// Call outside a render pass on the owning Context's encoder. Retain image
    /// and its immutable producer lease until the submitted SyncPoint completes.
    /// Every acquire must have a matching release before submission. Never call
    /// init_texture on imported content (it would discard the image contents).
    pub unsafe fn acquire_dma_buf(&mut self, image: &DmaBufImage) {
        unsafe {
            assert_eq!(
                self.device.core.handle(),
                image.device.handle(),
                "native texture belongs to another renderer"
            );
            let barrier = vk::ImageMemoryBarrier::default()
                .image(image.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(image.family)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_READ);
            self.device.core.cmd_pipeline_barrier(
                self.buffers[0].raw,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }
    }
    /// # Safety
    /// Same lifetime/encoder conditions as acquire_dma_buf. Native writer reuse
    /// is permitted only after completion, not after merely recording release.
    pub unsafe fn release_dma_buf(&mut self, image: &DmaBufImage) {
        unsafe {
            assert_eq!(self.device.core.handle(), image.device.handle());
            let barrier = vk::ImageMemoryBarrier::default()
                .image(image.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(image.family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .dst_access_mask(vk::AccessFlags::empty());
            self.device.core.cmd_pipeline_barrier(
                self.buffers[0].raw,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_rejects_ambiguous_or_out_of_range_layouts() {
        let file = std::fs::File::open("/dev/null").unwrap();
        use std::os::fd::AsFd;
        let good = DmaBufPlaneDesc {
            fd: file.as_fd(),
            allocation_size: 4096,
            modifier: 0,
            offset: 0,
            pitch: 64,
            width: 64,
            height: 64,
            format: PlaneFormat::R8,
            render_node: (226, 128),
        };
        assert!(validate(&good).is_ok());
        assert!(validate(&DmaBufPlaneDesc {
            offset: u64::MAX,
            ..good
        })
        .is_err());
        assert!(validate(&DmaBufPlaneDesc { pitch: 63, ..good }).is_err());
        assert!(validate(&DmaBufPlaneDesc {
            allocation_size: 4095,
            ..good
        })
        .is_err());
        assert!(validate(&DmaBufPlaneDesc {
            modifier: u64::MAX,
            ..good
        })
        .is_err());
    }
}
