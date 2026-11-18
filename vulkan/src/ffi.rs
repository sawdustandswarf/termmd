//! Raw Vulkan declarations, written by hand from the specification (no
//! headers or registry needed). Only what we use; layouts are pinned by the
//! size tests below, which match the C ABI on 64-bit Linux.

#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_void};

pub type VkResult = i32;
pub type VkBool32 = u32;
pub type VkFlags = u32;
pub type VkDeviceSize = u64;
pub type VkStructureType = i32;

// Dispatchable handles are pointers.
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;
pub type VkDevice = *mut c_void;
pub type VkQueue = *mut c_void;

pub const VK_SUCCESS: VkResult = 0;
pub const VK_INCOMPLETE: VkResult = 5;

pub const VK_MAX_EXTENSION_NAME_SIZE: usize = 256;
pub const VK_MAX_DESCRIPTION_SIZE: usize = 256;
pub const VK_MAX_PHYSICAL_DEVICE_NAME_SIZE: usize = 256;
pub const VK_MAX_DRIVER_NAME_SIZE: usize = 256;
pub const VK_MAX_DRIVER_INFO_SIZE: usize = 256;
pub const VK_UUID_SIZE: usize = 16;
pub const VK_MAX_MEMORY_TYPES: usize = 32;
pub const VK_MAX_MEMORY_HEAPS: usize = 16;

pub const VK_STRUCTURE_TYPE_APPLICATION_INFO: VkStructureType = 0;
pub const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: VkStructureType = 1;
pub const VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO: VkStructureType = 2;
pub const VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO: VkStructureType = 3;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2: VkStructureType = 1000059001;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES: VkStructureType = 1000196000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRM_PROPERTIES_EXT: VkStructureType = 1000353000;

pub const VK_PHYSICAL_DEVICE_TYPE_OTHER: i32 = 0;
pub const VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU: i32 = 1;
pub const VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU: i32 = 2;
pub const VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU: i32 = 3;
pub const VK_PHYSICAL_DEVICE_TYPE_CPU: i32 = 4;

pub const VK_QUEUE_GRAPHICS_BIT: VkFlags = 0x1;
pub const VK_MEMORY_HEAP_DEVICE_LOCAL_BIT: VkFlags = 0x1;

pub const fn vk_make_api_version(variant: u32, major: u32, minor: u32, patch: u32) -> u32 {
    (variant << 29) | (major << 22) | (minor << 12) | patch
}

pub const VK_API_VERSION_1_3: u32 = vk_make_api_version(0, 1, 3, 0);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkApplicationInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub pApplicationName: *const c_char,
    pub applicationVersion: u32,
    pub pEngineName: *const c_char,
    pub engineVersion: u32,
    pub apiVersion: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkInstanceCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub pApplicationInfo: *const VkApplicationInfo,
    pub enabledLayerCount: u32,
    pub ppEnabledLayerNames: *const *const c_char,
    pub enabledExtensionCount: u32,
    pub ppEnabledExtensionNames: *const *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkExtensionProperties {
    pub extensionName: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    pub specVersion: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkLayerProperties {
    pub layerName: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    pub specVersion: u32,
    pub implementationVersion: u32,
    pub description: [c_char; VK_MAX_DESCRIPTION_SIZE],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkExtent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkQueueFamilyProperties {
    pub queueFlags: VkFlags,
    pub queueCount: u32,
    pub timestampValidBits: u32,
    pub minImageTransferGranularity: VkExtent3D,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkPhysicalDeviceLimits {
    pub maxImageDimension1D: u32,
    pub maxImageDimension2D: u32,
    pub maxImageDimension3D: u32,
    pub maxImageDimensionCube: u32,
    pub maxImageArrayLayers: u32,
    pub maxTexelBufferElements: u32,
    pub maxUniformBufferRange: u32,
    pub maxStorageBufferRange: u32,
    pub maxPushConstantsSize: u32,
    pub maxMemoryAllocationCount: u32,
    pub maxSamplerAllocationCount: u32,
    pub bufferImageGranularity: VkDeviceSize,
    pub sparseAddressSpaceSize: VkDeviceSize,
    pub maxBoundDescriptorSets: u32,
    pub maxPerStageDescriptorSamplers: u32,
    pub maxPerStageDescriptorUniformBuffers: u32,
    pub maxPerStageDescriptorStorageBuffers: u32,
    pub maxPerStageDescriptorSampledImages: u32,
    pub maxPerStageDescriptorStorageImages: u32,
    pub maxPerStageDescriptorInputAttachments: u32,
    pub maxPerStageResources: u32,
    pub maxDescriptorSetSamplers: u32,
    pub maxDescriptorSetUniformBuffers: u32,
    pub maxDescriptorSetUniformBuffersDynamic: u32,
    pub maxDescriptorSetStorageBuffers: u32,
    pub maxDescriptorSetStorageBuffersDynamic: u32,
    pub maxDescriptorSetSampledImages: u32,
    pub maxDescriptorSetStorageImages: u32,
    pub maxDescriptorSetInputAttachments: u32,
    pub maxVertexInputAttributes: u32,
    pub maxVertexInputBindings: u32,
    pub maxVertexInputAttributeOffset: u32,
    pub maxVertexInputBindingStride: u32,
    pub maxVertexOutputComponents: u32,
    pub maxTessellationGenerationLevel: u32,
    pub maxTessellationPatchSize: u32,
    pub maxTessellationControlPerVertexInputComponents: u32,
    pub maxTessellationControlPerVertexOutputComponents: u32,
    pub maxTessellationControlPerPatchOutputComponents: u32,
    pub maxTessellationControlTotalOutputComponents: u32,
    pub maxTessellationEvaluationInputComponents: u32,
    pub maxTessellationEvaluationOutputComponents: u32,
    pub maxGeometryShaderInvocations: u32,
    pub maxGeometryInputComponents: u32,
    pub maxGeometryOutputComponents: u32,
    pub maxGeometryOutputVertices: u32,
    pub maxGeometryTotalOutputComponents: u32,
    pub maxFragmentInputComponents: u32,
    pub maxFragmentOutputAttachments: u32,
    pub maxFragmentDualSrcAttachments: u32,
    pub maxFragmentCombinedOutputResources: u32,
    pub maxComputeSharedMemorySize: u32,
    pub maxComputeWorkGroupCount: [u32; 3],
    pub maxComputeWorkGroupInvocations: u32,
    pub maxComputeWorkGroupSize: [u32; 3],
    pub subPixelPrecisionBits: u32,
    pub subTexelPrecisionBits: u32,
    pub mipmapPrecisionBits: u32,
    pub maxDrawIndexedIndexValue: u32,
    pub maxDrawIndirectCount: u32,
    pub maxSamplerLodBias: f32,
    pub maxSamplerAnisotropy: f32,
    pub maxViewports: u32,
    pub maxViewportDimensions: [u32; 2],
    pub viewportBoundsRange: [f32; 2],
    pub viewportSubPixelBits: u32,
    pub minMemoryMapAlignment: usize,
    pub minTexelBufferOffsetAlignment: VkDeviceSize,
    pub minUniformBufferOffsetAlignment: VkDeviceSize,
    pub minStorageBufferOffsetAlignment: VkDeviceSize,
    pub minTexelOffset: i32,
    pub maxTexelOffset: u32,
    pub minTexelGatherOffset: i32,
    pub maxTexelGatherOffset: u32,
    pub minInterpolationOffset: f32,
    pub maxInterpolationOffset: f32,
    pub subPixelInterpolationOffsetBits: u32,
    pub maxFramebufferWidth: u32,
    pub maxFramebufferHeight: u32,
    pub maxFramebufferLayers: u32,
    pub framebufferColorSampleCounts: VkFlags,
    pub framebufferDepthSampleCounts: VkFlags,
    pub framebufferStencilSampleCounts: VkFlags,
    pub framebufferNoAttachmentsSampleCounts: VkFlags,
    pub maxColorAttachments: u32,
    pub sampledImageColorSampleCounts: VkFlags,
    pub sampledImageIntegerSampleCounts: VkFlags,
    pub sampledImageDepthSampleCounts: VkFlags,
    pub sampledImageStencilSampleCounts: VkFlags,
    pub storageImageSampleCounts: VkFlags,
    pub maxSampleMaskWords: u32,
    pub timestampComputeAndGraphics: VkBool32,
    pub timestampPeriod: f32,
    pub maxClipDistances: u32,
    pub maxCullDistances: u32,
    pub maxCombinedClipAndCullDistances: u32,
    pub discreteQueuePriorities: u32,
    pub pointSizeRange: [f32; 2],
    pub lineWidthRange: [f32; 2],
    pub pointSizeGranularity: f32,
    pub lineWidthGranularity: f32,
    pub strictLines: VkBool32,
    pub standardSampleLocations: VkBool32,
    pub optimalBufferCopyOffsetAlignment: VkDeviceSize,
    pub optimalBufferCopyRowPitchAlignment: VkDeviceSize,
    pub nonCoherentAtomSize: VkDeviceSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkPhysicalDeviceSparseProperties {
    pub residencyStandard2DBlockShape: VkBool32,
    pub residencyStandard2DMultisampleBlockShape: VkBool32,
    pub residencyStandard3DBlockShape: VkBool32,
    pub residencyAlignedMipSize: VkBool32,
    pub residencyNonResidentStrict: VkBool32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceProperties {
    pub apiVersion: u32,
    pub driverVersion: u32,
    pub vendorID: u32,
    pub deviceID: u32,
    pub deviceType: i32,
    pub deviceName: [c_char; VK_MAX_PHYSICAL_DEVICE_NAME_SIZE],
    pub pipelineCacheUUID: [u8; VK_UUID_SIZE],
    pub limits: VkPhysicalDeviceLimits,
    pub sparseProperties: VkPhysicalDeviceSparseProperties,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceProperties2 {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub properties: VkPhysicalDeviceProperties,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkConformanceVersion {
    pub major: u8,
    pub minor: u8,
    pub subminor: u8,
    pub patch: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceDriverProperties {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub driverID: i32,
    pub driverName: [c_char; VK_MAX_DRIVER_NAME_SIZE],
    pub driverInfo: [c_char; VK_MAX_DRIVER_INFO_SIZE],
    pub conformanceVersion: VkConformanceVersion,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceDrmPropertiesEXT {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub hasPrimary: VkBool32,
    pub hasRender: VkBool32,
    pub primaryMajor: i64,
    pub primaryMinor: i64,
    pub renderMajor: i64,
    pub renderMinor: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkMemoryType {
    pub propertyFlags: VkFlags,
    pub heapIndex: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkMemoryHeap {
    pub size: VkDeviceSize,
    pub flags: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceMemoryProperties {
    pub memoryTypeCount: u32,
    pub memoryTypes: [VkMemoryType; VK_MAX_MEMORY_TYPES],
    pub memoryHeapCount: u32,
    pub memoryHeaps: [VkMemoryHeap; VK_MAX_MEMORY_HEAPS],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDeviceQueueCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueFamilyIndex: u32,
    pub queueCount: u32,
    pub pQueuePriorities: *const f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDeviceCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueCreateInfoCount: u32,
    pub pQueueCreateInfos: *const VkDeviceQueueCreateInfo,
    pub enabledLayerCount: u32,
    pub ppEnabledLayerNames: *const *const c_char,
    pub enabledExtensionCount: u32,
    pub ppEnabledExtensionNames: *const *const c_char,
    /// `VkPhysicalDeviceFeatures`; we chain features through `pNext` instead.
    pub pEnabledFeatures: *const c_void,
}

// Non-dispatchable handles.
pub type VkImage = u64;
pub type VkDeviceMemory = u64;
pub type VkCommandPool = u64;
pub type VkFence = u64;
pub type VkCommandBuffer = *mut c_void;

pub const VK_NULL_HANDLE: u64 = 0;

pub const VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO: VkStructureType = 5;
pub const VK_STRUCTURE_TYPE_FENCE_CREATE_INFO: VkStructureType = 8;
pub const VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO: VkStructureType = 14;
pub const VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO: VkStructureType = 39;
pub const VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO: VkStructureType = 40;
pub const VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO: VkStructureType = 42;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES: VkStructureType = 53;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2: VkStructureType = 1000059000;
pub const VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2: VkStructureType = 1000059002;
pub const VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2: VkStructureType = 1000059003;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2: VkStructureType = 1000059004;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO: VkStructureType = 1000071000;
pub const VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES: VkStructureType = 1000071001;
pub const VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO: VkStructureType = 1000072001;
pub const VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO: VkStructureType = 1000072002;
pub const VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR: VkStructureType = 1000074000;
pub const VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR: VkStructureType = 1000074001;
pub const VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR: VkStructureType = 1000074002;
pub const VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO: VkStructureType = 1000127001;
pub const VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_EXT: VkStructureType = 1000158000;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT: VkStructureType = 1000158002;
pub const VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_LIST_CREATE_INFO_EXT: VkStructureType = 1000158003;
pub const VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT: VkStructureType = 1000158004;
pub const VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_PROPERTIES_EXT: VkStructureType = 1000158005;
pub const VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2: VkStructureType = 1000314002;
pub const VK_STRUCTURE_TYPE_DEPENDENCY_INFO: VkStructureType = 1000314003;
pub const VK_STRUCTURE_TYPE_SUBMIT_INFO_2: VkStructureType = 1000314004;
pub const VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO: VkStructureType = 1000314006;

pub const VK_FORMAT_B8G8R8A8_UNORM: i32 = 44;
pub const VK_IMAGE_TYPE_2D: i32 = 1;
pub const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i32 = 1000158000;
pub const VK_SAMPLE_COUNT_1_BIT: VkFlags = 1;
pub const VK_SHARING_MODE_EXCLUSIVE: i32 = 0;

pub const VK_IMAGE_USAGE_TRANSFER_SRC_BIT: VkFlags = 0x1;
pub const VK_IMAGE_USAGE_TRANSFER_DST_BIT: VkFlags = 0x2;
pub const VK_IMAGE_USAGE_SAMPLED_BIT: VkFlags = 0x4;
pub const VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT: VkFlags = 0x10;

pub const VK_IMAGE_LAYOUT_UNDEFINED: i32 = 0;
pub const VK_IMAGE_LAYOUT_GENERAL: i32 = 1;
pub const VK_COMPONENT_SWIZZLE_ONE: i32 = 2;
pub const VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL: i32 = 2;
pub const VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL: i32 = 7;

pub const VK_IMAGE_ASPECT_COLOR_BIT: VkFlags = 0x1;
pub const VK_IMAGE_ASPECT_MEMORY_PLANE_0_BIT_EXT: VkFlags = 0x80;

pub const VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT: VkFlags = 0x80;
pub const VK_FORMAT_FEATURE_TRANSFER_DST_BIT: VkFlags = 0x8000;

pub const VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT: VkFlags = 0x200;
pub const VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT: VkFlags = 0x2;
pub const VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT: VkFlags = 0x4;

pub const VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT: VkFlags = 0x1;

pub const VK_QUEUE_FAMILY_IGNORED: u32 = !0;
pub const VK_QUEUE_FAMILY_FOREIGN_EXT: u32 = !2;

pub const VK_COMMAND_POOL_CREATE_TRANSIENT_BIT: VkFlags = 0x1;
pub const VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT: VkFlags = 0x2;
pub const VK_COMMAND_BUFFER_LEVEL_PRIMARY: i32 = 0;
pub const VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: VkFlags = 0x1;

pub type VkPipelineStageFlags2 = u64;
pub type VkAccessFlags2 = u64;
pub const VK_PIPELINE_STAGE_2_NONE: VkPipelineStageFlags2 = 0;
pub const VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT: VkPipelineStageFlags2 = 0x400;
pub const VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT: VkPipelineStageFlags2 = 0x10000;
pub const VK_PIPELINE_STAGE_2_CLEAR_BIT: VkPipelineStageFlags2 = 0x8_0000_0000;
pub const VK_ACCESS_2_NONE: VkAccessFlags2 = 0;
pub const VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT: VkAccessFlags2 = 0x100;
pub const VK_ACCESS_2_TRANSFER_WRITE_BIT: VkAccessFlags2 = 0x1000;
pub const VK_ACCESS_2_MEMORY_READ_BIT: VkAccessFlags2 = 0x8000;
pub const VK_ACCESS_2_MEMORY_WRITE_BIT: VkAccessFlags2 = 0x10000;

/// `VkPhysicalDeviceFeatures`: 55 booleans we don't use individually.
pub type VkPhysicalDeviceFeatures = [VkBool32; 55];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceFeatures2 {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub features: VkPhysicalDeviceFeatures,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceVulkan13Features {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub robustImageAccess: VkBool32,
    pub inlineUniformBlock: VkBool32,
    pub descriptorBindingInlineUniformBlockUpdateAfterBind: VkBool32,
    pub pipelineCreationCacheControl: VkBool32,
    pub privateData: VkBool32,
    pub shaderDemoteToHelperInvocation: VkBool32,
    pub shaderTerminateInvocation: VkBool32,
    pub subgroupSizeControl: VkBool32,
    pub computeFullSubgroups: VkBool32,
    pub synchronization2: VkBool32,
    pub textureCompressionASTC_HDR: VkBool32,
    pub shaderZeroInitializeWorkgroupMemory: VkBool32,
    pub dynamicRendering: VkBool32,
    pub shaderIntegerDotProduct: VkBool32,
    pub maintenance4: VkBool32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkFormatProperties {
    pub linearTilingFeatures: VkFlags,
    pub optimalTilingFeatures: VkFlags,
    pub bufferFeatures: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkFormatProperties2 {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub formatProperties: VkFormatProperties,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkDrmFormatModifierPropertiesEXT {
    pub drmFormatModifier: u64,
    pub drmFormatModifierPlaneCount: u32,
    pub drmFormatModifierTilingFeatures: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDrmFormatModifierPropertiesListEXT {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub drmFormatModifierCount: u32,
    pub pDrmFormatModifierProperties: *mut VkDrmFormatModifierPropertiesEXT,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceImageFormatInfo2 {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub format: i32,
    pub imageType: i32,
    pub tiling: i32,
    pub usage: VkFlags,
    pub flags: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceExternalImageFormatInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub handleType: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPhysicalDeviceImageDrmFormatModifierInfoEXT {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub drmFormatModifier: u64,
    pub sharingMode: i32,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkImageFormatProperties {
    pub maxExtent: VkExtent3D,
    pub maxMipLevels: u32,
    pub maxArrayLayers: u32,
    pub sampleCounts: VkFlags,
    pub maxResourceSize: VkDeviceSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageFormatProperties2 {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub imageFormatProperties: VkImageFormatProperties,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkExternalMemoryProperties {
    pub externalMemoryFeatures: VkFlags,
    pub exportFromImportedHandleTypes: VkFlags,
    pub compatibleHandleTypes: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkExternalImageFormatProperties {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub externalMemoryProperties: VkExternalMemoryProperties,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub imageType: i32,
    pub format: i32,
    pub extent: VkExtent3D,
    pub mipLevels: u32,
    pub arrayLayers: u32,
    pub samples: VkFlags,
    pub tiling: i32,
    pub usage: VkFlags,
    pub sharingMode: i32,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
    pub initialLayout: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkExternalMemoryImageCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub handleTypes: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageDrmFormatModifierListCreateInfoEXT {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub drmFormatModifierCount: u32,
    pub pDrmFormatModifiers: *const u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VkSubresourceLayout {
    pub offset: VkDeviceSize,
    pub size: VkDeviceSize,
    pub rowPitch: VkDeviceSize,
    pub arrayPitch: VkDeviceSize,
    pub depthPitch: VkDeviceSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageDrmFormatModifierExplicitCreateInfoEXT {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub drmFormatModifier: u64,
    pub drmFormatModifierPlaneCount: u32,
    pub pPlaneLayouts: *const VkSubresourceLayout,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageDrmFormatModifierPropertiesEXT {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub drmFormatModifier: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkImageSubresource {
    pub aspectMask: VkFlags,
    pub mipLevel: u32,
    pub arrayLayer: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkMemoryRequirements {
    pub size: VkDeviceSize,
    pub alignment: VkDeviceSize,
    pub memoryTypeBits: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkMemoryAllocateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub allocationSize: VkDeviceSize,
    pub memoryTypeIndex: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkExportMemoryAllocateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub handleTypes: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImportMemoryFdInfoKHR {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub handleType: VkFlags,
    pub fd: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkMemoryFdPropertiesKHR {
    pub sType: VkStructureType,
    pub pNext: *mut c_void,
    pub memoryTypeBits: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkMemoryDedicatedAllocateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub image: VkImage,
    pub buffer: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkMemoryGetFdInfoKHR {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub memory: VkDeviceMemory,
    pub handleType: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkCommandPoolCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub queueFamilyIndex: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkCommandBufferAllocateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub commandPool: VkCommandPool,
    pub level: i32,
    pub commandBufferCount: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkCommandBufferBeginInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub pInheritanceInfo: *const c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkFenceCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkImageSubresourceRange {
    pub aspectMask: VkFlags,
    pub baseMipLevel: u32,
    pub levelCount: u32,
    pub baseArrayLayer: u32,
    pub layerCount: u32,
}

/// `VkClearColorValue`, as floats.
pub type VkClearColorValue = [f32; 4];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageMemoryBarrier2 {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub srcStageMask: VkPipelineStageFlags2,
    pub srcAccessMask: VkAccessFlags2,
    pub dstStageMask: VkPipelineStageFlags2,
    pub dstAccessMask: VkAccessFlags2,
    pub oldLayout: i32,
    pub newLayout: i32,
    pub srcQueueFamilyIndex: u32,
    pub dstQueueFamilyIndex: u32,
    pub image: VkImage,
    pub subresourceRange: VkImageSubresourceRange,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDependencyInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub dependencyFlags: VkFlags,
    pub memoryBarrierCount: u32,
    pub pMemoryBarriers: *const c_void,
    pub bufferMemoryBarrierCount: u32,
    pub pBufferMemoryBarriers: *const c_void,
    pub imageMemoryBarrierCount: u32,
    pub pImageMemoryBarriers: *const VkImageMemoryBarrier2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkCommandBufferSubmitInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub commandBuffer: VkCommandBuffer,
    pub deviceMask: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkSubmitInfo2 {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub waitSemaphoreInfoCount: u32,
    pub pWaitSemaphoreInfos: *const c_void,
    pub commandBufferInfoCount: u32,
    pub pCommandBufferInfos: *const VkCommandBufferSubmitInfo,
    pub signalSemaphoreInfoCount: u32,
    pub pSignalSemaphoreInfos: *const c_void,
}

pub type VkSemaphore = u64;

pub const VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO: VkStructureType = 9;
pub const VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO: VkStructureType = 1000077000;
pub const VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR: VkStructureType = 1000079001;
pub const VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO: VkStructureType = 1000314005;
pub const VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT: VkFlags = 0x10;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkSemaphoreCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkExportSemaphoreCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub handleTypes: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkSemaphoreGetFdInfoKHR {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub semaphore: VkSemaphore,
    pub handleType: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkSemaphoreSubmitInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub semaphore: VkSemaphore,
    pub value: u64,
    pub stageMask: VkPipelineStageFlags2,
    pub deviceIndex: u32,
}

// Pipelines, buffers and dynamic rendering (the quad renderer).

pub type VkBuffer = u64;
pub type VkImageView = u64;
pub type VkSampler = u64;
pub type VkDescriptorSetLayout = u64;
pub type VkDescriptorPool = u64;
pub type VkDescriptorSet = u64;
pub type VkPipelineLayout = u64;
pub type VkPipeline = u64;
pub type VkShaderModule = u64;

pub const VK_WHOLE_SIZE: u64 = !0;

pub const VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO: VkStructureType = 12;
pub const VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO: VkStructureType = 15;
pub const VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO: VkStructureType = 16;
pub const VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO: VkStructureType = 18;
pub const VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO: VkStructureType = 19;
pub const VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO: VkStructureType = 20;
pub const VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO: VkStructureType = 22;
pub const VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO: VkStructureType = 23;
pub const VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO: VkStructureType = 24;
pub const VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO: VkStructureType = 26;
pub const VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO: VkStructureType = 27;
pub const VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO: VkStructureType = 28;
pub const VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO: VkStructureType = 30;
pub const VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO: VkStructureType = 31;
pub const VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: VkStructureType = 32;
pub const VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO: VkStructureType = 33;
pub const VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO: VkStructureType = 34;
pub const VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET: VkStructureType = 35;
pub const VK_STRUCTURE_TYPE_RENDERING_INFO: VkStructureType = 1000044000;
pub const VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO: VkStructureType = 1000044001;
pub const VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO: VkStructureType = 1000044002;

pub const VK_FORMAT_R8_UNORM: i32 = 9;
pub const VK_FORMAT_R8G8B8A8_UNORM: i32 = 37;
pub const VK_FORMAT_R16G16_UINT: i32 = 81;
pub const VK_FORMAT_R32_UINT: i32 = 98;
pub const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
pub const VK_IMAGE_VIEW_TYPE_2D: i32 = 1;
pub const VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL: i32 = 5;

pub const VK_BUFFER_USAGE_TRANSFER_SRC_BIT: VkFlags = 0x1;
pub const VK_BUFFER_USAGE_VERTEX_BUFFER_BIT: VkFlags = 0x80;
pub const VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT: VkFlags = 0x2;
pub const VK_MEMORY_PROPERTY_HOST_COHERENT_BIT: VkFlags = 0x4;

pub const VK_FILTER_NEAREST: i32 = 0;
pub const VK_SAMPLER_MIPMAP_MODE_NEAREST: i32 = 0;
pub const VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE: i32 = 2;
pub const VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER: i32 = 1;
pub const VK_SHADER_STAGE_VERTEX_BIT: VkFlags = 0x1;
pub const VK_SHADER_STAGE_FRAGMENT_BIT: VkFlags = 0x10;
pub const VK_VERTEX_INPUT_RATE_INSTANCE: i32 = 1;
pub const VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP: i32 = 4;
pub const VK_POLYGON_MODE_FILL: i32 = 0;
pub const VK_CULL_MODE_NONE: VkFlags = 0;
pub const VK_FRONT_FACE_COUNTER_CLOCKWISE: i32 = 0;
pub const VK_BLEND_FACTOR_ONE: i32 = 1;
pub const VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA: i32 = 7;
pub const VK_BLEND_OP_ADD: i32 = 0;
pub const VK_COLOR_COMPONENT_RGBA: VkFlags = 0xf;
pub const VK_DYNAMIC_STATE_VIEWPORT: i32 = 0;
pub const VK_DYNAMIC_STATE_SCISSOR: i32 = 1;
pub const VK_PIPELINE_BIND_POINT_GRAPHICS: i32 = 0;
pub const VK_ATTACHMENT_LOAD_OP_LOAD: i32 = 0;
pub const VK_ATTACHMENT_LOAD_OP_CLEAR: i32 = 1;
pub const VK_ATTACHMENT_STORE_OP_STORE: i32 = 0;
pub const VK_RESOLVE_MODE_NONE: VkFlags = 0;

pub const VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT: VkPipelineStageFlags2 = 0x80;
pub const VK_PIPELINE_STAGE_2_COPY_BIT: VkPipelineStageFlags2 = 0x1_0000_0000;
pub const VK_ACCESS_2_SHADER_SAMPLED_READ_BIT: VkAccessFlags2 = 0x1_0000_0000;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkBufferCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub size: VkDeviceSize,
    pub usage: VkFlags,
    pub sharingMode: i32,
    pub queueFamilyIndexCount: u32,
    pub pQueueFamilyIndices: *const u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkComponentMapping {
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub a: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkImageViewCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub image: VkImage,
    pub viewType: i32,
    pub format: i32,
    pub components: VkComponentMapping,
    pub subresourceRange: VkImageSubresourceRange,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkSamplerCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub magFilter: i32,
    pub minFilter: i32,
    pub mipmapMode: i32,
    pub addressModeU: i32,
    pub addressModeV: i32,
    pub addressModeW: i32,
    pub mipLodBias: f32,
    pub anisotropyEnable: VkBool32,
    pub maxAnisotropy: f32,
    pub compareEnable: VkBool32,
    pub compareOp: i32,
    pub minLod: f32,
    pub maxLod: f32,
    pub borderColor: i32,
    pub unnormalizedCoordinates: VkBool32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorSetLayoutBinding {
    pub binding: u32,
    pub descriptorType: i32,
    pub descriptorCount: u32,
    pub stageFlags: VkFlags,
    pub pImmutableSamplers: *const VkSampler,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorSetLayoutCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub bindingCount: u32,
    pub pBindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorPoolSize {
    pub type_: i32,
    pub descriptorCount: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorPoolCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub maxSets: u32,
    pub poolSizeCount: u32,
    pub pPoolSizes: *const VkDescriptorPoolSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorSetAllocateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub descriptorPool: VkDescriptorPool,
    pub descriptorSetCount: u32,
    pub pSetLayouts: *const VkDescriptorSetLayout,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkDescriptorImageInfo {
    pub sampler: VkSampler,
    pub imageView: VkImageView,
    pub imageLayout: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkWriteDescriptorSet {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub dstSet: VkDescriptorSet,
    pub dstBinding: u32,
    pub dstArrayElement: u32,
    pub descriptorCount: u32,
    pub descriptorType: i32,
    pub pImageInfo: *const VkDescriptorImageInfo,
    pub pBufferInfo: *const c_void,
    pub pTexelBufferView: *const c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPushConstantRange {
    pub stageFlags: VkFlags,
    pub offset: u32,
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineLayoutCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub setLayoutCount: u32,
    pub pSetLayouts: *const VkDescriptorSetLayout,
    pub pushConstantRangeCount: u32,
    pub pPushConstantRanges: *const VkPushConstantRange,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkShaderModuleCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub codeSize: usize,
    pub pCode: *const u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineShaderStageCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub stage: VkFlags,
    pub module: VkShaderModule,
    pub pName: *const c_char,
    pub pSpecializationInfo: *const c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkVertexInputBindingDescription {
    pub binding: u32,
    pub stride: u32,
    pub inputRate: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkVertexInputAttributeDescription {
    pub location: u32,
    pub binding: u32,
    pub format: i32,
    pub offset: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineVertexInputStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub vertexBindingDescriptionCount: u32,
    pub pVertexBindingDescriptions: *const VkVertexInputBindingDescription,
    pub vertexAttributeDescriptionCount: u32,
    pub pVertexAttributeDescriptions: *const VkVertexInputAttributeDescription,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineInputAssemblyStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub topology: i32,
    pub primitiveRestartEnable: VkBool32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineViewportStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub viewportCount: u32,
    pub pViewports: *const VkViewport,
    pub scissorCount: u32,
    pub pScissors: *const VkRect2D,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineRasterizationStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub depthClampEnable: VkBool32,
    pub rasterizerDiscardEnable: VkBool32,
    pub polygonMode: i32,
    pub cullMode: VkFlags,
    pub frontFace: i32,
    pub depthBiasEnable: VkBool32,
    pub depthBiasConstantFactor: f32,
    pub depthBiasClamp: f32,
    pub depthBiasSlopeFactor: f32,
    pub lineWidth: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineMultisampleStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub rasterizationSamples: VkFlags,
    pub sampleShadingEnable: VkBool32,
    pub minSampleShading: f32,
    pub pSampleMask: *const u32,
    pub alphaToCoverageEnable: VkBool32,
    pub alphaToOneEnable: VkBool32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineColorBlendAttachmentState {
    pub blendEnable: VkBool32,
    pub srcColorBlendFactor: i32,
    pub dstColorBlendFactor: i32,
    pub colorBlendOp: i32,
    pub srcAlphaBlendFactor: i32,
    pub dstAlphaBlendFactor: i32,
    pub alphaBlendOp: i32,
    pub colorWriteMask: VkFlags,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineColorBlendStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub logicOpEnable: VkBool32,
    pub logicOp: i32,
    pub attachmentCount: u32,
    pub pAttachments: *const VkPipelineColorBlendAttachmentState,
    pub blendConstants: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineDynamicStateCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub dynamicStateCount: u32,
    pub pDynamicStates: *const i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkPipelineRenderingCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub viewMask: u32,
    pub colorAttachmentCount: u32,
    pub pColorAttachmentFormats: *const i32,
    pub depthAttachmentFormat: i32,
    pub stencilAttachmentFormat: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkGraphicsPipelineCreateInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub stageCount: u32,
    pub pStages: *const VkPipelineShaderStageCreateInfo,
    pub pVertexInputState: *const VkPipelineVertexInputStateCreateInfo,
    pub pInputAssemblyState: *const VkPipelineInputAssemblyStateCreateInfo,
    pub pTessellationState: *const c_void,
    pub pViewportState: *const VkPipelineViewportStateCreateInfo,
    pub pRasterizationState: *const VkPipelineRasterizationStateCreateInfo,
    pub pMultisampleState: *const VkPipelineMultisampleStateCreateInfo,
    pub pDepthStencilState: *const c_void,
    pub pColorBlendState: *const VkPipelineColorBlendStateCreateInfo,
    pub pDynamicState: *const VkPipelineDynamicStateCreateInfo,
    pub layout: VkPipelineLayout,
    pub renderPass: u64,
    pub subpass: u32,
    pub basePipelineHandle: VkPipeline,
    pub basePipelineIndex: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkViewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub minDepth: f32,
    pub maxDepth: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkOffset2D {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkExtent2D {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkRect2D {
    pub offset: VkOffset2D,
    pub extent: VkExtent2D,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkRenderingAttachmentInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub imageView: VkImageView,
    pub imageLayout: i32,
    pub resolveMode: VkFlags,
    pub resolveImageView: VkImageView,
    pub resolveImageLayout: i32,
    pub loadOp: i32,
    pub storeOp: i32,
    /// `VkClearValue`: the colour as floats.
    pub clearValue: VkClearColorValue,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VkRenderingInfo {
    pub sType: VkStructureType,
    pub pNext: *const c_void,
    pub flags: VkFlags,
    pub renderArea: VkRect2D,
    pub layerCount: u32,
    pub viewMask: u32,
    pub colorAttachmentCount: u32,
    pub pColorAttachments: *const VkRenderingAttachmentInfo,
    pub pDepthAttachment: *const c_void,
    pub pStencilAttachment: *const c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkImageSubresourceLayers {
    pub aspectMask: VkFlags,
    pub mipLevel: u32,
    pub baseArrayLayer: u32,
    pub layerCount: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkOffset3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VkBufferImageCopy {
    pub bufferOffset: VkDeviceSize,
    pub bufferRowLength: u32,
    pub bufferImageHeight: u32,
    pub imageSubresource: VkImageSubresourceLayers,
    pub imageOffset: VkOffset3D,
    pub imageExtent: VkExtent3D,
}

/// What `vkGet*ProcAddr` returns.
pub type PFN_vkVoidFunction = Option<unsafe extern "C" fn()>;

pub type PFN_vkGetInstanceProcAddr = unsafe extern "C" fn(VkInstance, *const c_char) -> PFN_vkVoidFunction;
pub type PFN_vkGetDeviceProcAddr = unsafe extern "C" fn(VkDevice, *const c_char) -> PFN_vkVoidFunction;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layouts_match_the_c_abi() {
        assert_eq!(size_of::<VkPhysicalDeviceLimits>(), 504);
        assert_eq!(size_of::<VkPhysicalDeviceSparseProperties>(), 20);
        assert_eq!(size_of::<VkPhysicalDeviceProperties>(), 824);
        assert_eq!(offset_of!(VkPhysicalDeviceProperties, limits), 296);
        assert_eq!(offset_of!(VkPhysicalDeviceLimits, minMemoryMapAlignment), 304);
        assert_eq!(offset_of!(VkPhysicalDeviceLimits, nonCoherentAtomSize), 496);
        assert_eq!(size_of::<VkPhysicalDeviceProperties2>(), 840);
        assert_eq!(size_of::<VkPhysicalDeviceMemoryProperties>(), 520);
        assert_eq!(size_of::<VkPhysicalDeviceDriverProperties>(), 536);
        assert_eq!(size_of::<VkPhysicalDeviceDrmPropertiesEXT>(), 56);
        assert_eq!(size_of::<VkQueueFamilyProperties>(), 24);
        assert_eq!(size_of::<VkExtensionProperties>(), 260);
        assert_eq!(size_of::<VkLayerProperties>(), 520);
        assert_eq!(size_of::<VkApplicationInfo>(), 48);
        assert_eq!(size_of::<VkInstanceCreateInfo>(), 64);
        assert_eq!(size_of::<VkDeviceQueueCreateInfo>(), 40);
        assert_eq!(size_of::<VkDeviceCreateInfo>(), 72);
        assert_eq!(size_of::<VkPhysicalDeviceFeatures2>(), 240);
        assert_eq!(size_of::<VkPhysicalDeviceVulkan13Features>(), 80);
        assert_eq!(size_of::<VkFormatProperties2>(), 32);
        assert_eq!(size_of::<VkDrmFormatModifierPropertiesEXT>(), 16);
        assert_eq!(size_of::<VkDrmFormatModifierPropertiesListEXT>(), 32);
        assert_eq!(size_of::<VkPhysicalDeviceImageFormatInfo2>(), 40);
        assert_eq!(size_of::<VkPhysicalDeviceImageDrmFormatModifierInfoEXT>(), 40);
        assert_eq!(size_of::<VkImageFormatProperties2>(), 48);
        assert_eq!(size_of::<VkExternalImageFormatProperties>(), 32);
        assert_eq!(size_of::<VkImageCreateInfo>(), 88);
        assert_eq!(offset_of!(VkImageCreateInfo, initialLayout), 80);
        assert_eq!(size_of::<VkImageDrmFormatModifierExplicitCreateInfoEXT>(), 40);
        assert_eq!(size_of::<VkSubresourceLayout>(), 40);
        assert_eq!(size_of::<VkMemoryRequirements>(), 24);
        assert_eq!(size_of::<VkMemoryAllocateInfo>(), 32);
        assert_eq!(size_of::<VkImportMemoryFdInfoKHR>(), 24);
        assert_eq!(size_of::<VkMemoryDedicatedAllocateInfo>(), 32);
        assert_eq!(size_of::<VkMemoryGetFdInfoKHR>(), 32);
        assert_eq!(size_of::<VkImageMemoryBarrier2>(), 96);
        assert_eq!(offset_of!(VkImageMemoryBarrier2, image), 64);
        assert_eq!(size_of::<VkDependencyInfo>(), 64);
        assert_eq!(size_of::<VkSubmitInfo2>(), 64);
        assert_eq!(size_of::<VkCommandBufferSubmitInfo>(), 32);
        assert_eq!(size_of::<VkSemaphoreGetFdInfoKHR>(), 32);
        assert_eq!(size_of::<VkSemaphoreSubmitInfo>(), 48);
        assert_eq!(size_of::<VkBufferCreateInfo>(), 56);
        assert_eq!(size_of::<VkImageViewCreateInfo>(), 80);
        assert_eq!(size_of::<VkSamplerCreateInfo>(), 80);
        assert_eq!(size_of::<VkDescriptorSetLayoutBinding>(), 24);
        assert_eq!(size_of::<VkDescriptorSetLayoutCreateInfo>(), 32);
        assert_eq!(size_of::<VkDescriptorPoolCreateInfo>(), 40);
        assert_eq!(size_of::<VkDescriptorSetAllocateInfo>(), 40);
        assert_eq!(size_of::<VkDescriptorImageInfo>(), 24);
        assert_eq!(size_of::<VkWriteDescriptorSet>(), 64);
        assert_eq!(size_of::<VkPipelineLayoutCreateInfo>(), 48);
        assert_eq!(size_of::<VkShaderModuleCreateInfo>(), 40);
        assert_eq!(size_of::<VkPipelineShaderStageCreateInfo>(), 48);
        assert_eq!(size_of::<VkPipelineVertexInputStateCreateInfo>(), 48);
        assert_eq!(size_of::<VkPipelineInputAssemblyStateCreateInfo>(), 32);
        assert_eq!(size_of::<VkPipelineViewportStateCreateInfo>(), 48);
        assert_eq!(size_of::<VkPipelineRasterizationStateCreateInfo>(), 64);
        assert_eq!(size_of::<VkPipelineMultisampleStateCreateInfo>(), 48);
        assert_eq!(size_of::<VkPipelineColorBlendAttachmentState>(), 32);
        assert_eq!(size_of::<VkPipelineColorBlendStateCreateInfo>(), 56);
        assert_eq!(size_of::<VkPipelineDynamicStateCreateInfo>(), 32);
        assert_eq!(size_of::<VkPipelineRenderingCreateInfo>(), 40);
        assert_eq!(size_of::<VkGraphicsPipelineCreateInfo>(), 144);
        assert_eq!(offset_of!(VkGraphicsPipelineCreateInfo, layout), 104);
        assert_eq!(size_of::<VkRenderingAttachmentInfo>(), 72);
        assert_eq!(offset_of!(VkRenderingAttachmentInfo, clearValue), 52);
        assert_eq!(size_of::<VkRenderingInfo>(), 72);
        assert_eq!(size_of::<VkBufferImageCopy>(), 56);
    }
}
