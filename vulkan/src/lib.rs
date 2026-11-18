//! `termmd-vulkan`: our own Vulkan bindings, std only. The loader is
//! `dlopen`ed from `libvulkan.so.1`, entry points are fetched into typed
//! tables, and a small safe layer creates the instance, picks and opens a
//! device, and makes images that are dma-bufs (`dmabuf`) and semaphores
//! that export sync files (`sync`).
//!
//! No window-system extensions: termmd renders frames into dma-bufs and
//! hands them to the compositor (`zwp_linux_dmabuf_v1`), and an embedded
//! app renders into dma-bufs it hands to termmd (docs/SURFACE.md). termmd's
//! own renderer is in `termmd-linux` (`vulkan::renderer`).

pub mod dmabuf;
pub mod ffi;
mod library;
pub mod sync;

use std::ffi::{CStr, c_char};
use std::fmt;
use std::ptr;
use std::rc::Rc;

use ffi::*;
use library::Library;

/// Device extensions the renderer cannot work without: dmabuf import and
/// export with explicit modifiers, and sync-fd semaphores.
pub const REQUIRED_DEVICE_EXTENSIONS: [&CStr; 5] = [
    c"VK_KHR_external_memory_fd",
    c"VK_EXT_external_memory_dma_buf",
    c"VK_EXT_image_drm_format_modifier",
    c"VK_KHR_external_semaphore_fd",
    c"VK_EXT_queue_family_foreign",
];

/// Lowest Vulkan version we use (dynamic rendering, synchronization2).
pub const MIN_API_VERSION: u32 = VK_API_VERSION_1_3;

#[derive(Debug)]
pub enum Error {
    /// The loader library could not be opened.
    Load(String),
    /// A required entry point is missing.
    Missing(&'static str),
    /// A call failed.
    Call(&'static str, VkResult),
    /// No device meets the requirements; one line per device considered.
    NoDevice(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Load(e) => write!(f, "cannot load libvulkan.so.1: {e}"),
            Error::Missing(name) => write!(f, "Vulkan entry point {name} is missing"),
            Error::Call(name, r) => write!(f, "{name} failed: {}", result_name(*r)),
            Error::NoDevice(why) if why.is_empty() => write!(f, "no Vulkan devices"),
            Error::NoDevice(why) => write!(f, "no suitable Vulkan device ({})", why.join("; ")),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn result_name(r: VkResult) -> String {
    let name = match r {
        1 => "VK_NOT_READY",
        2 => "VK_TIMEOUT",
        5 => "VK_INCOMPLETE",
        -1 => "VK_ERROR_OUT_OF_HOST_MEMORY",
        -2 => "VK_ERROR_OUT_OF_DEVICE_MEMORY",
        -3 => "VK_ERROR_INITIALIZATION_FAILED",
        -4 => "VK_ERROR_DEVICE_LOST",
        -6 => "VK_ERROR_LAYER_NOT_PRESENT",
        -7 => "VK_ERROR_EXTENSION_NOT_PRESENT",
        -8 => "VK_ERROR_FEATURE_NOT_PRESENT",
        -9 => "VK_ERROR_INCOMPATIBLE_DRIVER",
        _ => return format!("VkResult {r}"),
    };
    name.to_owned()
}

/// `r` as a [`Result`]: an error naming the call `name` if it failed.
pub fn check(name: &'static str, r: VkResult) -> Result<()> {
    if r >= 0 { Ok(()) } else { Err(Error::Call(name, r)) }
}

/// A fixed-size, NUL-padded C string field as a `String`.
fn fixed_str(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Fill a `Vec` through Vulkan's two-call enumeration pattern.
fn enumerate<T: Copy>(
    name: &'static str,
    zero: T,
    mut call: impl FnMut(&mut u32, *mut T) -> VkResult,
) -> Result<Vec<T>> {
    loop {
        let mut n = 0;
        check(name, call(&mut n, ptr::null_mut()))?;
        let mut v = vec![zero; n as usize];
        let r = call(&mut n, v.as_mut_ptr());
        if r == VK_INCOMPLETE {
            continue; // grew between the calls
        }
        check(name, r)?;
        v.truncate(n as usize);
        return Ok(v);
    }
}

/// A table of entry points fetched by name.
macro_rules! fn_table {
    ($(#[$m:meta])* $vis:vis $table:ident { $($field:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)* }) => {
        $(#[$m])*
        #[allow(non_snake_case)]
        $vis struct $table {
            $(pub $field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl $table {
            /// SAFETY: `get` must return either null or the entry point
            /// named, with the signature the specification gives it.
            unsafe fn load(get: impl Fn(&CStr) -> PFN_vkVoidFunction) -> Result<$table> {
                Ok($table {
                    $($field: {
                        let name = concat!(stringify!($field), "\0");
                        let cname = CStr::from_bytes_with_nul(name.as_bytes()).expect("literal");
                        let f = get(cname).ok_or(Error::Missing(stringify!($field)))?;
                        // SAFETY: per this function's contract, `f` has this signature.
                        unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn($($arg),*) $(-> $ret)?>(f) }
                    },)*
                })
            }
        }

        impl fmt::Debug for $table {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(stringify!($table))
            }
        }
    };
}

fn_table! {
    /// Global commands (no instance yet).
    EntryFns {
        vkCreateInstance: fn(*const VkInstanceCreateInfo, *const std::ffi::c_void, *mut VkInstance) -> VkResult;
        vkEnumerateInstanceVersion: fn(*mut u32) -> VkResult;
        vkEnumerateInstanceLayerProperties: fn(*mut u32, *mut VkLayerProperties) -> VkResult;
    }
}

fn_table! {
    InstanceFns {
        vkDestroyInstance: fn(VkInstance, *const std::ffi::c_void);
        vkEnumeratePhysicalDevices: fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult;
        vkGetPhysicalDeviceProperties2: fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties2);
        vkGetPhysicalDeviceQueueFamilyProperties: fn(VkPhysicalDevice, *mut u32, *mut VkQueueFamilyProperties);
        vkGetPhysicalDeviceMemoryProperties: fn(VkPhysicalDevice, *mut VkPhysicalDeviceMemoryProperties);
        vkEnumerateDeviceExtensionProperties: fn(VkPhysicalDevice, *const c_char, *mut u32, *mut VkExtensionProperties) -> VkResult;
        vkCreateDevice: fn(VkPhysicalDevice, *const VkDeviceCreateInfo, *const std::ffi::c_void, *mut VkDevice) -> VkResult;
        vkGetDeviceProcAddr: fn(VkDevice, *const c_char) -> PFN_vkVoidFunction;
        vkGetPhysicalDeviceFeatures2: fn(VkPhysicalDevice, *mut VkPhysicalDeviceFeatures2);
        vkGetPhysicalDeviceFormatProperties2: fn(VkPhysicalDevice, i32, *mut VkFormatProperties2);
        vkGetPhysicalDeviceImageFormatProperties2: fn(VkPhysicalDevice, *const VkPhysicalDeviceImageFormatInfo2, *mut VkImageFormatProperties2) -> VkResult;
    }
}

fn_table! {
    /// Device commands; `submit_once` hands them to its recorder.
    pub DeviceFns {
        vkDestroyDevice: fn(VkDevice, *const std::ffi::c_void);
        vkGetDeviceQueue: fn(VkDevice, u32, u32, *mut VkQueue);
        vkDeviceWaitIdle: fn(VkDevice) -> VkResult;
        vkCreateImage: fn(VkDevice, *const VkImageCreateInfo, *const std::ffi::c_void, *mut VkImage) -> VkResult;
        vkDestroyImage: fn(VkDevice, VkImage, *const std::ffi::c_void);
        vkGetImageMemoryRequirements: fn(VkDevice, VkImage, *mut VkMemoryRequirements);
        vkGetImageSubresourceLayout: fn(VkDevice, VkImage, *const VkImageSubresource, *mut VkSubresourceLayout);
        vkGetImageDrmFormatModifierPropertiesEXT: fn(VkDevice, VkImage, *mut VkImageDrmFormatModifierPropertiesEXT) -> VkResult;
        vkAllocateMemory: fn(VkDevice, *const VkMemoryAllocateInfo, *const std::ffi::c_void, *mut VkDeviceMemory) -> VkResult;
        vkFreeMemory: fn(VkDevice, VkDeviceMemory, *const std::ffi::c_void);
        vkBindImageMemory: fn(VkDevice, VkImage, VkDeviceMemory, VkDeviceSize) -> VkResult;
        vkGetMemoryFdKHR: fn(VkDevice, *const VkMemoryGetFdInfoKHR, *mut i32) -> VkResult;
        vkGetMemoryFdPropertiesKHR: fn(VkDevice, VkFlags, i32, *mut VkMemoryFdPropertiesKHR) -> VkResult;
        vkCreateCommandPool: fn(VkDevice, *const VkCommandPoolCreateInfo, *const std::ffi::c_void, *mut VkCommandPool) -> VkResult;
        vkDestroyCommandPool: fn(VkDevice, VkCommandPool, *const std::ffi::c_void);
        vkAllocateCommandBuffers: fn(VkDevice, *const VkCommandBufferAllocateInfo, *mut VkCommandBuffer) -> VkResult;
        vkBeginCommandBuffer: fn(VkCommandBuffer, *const VkCommandBufferBeginInfo) -> VkResult;
        vkEndCommandBuffer: fn(VkCommandBuffer) -> VkResult;
        vkCmdPipelineBarrier2: fn(VkCommandBuffer, *const VkDependencyInfo);
        vkCmdClearColorImage: fn(VkCommandBuffer, VkImage, i32, *const VkClearColorValue, u32, *const VkImageSubresourceRange);
        vkQueueSubmit2: fn(VkQueue, u32, *const VkSubmitInfo2, VkFence) -> VkResult;
        vkCreateFence: fn(VkDevice, *const VkFenceCreateInfo, *const std::ffi::c_void, *mut VkFence) -> VkResult;
        vkDestroyFence: fn(VkDevice, VkFence, *const std::ffi::c_void);
        vkWaitForFences: fn(VkDevice, u32, *const VkFence, VkBool32, u64) -> VkResult;
        vkCreateSemaphore: fn(VkDevice, *const VkSemaphoreCreateInfo, *const std::ffi::c_void, *mut VkSemaphore) -> VkResult;
        vkDestroySemaphore: fn(VkDevice, VkSemaphore, *const std::ffi::c_void);
        vkGetSemaphoreFdKHR: fn(VkDevice, *const VkSemaphoreGetFdInfoKHR, *mut i32) -> VkResult;
        vkResetFences: fn(VkDevice, u32, *const VkFence) -> VkResult;
        vkGetFenceStatus: fn(VkDevice, VkFence) -> VkResult;
        vkResetCommandBuffer: fn(VkCommandBuffer, VkFlags) -> VkResult;
        vkCreateBuffer: fn(VkDevice, *const VkBufferCreateInfo, *const std::ffi::c_void, *mut VkBuffer) -> VkResult;
        vkDestroyBuffer: fn(VkDevice, VkBuffer, *const std::ffi::c_void);
        vkGetBufferMemoryRequirements: fn(VkDevice, VkBuffer, *mut VkMemoryRequirements);
        vkBindBufferMemory: fn(VkDevice, VkBuffer, VkDeviceMemory, VkDeviceSize) -> VkResult;
        vkMapMemory: fn(VkDevice, VkDeviceMemory, VkDeviceSize, VkDeviceSize, VkFlags, *mut *mut std::ffi::c_void) -> VkResult;
        vkUnmapMemory: fn(VkDevice, VkDeviceMemory);
        vkCreateImageView: fn(VkDevice, *const VkImageViewCreateInfo, *const std::ffi::c_void, *mut VkImageView) -> VkResult;
        vkDestroyImageView: fn(VkDevice, VkImageView, *const std::ffi::c_void);
        vkCreateSampler: fn(VkDevice, *const VkSamplerCreateInfo, *const std::ffi::c_void, *mut VkSampler) -> VkResult;
        vkDestroySampler: fn(VkDevice, VkSampler, *const std::ffi::c_void);
        vkCreateDescriptorSetLayout: fn(VkDevice, *const VkDescriptorSetLayoutCreateInfo, *const std::ffi::c_void, *mut VkDescriptorSetLayout) -> VkResult;
        vkDestroyDescriptorSetLayout: fn(VkDevice, VkDescriptorSetLayout, *const std::ffi::c_void);
        vkCreateDescriptorPool: fn(VkDevice, *const VkDescriptorPoolCreateInfo, *const std::ffi::c_void, *mut VkDescriptorPool) -> VkResult;
        vkDestroyDescriptorPool: fn(VkDevice, VkDescriptorPool, *const std::ffi::c_void);
        vkAllocateDescriptorSets: fn(VkDevice, *const VkDescriptorSetAllocateInfo, *mut VkDescriptorSet) -> VkResult;
        vkFreeDescriptorSets: fn(VkDevice, VkDescriptorPool, u32, *const VkDescriptorSet) -> VkResult;
        vkUpdateDescriptorSets: fn(VkDevice, u32, *const VkWriteDescriptorSet, u32, *const std::ffi::c_void);
        vkCreatePipelineLayout: fn(VkDevice, *const VkPipelineLayoutCreateInfo, *const std::ffi::c_void, *mut VkPipelineLayout) -> VkResult;
        vkDestroyPipelineLayout: fn(VkDevice, VkPipelineLayout, *const std::ffi::c_void);
        vkCreateShaderModule: fn(VkDevice, *const VkShaderModuleCreateInfo, *const std::ffi::c_void, *mut VkShaderModule) -> VkResult;
        vkDestroyShaderModule: fn(VkDevice, VkShaderModule, *const std::ffi::c_void);
        vkCreateGraphicsPipelines: fn(VkDevice, u64, u32, *const VkGraphicsPipelineCreateInfo, *const std::ffi::c_void, *mut VkPipeline) -> VkResult;
        vkDestroyPipeline: fn(VkDevice, VkPipeline, *const std::ffi::c_void);
        vkCmdBeginRendering: fn(VkCommandBuffer, *const VkRenderingInfo);
        vkCmdEndRendering: fn(VkCommandBuffer);
        vkCmdBindPipeline: fn(VkCommandBuffer, i32, VkPipeline);
        vkCmdBindDescriptorSets: fn(VkCommandBuffer, i32, VkPipelineLayout, u32, u32, *const VkDescriptorSet, u32, *const u32);
        vkCmdBindVertexBuffers: fn(VkCommandBuffer, u32, u32, *const VkBuffer, *const VkDeviceSize);
        vkCmdPushConstants: fn(VkCommandBuffer, VkPipelineLayout, VkFlags, u32, u32, *const std::ffi::c_void);
        vkCmdSetViewport: fn(VkCommandBuffer, u32, u32, *const VkViewport);
        vkCmdSetScissor: fn(VkCommandBuffer, u32, u32, *const VkRect2D);
        vkCmdDraw: fn(VkCommandBuffer, u32, u32, u32, u32);
        vkCmdCopyBufferToImage: fn(VkCommandBuffer, VkBuffer, VkImage, i32, u32, *const VkBufferImageCopy);
    }
}

/// A loaded Vulkan instance.
#[derive(Debug)]
pub struct Instance {
    raw: VkInstance,
    fns: InstanceFns,
    /// The version the loader supports (≥ what we asked for).
    pub api_version: u32,
    pub validation: bool,
    _lib: Library,
}

/// Major, minor of a DRM device node.
pub type DevNode = (u64, u64);

/// Split a `dev_t` (as `zwp_linux_dmabuf_feedback_v1.main_device` sends it).
pub fn dev_node(dev: u64) -> DevNode {
    // As glibc's gnu_dev_major/minor.
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    (major, minor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceKind {
    Cpu,
    Other,
    Virtual,
    Integrated,
    Discrete,
}

/// What we learned about a physical device, for choosing one.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    raw: VkPhysicalDevice,
    pub index: usize,
    pub name: String,
    pub kind: DeviceKind,
    pub api_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    /// Driver name and version string ("Intel open-source Mesa driver", "Mesa 26.2.5").
    pub driver: (String, String),
    pub primary_node: Option<DevNode>,
    pub render_node: Option<DevNode>,
    /// A queue family that can do graphics.
    pub graphics_family: Option<u32>,
    pub device_local_bytes: u64,
    /// Property flags of each memory type, by index.
    pub memory_types: Vec<VkFlags>,
    /// Supports dynamic rendering and synchronization2 (core 1.3 features).
    pub features_ok: bool,
    /// Required extensions the device lacks.
    pub missing: Vec<&'static str>,
}

impl DeviceInfo {
    /// Why the device can't be used, if it can't.
    pub fn unsuitable(&self) -> Option<String> {
        if self.api_version < MIN_API_VERSION {
            Some(format!("{}: Vulkan {} < 1.3", self.name, version_string(self.api_version)))
        } else if self.graphics_family.is_none() {
            Some(format!("{}: no graphics queue", self.name))
        } else if !self.missing.is_empty() {
            Some(format!("{}: lacks {}", self.name, self.missing.join(", ")))
        } else if !self.features_ok {
            Some(format!("{}: lacks dynamic rendering or synchronization2", self.name))
        } else {
            None
        }
    }

    /// Whether `node` (from the compositor) is this device.
    pub fn is_node(&self, node: DevNode) -> bool {
        self.primary_node == Some(node) || self.render_node == Some(node)
    }
}

pub fn version_string(v: u32) -> String {
    format!("{}.{}.{}", (v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
}

/// How the user asked for a device (`TERMMD_VK_DEVICE`): an index, or part
/// of the name (case-insensitive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChoice {
    Index(usize),
    Name(String),
}

impl DeviceChoice {
    pub fn parse(s: &str) -> DeviceChoice {
        s.parse().map(DeviceChoice::Index).unwrap_or_else(|_| DeviceChoice::Name(s.to_lowercase()))
    }

    fn matches(&self, d: &DeviceInfo) -> bool {
        match self {
            DeviceChoice::Index(i) => d.index == *i,
            DeviceChoice::Name(n) => d.name.to_lowercase().contains(n),
        }
    }
}

/// Pick a device: the user's choice if usable; else the compositor's
/// device (so buffers need no cross-GPU copy); else discrete over
/// integrated over virtual. Software rasterisers are never picked
/// automatically: the CPU renderer beats them.
pub fn select<'a>(
    devices: &'a [DeviceInfo],
    choice: Option<&DeviceChoice>,
    compositor: Option<DevNode>,
) -> std::result::Result<&'a DeviceInfo, Vec<String>> {
    let usable = |d: &&DeviceInfo| d.unsuitable().is_none();
    if let Some(c) = choice
        && let Some(d) = devices.iter().filter(usable).find(|d| c.matches(d))
    {
        return Ok(d);
    }
    if let Some(node) = compositor
        && let Some(d) = devices.iter().filter(usable).find(|d| d.is_node(node))
    {
        return Ok(d);
    }
    devices
        .iter()
        .filter(usable)
        .filter(|d| d.kind != DeviceKind::Cpu)
        .max_by_key(|d| (d.kind, d.device_local_bytes))
        .ok_or_else(|| {
            devices.iter().map(|d| d.unsuitable().unwrap_or_else(|| format!("{}: software renderer", d.name))).collect()
        })
}

impl Instance {
    /// Load the Vulkan loader and create an instance. With `validation`,
    /// enable the Khronos validation layer if it is installed.
    pub fn new(app_name: &CStr, validation: bool) -> Result<Rc<Instance>> {
        let lib = Library::open(c"libvulkan.so.1").map_err(Error::Load)?;
        let gipa = lib.symbol(c"vkGetInstanceProcAddr").ok_or(Error::Missing("vkGetInstanceProcAddr"))?;
        // SAFETY: the loader exports vkGetInstanceProcAddr with this signature.
        let gipa: PFN_vkGetInstanceProcAddr = unsafe { std::mem::transmute(gipa.as_ptr()) };
        // SAFETY: gipa with a null instance returns global commands or null.
        let entry = unsafe { EntryFns::load(|name| gipa(ptr::null_mut(), name.as_ptr())) }?;

        let mut api_version = 0;
        // SAFETY: valid out pointer.
        check("vkEnumerateInstanceVersion", unsafe { (entry.vkEnumerateInstanceVersion)(&mut api_version) })?;
        if api_version < MIN_API_VERSION {
            return Err(Error::NoDevice(vec![format!("loader supports only Vulkan {}", version_string(api_version))]));
        }

        let validation_layer = c"VK_LAYER_KHRONOS_validation";
        let validation = validation && {
            // SAFETY: two-call enumeration with valid pointers.
            let layers = enumerate("vkEnumerateInstanceLayerProperties", zeroed_layer(), |n, p| unsafe {
                (entry.vkEnumerateInstanceLayerProperties)(n, p)
            })?;
            layers.iter().any(|l| fixed_str(&l.layerName).as_bytes() == validation_layer.to_bytes())
        };
        let layers = [validation_layer.as_ptr()];

        let app = VkApplicationInfo {
            sType: VK_STRUCTURE_TYPE_APPLICATION_INFO,
            pNext: ptr::null(),
            pApplicationName: app_name.as_ptr(),
            applicationVersion: 0,
            pEngineName: c"termmd".as_ptr(),
            engineVersion: 0,
            apiVersion: MIN_API_VERSION,
        };
        let info = VkInstanceCreateInfo {
            sType: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            pApplicationInfo: &app,
            enabledLayerCount: validation as u32,
            ppEnabledLayerNames: layers.as_ptr(),
            enabledExtensionCount: 0,
            ppEnabledExtensionNames: ptr::null(),
        };
        let mut raw = ptr::null_mut();
        // SAFETY: `info` and everything it points to outlive the call.
        check("vkCreateInstance", unsafe { (entry.vkCreateInstance)(&info, ptr::null(), &mut raw) })?;
        // SAFETY: gipa with our instance returns that instance's commands or null.
        // (On failure the instance leaks: without vkDestroyInstance there is
        // no way to free it.)
        let fns = unsafe { InstanceFns::load(|name| gipa(raw, name.as_ptr())) }?;
        Ok(Rc::new(Instance { raw, fns, api_version, validation, _lib: lib }))
    }

    /// Every physical device, described.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let f = &self.fns;
        // SAFETY: two-call enumeration with valid pointers.
        let raws = enumerate("vkEnumeratePhysicalDevices", ptr::null_mut(), |n, p| unsafe {
            (f.vkEnumeratePhysicalDevices)(self.raw, n, p)
        })?;
        raws.into_iter().enumerate().map(|(index, raw)| self.describe(index, raw)).collect()
    }

    fn describe(&self, index: usize, raw: VkPhysicalDevice) -> Result<DeviceInfo> {
        let f = &self.fns;
        // SAFETY: two-call enumeration with valid pointers.
        let exts = enumerate("vkEnumerateDeviceExtensionProperties", zeroed_ext(), |n, p| unsafe {
            (f.vkEnumerateDeviceExtensionProperties)(raw, ptr::null(), n, p)
        })?;
        let ext_names: Vec<String> = exts.iter().map(|e| fixed_str(&e.extensionName)).collect();
        let has = |e: &CStr| ext_names.iter().any(|n| n.as_bytes() == e.to_bytes());

        // Properties, with driver and (if supported) DRM node info chained.
        // SAFETY: plain-old-data structs; all-zero is a valid bit pattern.
        let mut drm: VkPhysicalDeviceDrmPropertiesEXT = unsafe { std::mem::zeroed() };
        drm.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRM_PROPERTIES_EXT;
        // SAFETY: as above.
        let mut driver: VkPhysicalDeviceDriverProperties = unsafe { std::mem::zeroed() };
        driver.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES;
        let has_drm = has(c"VK_EXT_physical_device_drm");
        if has_drm {
            driver.pNext = (&mut drm as *mut VkPhysicalDeviceDrmPropertiesEXT).cast();
        }
        // SAFETY: as above.
        let mut props: VkPhysicalDeviceProperties2 = unsafe { std::mem::zeroed() };
        props.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
        props.pNext = (&mut driver as *mut VkPhysicalDeviceDriverProperties).cast();
        // SAFETY: the chain is well-formed and lives across the call; driver
        // properties are core in 1.2, DRM ones only chained when supported.
        unsafe { (f.vkGetPhysicalDeviceProperties2)(raw, &mut props) };
        let p = &props.properties;

        // SAFETY: two-call enumeration with valid pointers (cannot fail).
        let families = enumerate("vkGetPhysicalDeviceQueueFamilyProperties", Default::default(), |n, ptr| unsafe {
            (f.vkGetPhysicalDeviceQueueFamilyProperties)(raw, n, ptr);
            VK_SUCCESS
        })?;
        let graphics_family = families
            .iter()
            .position(|q: &VkQueueFamilyProperties| q.queueFlags & VK_QUEUE_GRAPHICS_BIT != 0 && q.queueCount > 0)
            .map(|i| i as u32);

        // SAFETY: as above.
        let mut mem: VkPhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointer.
        unsafe { (f.vkGetPhysicalDeviceMemoryProperties)(raw, &mut mem) };
        let heaps = &mem.memoryHeaps[..(mem.memoryHeapCount as usize).min(VK_MAX_MEMORY_HEAPS)];
        let device_local_bytes =
            heaps.iter().filter(|h| h.flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT != 0).map(|h| h.size).sum();

        // SAFETY: as above.
        let mut v13: VkPhysicalDeviceVulkan13Features = unsafe { std::mem::zeroed() };
        v13.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES;
        // SAFETY: as above.
        let mut features: VkPhysicalDeviceFeatures2 = unsafe { std::mem::zeroed() };
        features.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
        features.pNext = (&mut v13 as *mut VkPhysicalDeviceVulkan13Features).cast();
        // The 1.3 feature struct may only be chained on 1.3 devices.
        if p.apiVersion >= MIN_API_VERSION {
            // SAFETY: well-formed chain living across the call.
            unsafe { (f.vkGetPhysicalDeviceFeatures2)(raw, &mut features) };
        }

        let node = |has: VkBool32, major: i64, minor: i64| (has != 0).then_some((major as u64, minor as u64));
        Ok(DeviceInfo {
            raw,
            index,
            name: fixed_str(&p.deviceName),
            kind: match p.deviceType {
                VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU => DeviceKind::Discrete,
                VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU => DeviceKind::Integrated,
                VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU => DeviceKind::Virtual,
                VK_PHYSICAL_DEVICE_TYPE_CPU => DeviceKind::Cpu,
                _ => DeviceKind::Other,
            },
            api_version: p.apiVersion,
            vendor_id: p.vendorID,
            device_id: p.deviceID,
            driver: (fixed_str(&driver.driverName), fixed_str(&driver.driverInfo)),
            primary_node: if has_drm { node(drm.hasPrimary, drm.primaryMajor, drm.primaryMinor) } else { None },
            render_node: if has_drm { node(drm.hasRender, drm.renderMajor, drm.renderMinor) } else { None },
            graphics_family,
            device_local_bytes,
            memory_types: mem.memoryTypes[..(mem.memoryTypeCount as usize).min(VK_MAX_MEMORY_TYPES)]
                .iter()
                .map(|t| t.propertyFlags)
                .collect(),
            features_ok: v13.dynamicRendering != 0 && v13.synchronization2 != 0,
            missing: REQUIRED_DEVICE_EXTENSIONS
                .iter()
                .filter(|e| !has(e))
                .map(|e| e.to_str().expect("ASCII"))
                .collect(),
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: every Device holds an Rc to us, so none outlives this.
        unsafe { (self.fns.vkDestroyInstance)(self.raw, ptr::null()) };
    }
}

fn zeroed_ext() -> VkExtensionProperties {
    VkExtensionProperties { extensionName: [0; VK_MAX_EXTENSION_NAME_SIZE], specVersion: 0 }
}

fn zeroed_layer() -> VkLayerProperties {
    VkLayerProperties {
        layerName: [0; VK_MAX_EXTENSION_NAME_SIZE],
        specVersion: 0,
        implementationVersion: 0,
        description: [0; VK_MAX_DESCRIPTION_SIZE],
    }
}

/// An opened logical device with one graphics queue.
#[derive(Debug)]
pub struct Device {
    /// The device handle, for calls through [`fns`](Self::fns).
    pub raw: VkDevice,
    /// The device's entry points: what an app draws with.
    pub fns: DeviceFns,
    pub queue: VkQueue,
    pub info: DeviceInfo,
    instance: Rc<Instance>,
}

impl Device {
    /// Open `info` (from `instance.devices()`; it must be suitable).
    pub fn open(instance: &Rc<Instance>, info: &DeviceInfo) -> Result<Device> {
        if let Some(why) = info.unsuitable() {
            return Err(Error::NoDevice(vec![why]));
        }
        let family = info.graphics_family.expect("checked by unsuitable");
        let priority = 1.0f32;
        let queue_info = VkDeviceQueueCreateInfo {
            sType: VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
            pNext: ptr::null(),
            flags: 0,
            queueFamilyIndex: family,
            queueCount: 1,
            pQueuePriorities: &priority,
        };
        let exts: Vec<*const c_char> = REQUIRED_DEVICE_EXTENSIONS.iter().map(|e| e.as_ptr()).collect();
        // SAFETY: plain-old-data; all-zero is valid (every feature off).
        let mut v13: VkPhysicalDeviceVulkan13Features = unsafe { std::mem::zeroed() };
        v13.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES;
        v13.dynamicRendering = 1;
        v13.synchronization2 = 1;
        let create = VkDeviceCreateInfo {
            sType: VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
            pNext: (&v13 as *const VkPhysicalDeviceVulkan13Features).cast(),
            flags: 0,
            queueCreateInfoCount: 1,
            pQueueCreateInfos: &queue_info,
            enabledLayerCount: 0,
            ppEnabledLayerNames: ptr::null(),
            enabledExtensionCount: exts.len() as u32,
            ppEnabledExtensionNames: exts.as_ptr(),
            pEnabledFeatures: ptr::null(),
        };
        let f = &instance.fns;
        let mut raw = ptr::null_mut();
        // SAFETY: `create` and what it points to outlive the call; the
        // physical device belongs to this instance.
        check("vkCreateDevice", unsafe { (f.vkCreateDevice)(info.raw, &create, ptr::null(), &mut raw) })?;
        let gdpa = f.vkGetDeviceProcAddr;
        // SAFETY: gdpa with our device returns its commands or null.
        let fns = unsafe { DeviceFns::load(|name| gdpa(raw, name.as_ptr())) }?;
        let mut queue = ptr::null_mut();
        // SAFETY: family and index 0 were requested at creation.
        unsafe { (fns.vkGetDeviceQueue)(raw, family, 0, &mut queue) };
        Ok(Device { raw, fns, queue, info: info.clone(), instance: instance.clone() })
    }

    pub fn instance(&self) -> &Rc<Instance> {
        &self.instance
    }

    /// A memory type allowed by `bits` with all of `flags`, if any.
    pub fn memory_type(&self, bits: u32, flags: VkFlags) -> Option<u32> {
        self.info
            .memory_types
            .iter()
            .enumerate()
            .find(|&(i, &f)| bits & (1 << i) != 0 && f & flags == flags)
            .map(|(i, _)| i as u32)
    }

    /// Record commands with `record`, submit them and wait for completion.
    /// For setup and tests; frames use their own command buffers.
    pub fn submit_once(&self, record: impl FnOnce(&DeviceFns, VkCommandBuffer)) -> Result<()> {
        self.submit_once_signal(record, None)
    }

    /// `submit_once`, also signalling `signal` when the work completes.
    pub fn submit_once_signal(
        &self,
        record: impl FnOnce(&DeviceFns, VkCommandBuffer),
        signal: Option<VkSemaphore>,
    ) -> Result<()> {
        let f = &self.fns;
        let family = self.info.graphics_family.expect("opened devices have one");
        let pool_info = VkCommandPoolCreateInfo {
            sType: VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
            pNext: ptr::null(),
            flags: VK_COMMAND_POOL_CREATE_TRANSIENT_BIT,
            queueFamilyIndex: family,
        };
        let mut pool = VK_NULL_HANDLE;
        // SAFETY: valid device and create info.
        check("vkCreateCommandPool", unsafe { (f.vkCreateCommandPool)(self.raw, &pool_info, ptr::null(), &mut pool) })?;
        let result = (|| {
            let alloc = VkCommandBufferAllocateInfo {
                sType: VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                pNext: ptr::null(),
                commandPool: pool,
                level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                commandBufferCount: 1,
            };
            let mut cmd = ptr::null_mut();
            // SAFETY: one buffer from our pool into `cmd`.
            check("vkAllocateCommandBuffers", unsafe { (f.vkAllocateCommandBuffers)(self.raw, &alloc, &mut cmd) })?;
            let begin = VkCommandBufferBeginInfo {
                sType: VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                pNext: ptr::null(),
                flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
                pInheritanceInfo: ptr::null(),
            };
            // SAFETY: a fresh primary command buffer.
            check("vkBeginCommandBuffer", unsafe { (f.vkBeginCommandBuffer)(cmd, &begin) })?;
            record(f, cmd);
            // SAFETY: the buffer is recording.
            check("vkEndCommandBuffer", unsafe { (f.vkEndCommandBuffer)(cmd) })?;
            let fence_info =
                VkFenceCreateInfo { sType: VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, pNext: ptr::null(), flags: 0 };
            let mut fence = VK_NULL_HANDLE;
            // SAFETY: valid device and create info.
            check("vkCreateFence", unsafe { (f.vkCreateFence)(self.raw, &fence_info, ptr::null(), &mut fence) })?;
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
            // SAFETY: the recorded buffer, our queue and an unsignalled fence.
            let r =
                check("vkQueueSubmit2", unsafe { (f.vkQueueSubmit2)(self.queue, 1, &submit, fence) }).and_then(|()| {
                    // SAFETY: waiting on our fence.
                    check("vkWaitForFences", unsafe { (f.vkWaitForFences)(self.raw, 1, &fence, 1, u64::MAX) })
                });
            // SAFETY: the fence is signalled (or the submit failed), so unused.
            unsafe { (f.vkDestroyFence)(self.raw, fence, ptr::null()) };
            r
        })();
        // SAFETY: the work has completed (or never started); frees its buffers too.
        unsafe { (f.vkDestroyCommandPool)(self.raw, pool, ptr::null()) };
        result
    }

    pub fn wait_idle(&self) -> Result<()> {
        // SAFETY: valid device; no other thread uses it.
        check("vkDeviceWaitIdle", unsafe { (self.fns.vkDeviceWaitIdle)(self.raw) })
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: valid device; waiting first so nothing is in flight.
        unsafe {
            (self.fns.vkDeviceWaitIdle)(self.raw);
            (self.fns.vkDestroyDevice)(self.raw, ptr::null());
        }
    }
}

/// Create an instance and open the best device. `TERMMD_VK_DEVICE` picks
/// one by index or name; `TERMMD_VK_VALIDATION` enables validation.
pub fn open_default(compositor: Option<DevNode>) -> Result<Device> {
    let validation = std::env::var_os("TERMMD_VK_VALIDATION").is_some();
    let instance = Instance::new(c"termmd", validation)?;
    let devices = instance.devices()?;
    let choice = std::env::var("TERMMD_VK_DEVICE").ok().map(|s| DeviceChoice::parse(&s));
    let info = select(&devices, choice.as_ref(), compositor).map_err(Error::NoDevice)?;
    Device::open(&instance, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(index: usize, name: &str, kind: DeviceKind) -> DeviceInfo {
        DeviceInfo {
            raw: ptr::null_mut(),
            index,
            name: name.into(),
            kind,
            api_version: vk_make_api_version(0, 1, 4, 0),
            vendor_id: 0,
            device_id: 0,
            driver: Default::default(),
            primary_node: Some((226, index as u64)),
            render_node: Some((226, 128 + index as u64)),
            graphics_family: Some(0),
            device_local_bytes: 0,
            memory_types: vec![VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT],
            features_ok: true,
            missing: Vec::new(),
        }
    }

    #[test]
    fn selection_order() {
        let mut old = dev(3, "Old", DeviceKind::Discrete);
        old.api_version = vk_make_api_version(0, 1, 2, 0);
        let devices = [
            dev(0, "llvmpipe", DeviceKind::Cpu),
            dev(1, "Intel Iris Xe", DeviceKind::Integrated),
            dev(2, "NVIDIA RTX", DeviceKind::Discrete),
            old,
        ];
        let pick = |c: Option<DeviceChoice>, n: Option<DevNode>| select(&devices, c.as_ref(), n).map(|d| d.index);
        assert_eq!(pick(None, None), Ok(2), "discrete over integrated; too-old devices skipped");
        assert_eq!(pick(None, Some((226, 129))), Ok(1), "the compositor's render node");
        assert_eq!(pick(None, Some((226, 1))), Ok(1), "or its primary node");
        assert_eq!(pick(Some(DeviceChoice::parse("iris")), Some((226, 2))), Ok(1), "the user's choice wins");
        assert_eq!(pick(Some(DeviceChoice::parse("0")), None), Ok(0), "even a software renderer");
        assert_eq!(pick(Some(DeviceChoice::parse("3")), None), Ok(2), "but not an unusable device");

        let only_soft = [dev(0, "llvmpipe", DeviceKind::Cpu)];
        assert_eq!(select(&only_soft, None, None).unwrap_err(), ["llvmpipe: software renderer"]);
        let mut lacking = dev(0, "Weak", DeviceKind::Integrated);
        lacking.missing = vec!["VK_EXT_image_drm_format_modifier"];
        assert_eq!(select(&[lacking], None, None).unwrap_err(), ["Weak: lacks VK_EXT_image_drm_format_modifier"]);
    }

    #[test]
    fn dev_t_split() {
        // makedev(226, 128) as glibc encodes it.
        assert_eq!(dev_node((226 << 8) | 128), (226, 128));
        let big: u64 = ((0x1234u64 & !0xfff) << 32) | ((0x1234 & 0xfff) << 8) | ((0x56789u64 & !0xff) << 12) | 0x89;
        assert_eq!(dev_node(big), (0x1234, 0x56789));
    }

    /// Needs a Vulkan driver; run with `--ignored`.
    #[test]
    #[ignore]
    fn opens_the_system_device() {
        let instance = Instance::new(c"termmd-test", false).expect("instance");
        let devices = instance.devices().expect("devices");
        assert!(!devices.is_empty());
        for d in &devices {
            eprintln!("{d:?}");
            assert!(!d.name.is_empty());
        }
        let device = open_default(None).expect("device");
        assert!(!device.queue.is_null());
        device.wait_idle().unwrap();
    }
}
