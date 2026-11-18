//! The quad renderer: draws `termmd_core::render::quads` output into
//! dma-buf images with one instanced pipeline (dynamic rendering, no
//! render passes) and a single-channel glyph atlas.
//!
//! Frames rotate through a few slots, each with its own command buffer,
//! fence and host-visible instance and staging buffers, so the CPU can
//! build the next frame while the GPU draws the last one.
//!
//! Textures mirror `termmd_core::render::resources`: [`QuadRenderer::apply`]
//! turns its events into images, each with its own descriptor set.
//! Destroyed textures wait in a graveyard until no frame in flight can
//! still sample them. Embedded surfaces are imported dma-bufs
//! ([`QuadRenderer::import`]) under ids the table hands out for them.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;

use termmd_core::render::quads::{Quad, Upload};
use termmd_core::render::resources::{ResourceEvent, TextureDesc, TextureFormat, TextureId};
use termmd_core::term::Rgb;

use super::dmabuf::{DRM_FORMAT_XRGB8888, DmabufImage, allocate};
use super::ffi::*;
use super::{Device, Result, check};

const VERT_SPV: &[u8] = include_bytes!("shaders/quad.vert.spv");
const FRAG_SPV: &[u8] = include_bytes!("shaders/quad.frag.spv");

/// Frames that may be in flight at once.
const SLOTS: usize = 2;
const VK_FENCE_CREATE_SIGNALED_BIT: VkFlags = 0x1;
const VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT: VkFlags = 0x1;
/// Textures (descriptor sets) alive at once.
const MAX_TEXTURES: u32 = 1024;

const COLOR_RANGE: VkImageSubresourceRange = VkImageSubresourceRange {
    aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
    baseMipLevel: 0,
    levelCount: 1,
    baseArrayLayer: 0,
    layerCount: 1,
};

/// A host-visible, coherent buffer kept mapped for its whole life.
#[derive(Debug)]
struct HostBuffer {
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    ptr: *mut u8,
    size: u64,
    device: Rc<Device>,
}

impl HostBuffer {
    fn new(device: &Rc<Device>, size: u64, usage: VkFlags) -> Result<HostBuffer> {
        let f = &device.fns;
        let info = VkBufferCreateInfo {
            sType: VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            size,
            usage,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: ptr::null(),
        };
        let mut b = HostBuffer {
            buffer: VK_NULL_HANDLE,
            memory: VK_NULL_HANDLE,
            ptr: ptr::null_mut(),
            size,
            device: device.clone(),
        };
        // SAFETY: valid device and create info.
        check("vkCreateBuffer", unsafe { (f.vkCreateBuffer)(device.raw, &info, ptr::null(), &mut b.buffer) })?;
        let mut req = VkMemoryRequirements::default();
        // SAFETY: a buffer of ours.
        unsafe { (f.vkGetBufferMemoryRequirements)(device.raw, b.buffer, &mut req) };
        let flags = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        let index = device.memory_type(req.memoryTypeBits, flags).ok_or(super::Error::Call("host memory type", -2))?;
        let alloc = VkMemoryAllocateInfo {
            sType: VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            pNext: ptr::null(),
            allocationSize: req.size,
            memoryTypeIndex: index,
        };
        // SAFETY: valid allocate info; `b` frees the memory on drop.
        check("vkAllocateMemory", unsafe { (f.vkAllocateMemory)(device.raw, &alloc, ptr::null(), &mut b.memory) })?;
        // SAFETY: fresh memory sized for the buffer.
        check("vkBindBufferMemory", unsafe { (f.vkBindBufferMemory)(device.raw, b.buffer, b.memory, 0) })?;
        let mut mapped = ptr::null_mut();
        // SAFETY: host-visible memory, mapped once for the buffer's life.
        check("vkMapMemory", unsafe { (f.vkMapMemory)(device.raw, b.memory, 0, VK_WHOLE_SIZE, 0, &mut mapped) })?;
        b.ptr = mapped.cast();
        Ok(b)
    }

    /// A buffer of at least `need` bytes: `slot`'s if big enough, else a new one.
    fn ensure(slot: &mut Option<HostBuffer>, device: &Rc<Device>, need: u64, usage: VkFlags) -> Result<()> {
        if slot.as_ref().is_none_or(|b| b.size < need) {
            *slot = None; // the GPU is done with it: its slot's fence was waited on
            *slot = Some(HostBuffer::new(device, need.next_power_of_two().max(64 << 10), usage)?);
        }
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) {
        assert!(offset + data.len() <= self.size as usize);
        // SAFETY: in bounds (checked) of a mapping no GPU work is using.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(offset), data.len()) };
    }
}

impl Drop for HostBuffer {
    fn drop(&mut self) {
        let f = &self.device.fns;
        // SAFETY: no submitted work uses the buffer (callers wait on its
        // slot's fence); freeing the memory also unmaps it.
        unsafe {
            (f.vkDestroyBuffer)(self.device.raw, self.buffer, ptr::null());
            (f.vkFreeMemory)(self.device.raw, self.memory, ptr::null());
        }
    }
}

/// A dma-buf image the renderer can draw into.
#[derive(Debug)]
pub struct RenderTarget {
    view: VkImageView,
    image: VkImage,
    pub width: u32,
    pub height: u32,
    device: Rc<Device>,
}

impl RenderTarget {
    /// `img` must have been created with colour-attachment usage.
    pub fn new(device: &Rc<Device>, img: &DmabufImage) -> Result<RenderTarget> {
        let view = create_view(device, img.image, img.buf.format.vk)?;
        Ok(RenderTarget {
            view,
            image: img.image,
            width: img.buf.width,
            height: img.buf.height,
            device: device.clone(),
        })
    }
}

impl Drop for RenderTarget {
    fn drop(&mut self) {
        // SAFETY: callers wait for frames drawing into the target first.
        unsafe { (self.device.fns.vkDestroyImageView)(self.device.raw, self.view, ptr::null()) };
    }
}

fn create_view(device: &Device, image: VkImage, format: i32) -> Result<VkImageView> {
    create_view_mapped(device, image, format, VkComponentMapping::default())
}

fn create_view_mapped(
    device: &Device,
    image: VkImage,
    format: i32,
    components: VkComponentMapping,
) -> Result<VkImageView> {
    let info = VkImageViewCreateInfo {
        sType: VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
        pNext: ptr::null(),
        flags: 0,
        image,
        viewType: VK_IMAGE_VIEW_TYPE_2D,
        format,
        components,
        subresourceRange: COLOR_RANGE,
    };
    let mut view = VK_NULL_HANDLE;
    // SAFETY: a valid image of ours and create info.
    check("vkCreateImageView", unsafe { (device.fns.vkCreateImageView)(device.raw, &info, ptr::null(), &mut view) })?;
    Ok(view)
}

#[derive(Debug)]
struct Slot {
    cmd: VkCommandBuffer,
    fence: VkFence,
    /// The submission last made from this slot.
    serial: u64,
    /// Its fence has been waited on since.
    done: bool,
    instances: Option<HostBuffer>,
    staging: Option<HostBuffer>,
}

/// A sampled image with its view and descriptor set.
#[derive(Debug)]
struct GpuTexture {
    image: VkImage,
    memory: VkDeviceMemory,
    view: VkImageView,
    set: VkDescriptorSet,
    pool: VkDescriptorPool,
    /// Out of `UNDEFINED`: copies must preserve what is there.
    initialised: bool,
    /// An imported dma-buf (owning `image` and `memory`), sampled in
    /// `GENERAL` layout and acquired from its owner each frame.
    external: Option<DmabufImage>,
    device: Rc<Device>,
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        let (d, f) = (self.device.raw, &self.device.fns);
        // SAFETY: no frame in flight uses the texture (the graveyard waits
        // for that); null handles are ignored.
        unsafe {
            if self.set != VK_NULL_HANDLE {
                (f.vkFreeDescriptorSets)(d, self.pool, 1, &self.set);
            }
            (f.vkDestroyImageView)(d, self.view, ptr::null());
            if self.external.is_none() {
                (f.vkDestroyImage)(d, self.image, ptr::null());
                (f.vkFreeMemory)(d, self.memory, ptr::null());
            }
        }
    }
}

/// Pipeline, atlas and per-frame resources. Every handle starts null and
/// `Drop` destroys whatever was created, so `new` can bail out anywhere.
#[derive(Debug)]
pub struct QuadRenderer {
    set_layout: VkDescriptorSetLayout,
    descriptor_pool: VkDescriptorPool,
    pipeline_layout: VkPipelineLayout,
    pipeline: VkPipeline,
    sampler: VkSampler,
    textures: HashMap<TextureId, GpuTexture>,
    /// Bound for draws that sample nothing.
    blank: Option<GpuTexture>,
    /// Destroyed textures and the last submission that may use them.
    graveyard: Vec<(u64, GpuTexture)>,
    submitted: u64,
    cmd_pool: VkCommandPool,
    slots: Vec<Slot>,
    next: usize,
    scratch: Vec<u8>,
    device: Rc<Device>,
}

impl QuadRenderer {
    /// A renderer drawing into images of Vulkan format `format`.
    pub fn new(device: &Rc<Device>, format: i32) -> Result<QuadRenderer> {
        let mut r = QuadRenderer {
            set_layout: VK_NULL_HANDLE,
            descriptor_pool: VK_NULL_HANDLE,
            pipeline_layout: VK_NULL_HANDLE,
            pipeline: VK_NULL_HANDLE,
            sampler: VK_NULL_HANDLE,
            textures: HashMap::new(),
            blank: None,
            graveyard: Vec::new(),
            submitted: 0,
            cmd_pool: VK_NULL_HANDLE,
            slots: Vec::new(),
            next: 0,
            scratch: Vec::new(),
            device: device.clone(),
        };
        r.create_descriptors()?;
        r.create_pipeline(format)?;
        r.create_slots()?;
        let blank = r.create_texture(TextureDesc { width: 1, height: 1, format: TextureFormat::R8 })?;
        r.clear_texture(&blank)?;
        r.blank = Some(blank);
        Ok(r)
    }

    fn create_descriptors(&mut self) -> Result<()> {
        let (d, f) = (self.device.raw, &self.device.fns);
        let sampler_info = VkSamplerCreateInfo {
            sType: VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            magFilter: VK_FILTER_NEAREST,
            minFilter: VK_FILTER_NEAREST,
            mipmapMode: VK_SAMPLER_MIPMAP_MODE_NEAREST,
            addressModeU: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeV: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            addressModeW: VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
            mipLodBias: 0.0,
            anisotropyEnable: 0,
            maxAnisotropy: 1.0,
            compareEnable: 0,
            compareOp: 0,
            minLod: 0.0,
            maxLod: 0.0,
            borderColor: 0,
            unnormalizedCoordinates: 0,
        };
        // SAFETY: valid device and create info; Drop destroys the sampler.
        check("vkCreateSampler", unsafe { (f.vkCreateSampler)(d, &sampler_info, ptr::null(), &mut self.sampler) })?;
        let binding = VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptorCount: 1,
            stageFlags: VK_SHADER_STAGE_FRAGMENT_BIT,
            pImmutableSamplers: &self.sampler,
        };
        let layout_info = VkDescriptorSetLayoutCreateInfo {
            sType: VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            bindingCount: 1,
            pBindings: &binding,
        };
        // SAFETY: see above.
        check("vkCreateDescriptorSetLayout", unsafe {
            (f.vkCreateDescriptorSetLayout)(d, &layout_info, ptr::null(), &mut self.set_layout)
        })?;
        let size =
            VkDescriptorPoolSize { type_: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, descriptorCount: MAX_TEXTURES };
        let pool_info = VkDescriptorPoolCreateInfo {
            sType: VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
            pNext: ptr::null(),
            flags: VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT,
            maxSets: MAX_TEXTURES,
            poolSizeCount: 1,
            pPoolSizes: &size,
        };
        // SAFETY: see above.
        check("vkCreateDescriptorPool", unsafe {
            (f.vkCreateDescriptorPool)(d, &pool_info, ptr::null(), &mut self.descriptor_pool)
        })
    }

    fn create_pipeline(&mut self, format: i32) -> Result<()> {
        let (d, f) = (self.device.raw, &self.device.fns);
        let push = VkPushConstantRange { stageFlags: VK_SHADER_STAGE_VERTEX_BIT, offset: 0, size: 8 };
        let layout_info = VkPipelineLayoutCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            setLayoutCount: 1,
            pSetLayouts: &self.set_layout,
            pushConstantRangeCount: 1,
            pPushConstantRanges: &push,
        };
        // SAFETY: valid device and create info.
        check("vkCreatePipelineLayout", unsafe {
            (f.vkCreatePipelineLayout)(d, &layout_info, ptr::null(), &mut self.pipeline_layout)
        })?;
        let vert = ShaderModule::new(&self.device, VERT_SPV)?;
        let frag = ShaderModule::new(&self.device, FRAG_SPV)?;
        let stage = |stage, module| VkPipelineShaderStageCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            stage,
            module,
            pName: c"main".as_ptr(),
            pSpecializationInfo: ptr::null(),
        };
        let stages = [stage(VK_SHADER_STAGE_VERTEX_BIT, vert.0), stage(VK_SHADER_STAGE_FRAGMENT_BIT, frag.0)];
        let binding = VkVertexInputBindingDescription {
            binding: 0,
            stride: Quad::SIZE as u32,
            inputRate: VK_VERTEX_INPUT_RATE_INSTANCE,
        };
        let attr =
            |location, format, offset| VkVertexInputAttributeDescription { location, binding: 0, format, offset };
        let attrs = [
            attr(0, VK_FORMAT_R16G16_UINT, 0),
            attr(1, VK_FORMAT_R16G16_UINT, 4),
            attr(2, VK_FORMAT_R16G16_UINT, 8),
            attr(3, VK_FORMAT_R8G8B8A8_UNORM, 12),
            attr(4, VK_FORMAT_R32_UINT, 16),
        ];
        let vertex_input = VkPipelineVertexInputStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            vertexBindingDescriptionCount: 1,
            pVertexBindingDescriptions: &binding,
            vertexAttributeDescriptionCount: attrs.len() as u32,
            pVertexAttributeDescriptions: attrs.as_ptr(),
        };
        let input_assembly = VkPipelineInputAssemblyStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            topology: VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,
            primitiveRestartEnable: 0,
        };
        let viewport = VkPipelineViewportStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            viewportCount: 1,
            pViewports: ptr::null(),
            scissorCount: 1,
            pScissors: ptr::null(),
        };
        let raster = VkPipelineRasterizationStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            depthClampEnable: 0,
            rasterizerDiscardEnable: 0,
            polygonMode: VK_POLYGON_MODE_FILL,
            cullMode: VK_CULL_MODE_NONE,
            frontFace: VK_FRONT_FACE_COUNTER_CLOCKWISE,
            depthBiasEnable: 0,
            depthBiasConstantFactor: 0.0,
            depthBiasClamp: 0.0,
            depthBiasSlopeFactor: 0.0,
            lineWidth: 1.0,
        };
        let multisample = VkPipelineMultisampleStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            rasterizationSamples: VK_SAMPLE_COUNT_1_BIT,
            sampleShadingEnable: 0,
            minSampleShading: 0.0,
            pSampleMask: ptr::null(),
            alphaToCoverageEnable: 0,
            alphaToOneEnable: 0,
        };
        // Premultiplied source over.
        let blend_attachment = VkPipelineColorBlendAttachmentState {
            blendEnable: 1,
            srcColorBlendFactor: VK_BLEND_FACTOR_ONE,
            dstColorBlendFactor: VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            colorBlendOp: VK_BLEND_OP_ADD,
            srcAlphaBlendFactor: VK_BLEND_FACTOR_ONE,
            dstAlphaBlendFactor: VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alphaBlendOp: VK_BLEND_OP_ADD,
            colorWriteMask: VK_COLOR_COMPONENT_RGBA,
        };
        let blend = VkPipelineColorBlendStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            logicOpEnable: 0,
            logicOp: 0,
            attachmentCount: 1,
            pAttachments: &blend_attachment,
            blendConstants: [0.0; 4],
        };
        let dynamic_states = [VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = VkPipelineDynamicStateCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            dynamicStateCount: dynamic_states.len() as u32,
            pDynamicStates: dynamic_states.as_ptr(),
        };
        let rendering = VkPipelineRenderingCreateInfo {
            sType: VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
            pNext: ptr::null(),
            viewMask: 0,
            colorAttachmentCount: 1,
            pColorAttachmentFormats: &format,
            depthAttachmentFormat: 0,
            stencilAttachmentFormat: 0,
        };
        let info = VkGraphicsPipelineCreateInfo {
            sType: VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
            pNext: (&rendering as *const VkPipelineRenderingCreateInfo).cast(),
            flags: 0,
            stageCount: stages.len() as u32,
            pStages: stages.as_ptr(),
            pVertexInputState: &vertex_input,
            pInputAssemblyState: &input_assembly,
            pTessellationState: ptr::null(),
            pViewportState: &viewport,
            pRasterizationState: &raster,
            pMultisampleState: &multisample,
            pDepthStencilState: ptr::null(),
            pColorBlendState: &blend,
            pDynamicState: &dynamic,
            layout: self.pipeline_layout,
            renderPass: VK_NULL_HANDLE,
            subpass: 0,
            basePipelineHandle: VK_NULL_HANDLE,
            basePipelineIndex: -1,
        };
        // SAFETY: every state struct above outlives the call; no cache.
        check("vkCreateGraphicsPipelines", unsafe {
            (f.vkCreateGraphicsPipelines)(d, VK_NULL_HANDLE, 1, &info, ptr::null(), &mut self.pipeline)
        })
    }

    /// An image for `desc` with its view and descriptor set, still `UNDEFINED`.
    fn create_texture(&self, desc: TextureDesc) -> Result<GpuTexture> {
        let device = &self.device;
        let format = match desc.format {
            TextureFormat::R8 => VK_FORMAT_R8_UNORM,
            TextureFormat::Rgba8 => VK_FORMAT_R8G8B8A8_UNORM,
        };
        let mut t = GpuTexture {
            image: VK_NULL_HANDLE,
            memory: VK_NULL_HANDLE,
            view: VK_NULL_HANDLE,
            set: VK_NULL_HANDLE,
            pool: self.descriptor_pool,
            initialised: false,
            external: None,
            device: device.clone(),
        };
        let info = VkImageCreateInfo {
            sType: VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            imageType: VK_IMAGE_TYPE_2D,
            format,
            extent: VkExtent3D { width: desc.width, height: desc.height, depth: 1 },
            mipLevels: 1,
            arrayLayers: 1,
            samples: VK_SAMPLE_COUNT_1_BIT,
            tiling: VK_IMAGE_TILING_OPTIMAL,
            usage: VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT,
            sharingMode: VK_SHARING_MODE_EXCLUSIVE,
            queueFamilyIndexCount: 0,
            pQueueFamilyIndices: ptr::null(),
            initialLayout: VK_IMAGE_LAYOUT_UNDEFINED,
        };
        // SAFETY: valid device and create info; `t` frees what is made.
        check("vkCreateImage", unsafe { (device.fns.vkCreateImage)(device.raw, &info, ptr::null(), &mut t.image) })?;
        t.memory = allocate(device, t.image, !0, ptr::null())?;
        t.view = create_view(device, t.image, format)?;
        self.bind_set(&mut t, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)?;
        Ok(t)
    }

    /// Sample dma-buf `image` as texture `id` (an id from
    /// [`Resources::external`](termmd_core::render::resources::Resources::external)).
    /// Its contents are premultiplied; `XR24` alpha reads as opaque.
    pub fn import(&mut self, id: TextureId, image: DmabufImage) -> Result<()> {
        let mut t = GpuTexture {
            image: image.image,
            memory: VK_NULL_HANDLE,
            view: VK_NULL_HANDLE,
            set: VK_NULL_HANDLE,
            pool: self.descriptor_pool,
            initialised: true,
            external: None,
            device: self.device.clone(),
        };
        let opaque = image.buf.format.drm == DRM_FORMAT_XRGB8888;
        let components =
            VkComponentMapping { a: if opaque { VK_COMPONENT_SWIZZLE_ONE } else { 0 }, ..Default::default() };
        t.external = Some(image);
        t.view =
            create_view_mapped(&self.device, t.image, t.external.as_ref().expect("set").buf.format.vk, components)?;
        self.bind_set(&mut t, VK_IMAGE_LAYOUT_GENERAL)?;
        if let Some(old) = self.textures.insert(id, t) {
            self.graveyard.push((self.submitted, old));
        }
        Ok(())
    }

    /// Give `t` a descriptor set sampling its view in `layout`.
    fn bind_set(&self, t: &mut GpuTexture, layout: i32) -> Result<()> {
        let device = &self.device;
        let alloc = VkDescriptorSetAllocateInfo {
            sType: VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
            pNext: ptr::null(),
            descriptorPool: self.descriptor_pool,
            descriptorSetCount: 1,
            pSetLayouts: &self.set_layout,
        };
        // SAFETY: one set from our pool (which allows freeing sets).
        check("vkAllocateDescriptorSets", unsafe {
            (device.fns.vkAllocateDescriptorSets)(device.raw, &alloc, &mut t.set)
        })?;
        let image_info = VkDescriptorImageInfo { sampler: VK_NULL_HANDLE, imageView: t.view, imageLayout: layout };
        let write = VkWriteDescriptorSet {
            sType: VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            pNext: ptr::null(),
            dstSet: t.set,
            dstBinding: 0,
            dstArrayElement: 0,
            descriptorCount: 1,
            descriptorType: VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            pImageInfo: &image_info,
            pBufferInfo: ptr::null(),
            pTexelBufferView: ptr::null(),
        };
        // SAFETY: a fresh set no command buffer uses yet.
        unsafe { (device.fns.vkUpdateDescriptorSets)(device.raw, 1, &write, 0, ptr::null()) };
        Ok(())
    }

    /// Zero `t` and leave it ready to sample (setup work, waited on).
    fn clear_texture(&self, t: &GpuTexture) -> Result<()> {
        let image = t.image;
        self.device.submit_once(|f, cmd| {
            let to_dst = image_barrier(
                image,
                (VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL),
                (VK_PIPELINE_STAGE_2_NONE, VK_ACCESS_2_NONE),
                (VK_PIPELINE_STAGE_2_CLEAR_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT),
            );
            let to_read = image_barrier(
                image,
                (VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL),
                (VK_PIPELINE_STAGE_2_CLEAR_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT),
                (VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT, VK_ACCESS_2_SHADER_SAMPLED_READ_BIT),
            );
            let zero: VkClearColorValue = [0.0; 4];
            // SAFETY: recording; the structs outlive the calls.
            unsafe {
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(&to_dst));
                (f.vkCmdClearColorImage)(cmd, image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &zero, 1, &COLOR_RANGE);
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(&to_read));
            }
        })
    }

    /// Mirror resource events: create textures, and retire destroyed ones
    /// once the frames that may sample them have finished.
    pub fn apply(&mut self, events: &[ResourceEvent]) -> Result<()> {
        for ev in events {
            match *ev {
                ResourceEvent::Created(id, desc) => {
                    let t = self.create_texture(desc)?;
                    self.textures.insert(id, t);
                }
                ResourceEvent::Destroyed(id) => {
                    if let Some(t) = self.textures.remove(&id) {
                        self.graveyard.push((self.submitted, t));
                    }
                }
            }
        }
        self.bury();
        Ok(())
    }

    /// Free retired textures no frame in flight can use: every slot has
    /// either finished (fence waited) or been resubmitted since.
    fn bury(&mut self) {
        let slots = &self.slots;
        self.graveyard.retain(|(last, _)| !slots.iter().all(|s| s.done || s.serial > *last));
    }

    /// The last submission known to have finished (fences checked without
    /// waiting): resources it used may be handed back.
    pub fn completed(&mut self) -> u64 {
        let (d, f) = (self.device.raw, &self.device.fns);
        for s in self.slots.iter_mut().filter(|s| !s.done) {
            // SAFETY: our fence.
            s.done = unsafe { (f.vkGetFenceStatus)(d, s.fence) } == VK_SUCCESS;
        }
        self.bury();
        let pending = self.slots.iter().filter(|s| !s.done).map(|s| s.serial).min();
        pending.map_or(self.submitted, |p| p - 1)
    }

    /// Submissions made so far (the serial of the latest).
    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Textures alive (excluding retired ones).
    pub fn textures(&self) -> usize {
        self.textures.len()
    }

    /// Retired textures not yet freed.
    pub fn retired(&self) -> usize {
        self.graveyard.len()
    }

    fn create_slots(&mut self) -> Result<()> {
        let (d, f) = (self.device.raw, &self.device.fns);
        let pool_info = VkCommandPoolCreateInfo {
            sType: VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            pNext: ptr::null(),
            flags: VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
            queueFamilyIndex: self.device.info.graphics_family.expect("opened devices have one"),
        };
        // SAFETY: valid device and create info.
        check("vkCreateCommandPool", unsafe {
            (f.vkCreateCommandPool)(d, &pool_info, ptr::null(), &mut self.cmd_pool)
        })?;
        let alloc = VkCommandBufferAllocateInfo {
            sType: VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
            pNext: ptr::null(),
            commandPool: self.cmd_pool,
            level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
            commandBufferCount: SLOTS as u32,
        };
        let mut cmds = [ptr::null_mut(); SLOTS];
        // SAFETY: SLOTS buffers from our pool into `cmds`.
        check("vkAllocateCommandBuffers", unsafe { (f.vkAllocateCommandBuffers)(d, &alloc, cmds.as_mut_ptr()) })?;
        for cmd in cmds {
            // Signalled, so the first wait on each slot returns at once.
            let info = VkFenceCreateInfo {
                sType: VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
                pNext: ptr::null(),
                flags: VK_FENCE_CREATE_SIGNALED_BIT,
            };
            let mut fence = VK_NULL_HANDLE;
            // SAFETY: valid device and create info.
            check("vkCreateFence", unsafe { (f.vkCreateFence)(d, &info, ptr::null(), &mut fence) })?;
            self.slots.push(Slot { cmd, fence, serial: 0, done: true, instances: None, staging: None });
        }
        Ok(())
    }

    /// Wait until every submitted frame has finished.
    pub fn wait(&mut self) -> Result<()> {
        let fences: Vec<VkFence> = self.slots.iter().map(|s| s.fence).collect();
        // SAFETY: our fences; all have been submitted or created signalled.
        check("vkWaitForFences", unsafe {
            (self.device.fns.vkWaitForFences)(self.device.raw, fences.len() as u32, fences.as_ptr(), 1, u64::MAX)
        })?;
        self.slots.iter_mut().for_each(|s| s.done = true);
        self.bury();
        Ok(())
    }

    /// Draw `quads` over `clear` into `target`, after copying `uploads`
    /// into their textures (created by [`apply`](Self::apply) beforehand),
    /// then hand the image to the compositor. Signals `signal` (a binary
    /// semaphore) when the GPU is done, if given.
    pub fn render(
        &mut self,
        target: &RenderTarget,
        clear: Rgb,
        quads: &[Quad],
        uploads: &[Upload],
        signal: Option<VkSemaphore>,
    ) -> Result<()> {
        let device = self.device.clone();
        let (d, f) = (device.raw, &device.fns);
        let index = self.next;
        self.next = (self.next + 1) % SLOTS;
        // SAFETY: our fence; then reset once it has signalled.
        unsafe {
            check("vkWaitForFences", (f.vkWaitForFences)(d, 1, &self.slots[index].fence, 1, u64::MAX))?;
        }
        self.slots[index].done = true;
        self.bury();
        let slot = &mut self.slots[index];

        // Instances and upload bytes into this slot's buffers.
        self.scratch.clear();
        for q in quads {
            q.write(&mut self.scratch);
        }
        HostBuffer::ensure(&mut slot.instances, &device, self.scratch.len() as u64, VK_BUFFER_USAGE_VERTEX_BUFFER_BIT)?;
        let instances = slot.instances.as_mut().expect("ensured");
        instances.write(0, &self.scratch);
        // Copies grouped by texture, each group's regions contiguous.
        let mut order: Vec<&Upload> = uploads.iter().filter(|u| self.textures.contains_key(&u.texture)).collect();
        order.sort_by_key(|u| u.texture);
        let uploads = order;
        let mut copies = Vec::with_capacity(uploads.len());
        let mut offset = 0usize;
        for u in &uploads {
            copies.push(VkBufferImageCopy {
                bufferOffset: offset as u64,
                bufferRowLength: 0,
                bufferImageHeight: 0,
                imageSubresource: VkImageSubresourceLayers {
                    aspectMask: VK_IMAGE_ASPECT_COLOR_BIT,
                    mipLevel: 0,
                    baseArrayLayer: 0,
                    layerCount: 1,
                },
                imageOffset: VkOffset3D { x: u.x as i32, y: u.y as i32, z: 0 },
                imageExtent: VkExtent3D { width: u.width, height: u.height, depth: 1 },
            });
            offset += u.data.len().next_multiple_of(4);
        }
        if !uploads.is_empty() {
            HostBuffer::ensure(&mut slot.staging, &device, offset as u64, VK_BUFFER_USAGE_TRANSFER_SRC_BIT)?;
            let staging = slot.staging.as_mut().expect("ensured");
            for (u, c) in uploads.iter().zip(&copies) {
                staging.write(c.bufferOffset as usize, &u.data);
            }
        }

        let cmd = slot.cmd;
        let begin = VkCommandBufferBeginInfo {
            sType: VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
            pNext: ptr::null(),
            flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
            pInheritanceInfo: ptr::null(),
        };
        // SAFETY: the slot's previous submission has completed (fence).
        unsafe {
            check("vkResetCommandBuffer", (f.vkResetCommandBuffer)(cmd, 0))?;
            check("vkBeginCommandBuffer", (f.vkBeginCommandBuffer)(cmd, &begin))?;
        }
        let family = device.info.graphics_family.expect("opened devices have one");
        // Per uploaded texture: its copy range and barriers around it.
        let mut groups: Vec<(VkImage, std::ops::Range<usize>, VkImageMemoryBarrier2, VkImageMemoryBarrier2)> =
            Vec::new();
        let mut start = 0;
        while start < uploads.len() {
            let id = uploads[start].texture;
            let end = start + uploads[start..].iter().take_while(|u| u.texture == id).count();
            let tex = self.textures.get_mut(&id).expect("filtered above");
            // A first use discards the (undefined) contents; later ones wait
            // for earlier frames' sampling.
            let before = if tex.initialised {
                (VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT)
            } else {
                (VK_IMAGE_LAYOUT_UNDEFINED, VK_PIPELINE_STAGE_2_NONE)
            };
            tex.initialised = true;
            let to_dst = image_barrier(
                tex.image,
                (before.0, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL),
                (before.1, VK_ACCESS_2_NONE),
                (VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT),
            );
            let to_read = image_barrier(
                tex.image,
                (VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL),
                (VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT),
                (VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT, VK_ACCESS_2_SHADER_SAMPLED_READ_BIT),
            );
            groups.push((tex.image, start..end, to_dst, to_read));
            start = end;
        }
        // Draw runs: consecutive quads sampling at most one texture.
        let blank_set = self.blank.as_ref().expect("made in new").set;
        let set_of = |t: Option<TextureId>| t.and_then(|t| self.textures.get(&t)).map_or(blank_set, |t| t.set);
        let mut runs: Vec<(VkDescriptorSet, u32, u32)> = Vec::new();
        let mut run_tex: Option<TextureId> = None;
        let mut run_start = 0;
        for (i, q) in quads.iter().enumerate() {
            if let Some(t) = q.texture() {
                if run_tex.is_some_and(|r| r != t) {
                    runs.push((set_of(run_tex), run_start as u32, (i - run_start) as u32));
                    run_start = i;
                }
                run_tex = Some(t);
            }
        }
        if run_start < quads.len() {
            runs.push((set_of(run_tex), run_start as u32, (quads.len() - run_start) as u32));
        }
        // Imported surfaces sampled this frame: taken from their owner (whose
        // writes the client finished before presenting) and handed back.
        let mut external: Vec<VkImage> = Vec::new();
        for t in quads.iter().filter_map(|q| q.texture()) {
            if let Some(tex) = self.textures.get(&t).filter(|t| t.external.is_some())
                && !external.contains(&tex.image)
            {
                external.push(tex.image);
            }
        }
        let foreign = |image: VkImage, acquire: bool| {
            let shader = (VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT, VK_ACCESS_2_SHADER_SAMPLED_READ_BIT);
            let none = (VK_PIPELINE_STAGE_2_NONE, VK_ACCESS_2_NONE);
            let layouts = (VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_GENERAL);
            let mut b = if acquire {
                image_barrier(image, layouts, none, shader)
            } else {
                image_barrier(image, layouts, shader, none)
            };
            (b.srcQueueFamilyIndex, b.dstQueueFamilyIndex) =
                if acquire { (VK_QUEUE_FAMILY_FOREIGN_EXT, family) } else { (family, VK_QUEUE_FAMILY_FOREIGN_EXT) };
            b
        };
        // The previous contents are discarded, so no ownership acquire is
        // needed; waiting on earlier colour writes orders frames.
        let to_color = image_barrier(
            target.image,
            (VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL),
            (VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT, VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT),
            (VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT, VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT),
        );
        let mut release = image_barrier(
            target.image,
            (VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_GENERAL),
            (VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT, VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT),
            (VK_PIPELINE_STAGE_2_NONE, VK_ACCESS_2_NONE),
        );
        release.srcQueueFamilyIndex = family;
        release.dstQueueFamilyIndex = VK_QUEUE_FAMILY_FOREIGN_EXT;
        let unorm = |c: u8| c as f32 / 255.0;
        let color = VkRenderingAttachmentInfo {
            sType: VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
            pNext: ptr::null(),
            imageView: target.view,
            imageLayout: VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
            resolveMode: VK_RESOLVE_MODE_NONE,
            resolveImageView: VK_NULL_HANDLE,
            resolveImageLayout: VK_IMAGE_LAYOUT_UNDEFINED,
            loadOp: VK_ATTACHMENT_LOAD_OP_CLEAR,
            storeOp: VK_ATTACHMENT_STORE_OP_STORE,
            clearValue: [unorm(clear.r), unorm(clear.g), unorm(clear.b), 1.0],
        };
        let area = VkRect2D {
            offset: VkOffset2D { x: 0, y: 0 },
            extent: VkExtent2D { width: target.width, height: target.height },
        };
        let rendering = VkRenderingInfo {
            sType: VK_STRUCTURE_TYPE_RENDERING_INFO,
            pNext: ptr::null(),
            flags: 0,
            renderArea: area,
            layerCount: 1,
            viewMask: 0,
            colorAttachmentCount: 1,
            pColorAttachments: &color,
            pDepthAttachment: ptr::null(),
            pStencilAttachment: ptr::null(),
        };
        let viewport = VkViewport {
            x: 0.0,
            y: 0.0,
            width: target.width as f32,
            height: target.height as f32,
            minDepth: 0.0,
            maxDepth: 1.0,
        };
        let inv_viewport = [1.0 / target.width as f32, 1.0 / target.height as f32];
        let offset0: VkDeviceSize = 0;
        // SAFETY: recording into `cmd`; every struct outlives its call and
        // the buffers, set and pipeline stay alive until the fence signals.
        unsafe {
            for (image, range, to_dst, to_read) in &groups {
                let staging = slot.staging.as_ref().expect("ensured");
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(to_dst));
                (f.vkCmdCopyBufferToImage)(
                    cmd,
                    staging.buffer,
                    *image,
                    VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                    range.len() as u32,
                    copies[range.clone()].as_ptr(),
                );
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(to_read));
            }
            (f.vkCmdPipelineBarrier2)(cmd, &dependency(&to_color));
            for &image in &external {
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(&foreign(image, true)));
            }
            (f.vkCmdBeginRendering)(cmd, &rendering);
            if !quads.is_empty() {
                (f.vkCmdBindPipeline)(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, self.pipeline);
                (f.vkCmdSetViewport)(cmd, 0, 1, &viewport);
                (f.vkCmdSetScissor)(cmd, 0, 1, &area);
                (f.vkCmdPushConstants)(
                    cmd,
                    self.pipeline_layout,
                    VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    8,
                    inv_viewport.as_ptr().cast(),
                );
                (f.vkCmdBindVertexBuffers)(cmd, 0, 1, &instances_buffer(slot), &offset0);
                for &(set, first, count) in &runs {
                    let layout = self.pipeline_layout;
                    (f.vkCmdBindDescriptorSets)(
                        cmd,
                        VK_PIPELINE_BIND_POINT_GRAPHICS,
                        layout,
                        0,
                        1,
                        &set,
                        0,
                        ptr::null(),
                    );
                    (f.vkCmdDraw)(cmd, 4, count, 0, first);
                }
            }
            (f.vkCmdEndRendering)(cmd);
            (f.vkCmdPipelineBarrier2)(cmd, &dependency(&release));
            for &image in &external {
                (f.vkCmdPipelineBarrier2)(cmd, &dependency(&foreign(image, false)));
            }
            check("vkEndCommandBuffer", (f.vkEndCommandBuffer)(cmd))?;
        }

        let cmd_info = VkCommandBufferSubmitInfo {
            sType: VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO,
            pNext: ptr::null(),
            commandBuffer: cmd,
            deviceMask: 0,
        };
        let signal_info = signal.map(|semaphore| VkSemaphoreSubmitInfo {
            sType: VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO,
            pNext: ptr::null(),
            semaphore,
            value: 0,
            stageMask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
            deviceIndex: 0,
        });
        let submit = VkSubmitInfo2 {
            sType: VK_STRUCTURE_TYPE_SUBMIT_INFO_2,
            pNext: ptr::null(),
            flags: 0,
            waitSemaphoreInfoCount: 0,
            pWaitSemaphoreInfos: ptr::null(),
            commandBufferInfoCount: 1,
            pCommandBufferInfos: &cmd_info,
            signalSemaphoreInfoCount: signal_info.is_some() as u32,
            pSignalSemaphoreInfos: signal_info
                .as_ref()
                .map_or(ptr::null(), |s| (s as *const VkSemaphoreSubmitInfo).cast()),
        };
        // SAFETY: a recorded buffer and our fence, reset just before reuse.
        unsafe {
            check("vkResetFences", (f.vkResetFences)(d, 1, &slot.fence))?;
            check("vkQueueSubmit2", (f.vkQueueSubmit2)(device.queue, 1, &submit, slot.fence))?;
        }
        self.submitted += 1;
        slot.serial = self.submitted;
        slot.done = false;
        Ok(())
    }
}

fn instances_buffer(slot: &Slot) -> VkBuffer {
    slot.instances.as_ref().map_or(VK_NULL_HANDLE, |b| b.buffer)
}

impl Drop for QuadRenderer {
    fn drop(&mut self) {
        let _ = self.wait();
        self.slots.iter_mut().for_each(|s| (s.instances, s.staging) = (None, None));
        // Before the pool their sets come from.
        self.textures.clear();
        self.graveyard.clear();
        self.blank = None;
        let (d, f) = (self.device.raw, &self.device.fns);
        // SAFETY: all frames have completed; destroying null handles is a
        // no-op, so a partly built renderer cleans up too.
        unsafe {
            for s in &self.slots {
                (f.vkDestroyFence)(d, s.fence, ptr::null());
            }
            (f.vkDestroyCommandPool)(d, self.cmd_pool, ptr::null());
            (f.vkDestroyPipeline)(d, self.pipeline, ptr::null());
            (f.vkDestroyPipelineLayout)(d, self.pipeline_layout, ptr::null());
            (f.vkDestroyDescriptorPool)(d, self.descriptor_pool, ptr::null());
            (f.vkDestroyDescriptorSetLayout)(d, self.set_layout, ptr::null());
            (f.vkDestroySampler)(d, self.sampler, ptr::null());
        }
    }
}

/// A shader module, destroyed once the pipeline is built.
struct ShaderModule<'a>(VkShaderModule, &'a Device);

impl<'a> ShaderModule<'a> {
    fn new(device: &'a Device, spv: &[u8]) -> Result<ShaderModule<'a>> {
        let words: Vec<u32> = spv.as_chunks::<4>().0.iter().map(|&w| u32::from_le_bytes(w)).collect();
        let info = VkShaderModuleCreateInfo {
            sType: VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            codeSize: words.len() * 4,
            pCode: words.as_ptr(),
        };
        let mut m = VK_NULL_HANDLE;
        // SAFETY: SPIR-V words that outlive the call.
        check("vkCreateShaderModule", unsafe {
            (device.fns.vkCreateShaderModule)(device.raw, &info, ptr::null(), &mut m)
        })?;
        Ok(ShaderModule(m, device))
    }
}

impl Drop for ShaderModule<'_> {
    fn drop(&mut self) {
        // SAFETY: pipelines keep what they need; the module can go.
        unsafe { (self.1.fns.vkDestroyShaderModule)(self.1.raw, self.0, ptr::null()) };
    }
}

/// A same-queue image barrier: `(old, new)` layouts, `(stage, access)` before and after.
fn image_barrier(
    image: VkImage,
    (old, new): (i32, i32),
    (src_stage, src_access): (VkPipelineStageFlags2, VkAccessFlags2),
    (dst_stage, dst_access): (VkPipelineStageFlags2, VkAccessFlags2),
) -> VkImageMemoryBarrier2 {
    VkImageMemoryBarrier2 {
        sType: VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        pNext: ptr::null(),
        srcStageMask: src_stage,
        srcAccessMask: src_access,
        dstStageMask: dst_stage,
        dstAccessMask: dst_access,
        oldLayout: old,
        newLayout: new,
        srcQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
        dstQueueFamilyIndex: VK_QUEUE_FAMILY_IGNORED,
        image,
        subresourceRange: COLOR_RANGE,
    }
}

fn dependency(barrier: &VkImageMemoryBarrier2) -> VkDependencyInfo {
    VkDependencyInfo {
        sType: VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        pNext: ptr::null::<c_void>(),
        dependencyFlags: 0,
        memoryBarrierCount: 0,
        pMemoryBarriers: ptr::null(),
        bufferMemoryBarrierCount: 0,
        pBufferMemoryBarriers: ptr::null(),
        imageMemoryBarrierCount: 1,
        pImageMemoryBarriers: barrier,
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use termmd_core::font::{Face, FontSet, Loader};
    use termmd_core::render::cpu::{CpuRenderer, Target};
    use termmd_core::render::quads::{GlyphAtlas, build_quads};
    use termmd_core::render::resources::Resources;
    use termmd_core::render::shape::shape;
    use termmd_core::render::{FrameParams, build_frame};
    use termmd_core::term::{Palette, Terminal};

    use super::super::dmabuf::{ARGB8888, DRM_FORMAT_MOD_LINEAR};
    use super::*;
    use crate::sys;

    /// Needs a Vulkan driver and the test font; run with `--ignored`.
    #[test]
    #[ignore]
    fn gpu_frames_match_the_software_renderer() {
        let Ok(data) = std::fs::read("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf") else { return };
        let mut fonts = FontSet::new(Face::new(Box::new(data), 0).unwrap(), None, None, None, 16.0);
        if let Ok(data) = std::fs::read("/usr/share/fonts/noto/NotoColorEmoji.ttf") {
            let mut emoji = Face::new(Box::new(data), 0).ok();
            fonts.set_fallback(Loader(Box::new(move |_, color| if color { emoji.take() } else { None })));
        }
        let m = fonts.metrics();
        let device = Rc::new(super::super::open_default(None).expect("device"));
        let (cols, rows) = (40u16, 8u16);
        let (w, h) = (m.width * cols as u32 + 9, m.height * rows as u32 + 6);
        let usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
        let img = DmabufImage::export(&device, w, h, ARGB8888, &[DRM_FORMAT_MOD_LINEAR], usage).expect("export");
        let target = RenderTarget::new(&device, &img).expect("target");
        let mut renderer = QuadRenderer::new(&device, ARGB8888.vk).expect("renderer");
        // Small pages: glyphs spread over several, so draws switch textures.
        let mut res = Resources::new(64 << 20);
        let mut atlas = GlyphAtlas::new(64, 16);

        let mut t = Terminal::new(cols as usize, rows as usize, 0, Palette::default());
        t.set_cell_size(m.width as u16, m.height as u16);
        let p = FrameParams {
            width: w,
            height: h,
            origin: (4, 3),
            scroll_offset: 0,
            focused: true,
            blink_on: true,
            selection: None,
            link: None,
            table_cell: None,
        };
        let feeds: [&str; 5] = [
            "\x1b[44mHello\x1b[m, \x1b[1;31mworld\x1b[m gjpqy ╭──╮ 日本\r\n\x1b[4:3mcurly\x1b[m \x1b[9mstrike\x1b[m",
            "\r\n\x1b[7mreverse\x1b[m \x1b[2mdim\x1b[m ▁▂▃▄▅▆▇█ \x1b[3mitalic\x1b[m\x1b[5 q",
            "\x1b[2J\x1b[H\x1b[38;2;255;128;0mtruecolor\x1b[48;5;22m 256 \x1b[m ABCXYZ 😀 \x1b[44m🇦🇺\x1b[m ->",
            // Kitty graphics: an RGBA ramp fading out and an RGB checker,
            // under backgrounds, under text and over text.
            concat!(
                "\x1b[2J\x1b[H\x1b_Gi=1,s=8,v=6,q=2;AP8A/yTaAP9ItgD/bZEA/5FtAP+2SAD/2iQA//8AAP8A/zP/JNoz/0i2M/9tkTP/kW0z/7ZIM//aJDP//wAz/wD/Zv8k2mb/SLZm/22RZv+RbWb/tkhm/9okZv//AGb/AP+Z/yTamf9Itpn/bZGZ/5Ftmf+2SJn/2iSZ//8Amf8A/8ygJNrMoEi2zKBtkcygkW3MoLZIzKDaJMyg/wDMoAD//1Ak2v9QSLb/UG2R/1CRbf9Qtkj/UNok/1D/AP9Q\x1b\\",
                "\x1b_Gi=2,f=24,s=4,v=4,q=2;8PDwHh4e8PDwHh4eHh4e8PDwHh4e8PDw8PDwHh4e8PDwHh4eHh4e8PDwHh4e8PDw\x1b\\",
                "\x1b[44mblue cells\x1b[m over images\r\ntext under and over images\r\n\x1b[41m red \x1b[m",
                "\x1b[1;2H\x1b_Ga=p,i=2,c=6,r=4,z=-1073741825,C=1,q=2\x1b\\",
                "\x1b[2;9H\x1b_Ga=p,i=1,x=2,y=1,w=4,h=3,c=5,r=2,X=4,Y=6,z=-1,C=1,q=2\x1b\\",
                "\x1b[1;16H\x1b_Ga=p,i=1,c=9,r=5,z=1,C=1,q=2\x1b\\"
            ),
            // A Markdown block faded over all of that.
            concat!(
                "\x1b]7701;md;fixed=2,1 w=30 h=5 wrap=none opacity=50%\x1b\\",
                "# Heading\n\n~~struck~~ *text* 😀\n\n---\n",
                "\x1b]7701;md-end\x1b\\"
            ),
        ];
        for (i, feed) in feeds.iter().enumerate() {
            t.feed(feed.as_bytes());
            let mut frame = build_frame(&t, m, &p);
            shape(&mut frame, &mut fonts);
            let mut quads = Vec::new();
            res.begin_frame();
            build_quads(&frame, &mut fonts, &mut atlas, &mut res, &mut quads);
            renderer.apply(&res.take_events()).expect("textures");
            let uploads = atlas.take_uploads();
            renderer.render(&target, frame.layout.background, &quads, &uploads, None).expect("render");
            renderer.wait().expect("wait");

            let mut cpu = vec![0u32; (w * h) as usize];
            let mut ct = Target { pixels: &mut cpu, width: w, height: h, stride: w as usize, id: 1 };
            CpuRenderer::new().render(&frame, &mut fonts, &mut ct);
            let b = &img.buf;
            let bytes = sys::read_dmabuf(b.fd.as_fd(), b.byte_len()).expect("map");
            let (mut worst, mut off) = (0u8, 0usize);
            for y in 0..h {
                for x in 0..w {
                    let o = (b.plane.offset + y as u64 * b.plane.stride + x as u64 * 4) as usize;
                    let c = cpu[(y * w + x) as usize];
                    let want = [c as u8, (c >> 8) as u8, (c >> 16) as u8];
                    let d = (0..3).map(|k| bytes[o + k].abs_diff(want[k])).max().unwrap();
                    assert_eq!(bytes[o + 3], 255, "opaque at {x},{y}");
                    worst = worst.max(d);
                    off += (d > 0) as usize;
                }
            }
            let pages = renderer.textures();
            eprintln!(
                "frame {i}: {} quads, {} uploads, {pages} pages, {off} pixels off by <= {worst}",
                quads.len(),
                uploads.len()
            );
            assert!(worst <= 1, "frame {i}: GPU differs from software by {worst}");
        }
        assert!(renderer.textures() > 1);
        // Pages given back and destroyed are retired, then freed once idle.
        let before = renderer.textures();
        atlas.release(&mut res);
        res.begin_frame();
        let ids: Vec<_> = (1..=before as u32).map(termmd_core::render::resources::TextureId).collect();
        ids.iter().for_each(|&id| res.destroy(id));
        renderer.render(&target, Palette::default().background, &[], &[], None).expect("empty frame");
        renderer.apply(&res.take_events()).expect("destroy");
        assert_eq!((renderer.textures(), renderer.retired()), (0, before), "still possibly in use");
        renderer.wait().expect("wait");
        assert_eq!(renderer.retired(), 0);
    }

    /// A client's dma-buf (rendered by another renderer, in a tiled layout
    /// when the driver has one) sampled as an imported surface. Needs a
    /// Vulkan driver; run with `--ignored`.
    #[test]
    #[ignore]
    fn imported_surfaces_are_drawn() {
        use super::super::dmabuf::{Dmabuf, modifiers};
        use termmd_core::render::quads::QuadKind;
        let device = Rc::new(super::super::open_default(None).expect("device"));
        let usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT;
        let mods = modifiers(&device, ARGB8888, usage).expect("modifiers");
        let tiled: Vec<u64> = mods.iter().copied().filter(|&m| m != DRM_FORMAT_MOD_LINEAR).collect();
        let client_img =
            DmabufImage::export(&device, 24, 16, ARGB8888, if tiled.is_empty() { &mods } else { &tiled }, usage)
                .expect("client buffer");
        let mut client = QuadRenderer::new(&device, ARGB8888.vk).expect("client renderer");
        let client_target = RenderTarget::new(&device, &client_img).expect("client target");
        client.render(&client_target, Rgb::new(10, 200, 30), &[], &[], None).expect("client draw");
        client.wait().expect("client wait");

        let b = &client_img.buf;
        let buf = Dmabuf {
            fd: b.fd.try_clone().expect("dup"),
            width: b.width,
            height: b.height,
            format: b.format,
            modifier: b.modifier,
            plane: b.plane,
        };
        let imported = DmabufImage::import(&device, buf, VK_IMAGE_USAGE_SAMPLED_BIT).expect("import");
        let (w, h) = (40, 30);
        let img =
            DmabufImage::export(&device, w, h, ARGB8888, &[DRM_FORMAT_MOD_LINEAR], VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT)
                .expect("export");
        let target = RenderTarget::new(&device, &img).expect("target");
        let mut renderer = QuadRenderer::new(&device, ARGB8888.vk).expect("renderer");
        let id = TextureId(1000);
        renderer.import(id, imported).expect("sample it");
        // 1:1, starting 2 texels into the surface.
        let q = Quad {
            x: 5,
            y: 4,
            w: 20,
            h: 10,
            u: 2,
            v: 1,
            color: Rgb::new(0, 0, 0),
            alpha: 255,
            kind: QuadKind::Image(id),
        };
        renderer.render(&target, Rgb::new(0, 0, 0), &[q], &[], None).expect("render");
        assert!(renderer.completed() <= renderer.submitted());
        eprintln!("client modifier {:x}", client_img.buf.modifier);
        renderer.wait().expect("wait");
        assert_eq!(renderer.completed(), renderer.submitted());

        let t = &img.buf;
        let bytes = sys::read_dmabuf(t.fd.as_fd(), t.byte_len()).expect("map");
        let at = |x: u64, y: u64| {
            let o = (t.plane.offset + y * t.plane.stride + x * 4) as usize;
            [bytes[o + 2], bytes[o + 1], bytes[o]]
        };
        assert_eq!((at(5, 4), at(24, 13)), ([10, 200, 30], [10, 200, 30]), "modifier {:x}", b.modifier);
        assert_eq!((at(4, 4), at(25, 13), at(5, 14)), ([0; 3], [0; 3], [0; 3]));
        // Destroyed like any texture: retired until idle.
        renderer.apply(&[ResourceEvent::Destroyed(id)]).expect("destroy");
        renderer.wait().expect("wait");
        assert_eq!((renderer.textures(), renderer.retired()), (0, 0));
    }
}
