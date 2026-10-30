//! The part of the Vulkan 1.0 API this crate uses, written after
//! `vulkan_core.h`: handles, the structures (`#[repr(C)]`, same layout
//! as in C), constants and tables of function pointers.
//!
//! Fields that are not used are grouped into arrays of the same size and
//! alignment, so the layout stays that of the C structure. The tests at
//! the end compare sizes and offsets with the C compiler's (64-bit).
//!
//! Calling convention: `extern "system"` is Vulkan's `VKAPI_CALL`
//! (`stdcall` on 32-bit Windows, C elsewhere). 32-bit Android declares
//! the functions with the hard-float ABI; none used here passes or
//! returns floating-point values, so both ABIs agree.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void, CStr};

// Dispatchable handles are pointers; the others are 64-bit on all
// platforms.
pub type Instance = *mut c_void;
pub type PhysicalDevice = *mut c_void;
pub type Device = *mut c_void;
pub type Queue = *mut c_void;
pub type CommandBuffer = *mut c_void;
pub type Handle = u64;
pub const NULL_HANDLE: Handle = 0;

pub type Result = i32;
pub const SUCCESS: Result = 0;
pub const INCOMPLETE: Result = 5;

pub const API_VERSION_1_0: u32 = 1 << 22;

// Structure types.
pub const ST_APPLICATION_INFO: i32 = 0;
pub const ST_INSTANCE_CREATE_INFO: i32 = 1;
pub const ST_DEVICE_QUEUE_CREATE_INFO: i32 = 2;
pub const ST_DEVICE_CREATE_INFO: i32 = 3;
pub const ST_SUBMIT_INFO: i32 = 4;
pub const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
pub const ST_FENCE_CREATE_INFO: i32 = 8;
pub const ST_BUFFER_CREATE_INFO: i32 = 12;
pub const ST_SHADER_MODULE_CREATE_INFO: i32 = 16;
pub const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: i32 = 18;
pub const ST_COMPUTE_PIPELINE_CREATE_INFO: i32 = 29;
pub const ST_PIPELINE_LAYOUT_CREATE_INFO: i32 = 30;
pub const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: i32 = 32;
pub const ST_DESCRIPTOR_POOL_CREATE_INFO: i32 = 33;
pub const ST_DESCRIPTOR_SET_ALLOCATE_INFO: i32 = 34;
pub const ST_WRITE_DESCRIPTOR_SET: i32 = 35;
pub const ST_COMMAND_POOL_CREATE_INFO: i32 = 39;
pub const ST_COMMAND_BUFFER_ALLOCATE_INFO: i32 = 40;
pub const ST_COMMAND_BUFFER_BEGIN_INFO: i32 = 42;
pub const ST_MEMORY_BARRIER: i32 = 46;

pub const PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU: i32 = 1;
pub const PHYSICAL_DEVICE_TYPE_DISCRETE_GPU: i32 = 2;
pub const PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU: i32 = 3;
pub const PHYSICAL_DEVICE_TYPE_CPU: i32 = 4;

pub const QUEUE_COMPUTE_BIT: u32 = 0x2;

pub const MEMORY_PROPERTY_DEVICE_LOCAL_BIT: u32 = 0x1;
pub const MEMORY_PROPERTY_HOST_VISIBLE_BIT: u32 = 0x2;
pub const MEMORY_PROPERTY_HOST_COHERENT_BIT: u32 = 0x4;
pub const MEMORY_PROPERTY_HOST_CACHED_BIT: u32 = 0x8;

pub const BUFFER_USAGE_TRANSFER_SRC_BIT: u32 = 0x1;
pub const BUFFER_USAGE_TRANSFER_DST_BIT: u32 = 0x2;
pub const BUFFER_USAGE_STORAGE_BUFFER_BIT: u32 = 0x20;
pub const SHARING_MODE_EXCLUSIVE: i32 = 0;

pub const DESCRIPTOR_TYPE_STORAGE_BUFFER: i32 = 7;
pub const SHADER_STAGE_COMPUTE_BIT: u32 = 0x20;
pub const PIPELINE_BIND_POINT_COMPUTE: i32 = 1;
pub const COMMAND_BUFFER_LEVEL_PRIMARY: i32 = 0;
pub const COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: u32 = 0x1;

pub const PIPELINE_STAGE_COMPUTE_SHADER_BIT: u32 = 0x800;
pub const PIPELINE_STAGE_TRANSFER_BIT: u32 = 0x1000;
pub const PIPELINE_STAGE_HOST_BIT: u32 = 0x4000;

pub const ACCESS_SHADER_READ_BIT: u32 = 0x20;
pub const ACCESS_SHADER_WRITE_BIT: u32 = 0x40;
pub const ACCESS_TRANSFER_READ_BIT: u32 = 0x800;
pub const ACCESS_TRANSFER_WRITE_BIT: u32 = 0x1000;
pub const ACCESS_HOST_READ_BIT: u32 = 0x2000;

#[repr(C)]
pub struct ApplicationInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub application_name: *const c_char,
    pub application_version: u32,
    pub engine_name: *const c_char,
    pub engine_version: u32,
    pub api_version: u32,
}

#[repr(C)]
pub struct InstanceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub application_info: *const ApplicationInfo,
    pub enabled_layer_count: u32,
    pub enabled_layer_names: *const *const c_char,
    pub enabled_extension_count: u32,
    pub enabled_extension_names: *const *const c_char,
}

/// `VkPhysicalDeviceLimits`; the comments name the first and last field
/// of each group.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDeviceLimits {
    /// maxImageDimension1D .. maxUniformBufferRange
    pub _a: [u32; 7],
    pub max_storage_buffer_range: u32,
    pub max_push_constants_size: u32,
    pub max_memory_allocation_count: u32,
    /// maxSamplerAllocationCount
    pub _b: u32,
    /// bufferImageGranularity, sparseAddressSpaceSize
    pub _c: [u64; 2],
    /// maxBoundDescriptorSets .. maxFragmentCombinedOutputResources
    pub _d: [u32; 38],
    pub max_compute_shared_memory_size: u32,
    pub max_compute_work_group_count: [u32; 3],
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size: [u32; 3],
    /// subPixelPrecisionBits .. viewportSubPixelBits
    pub _e: [u32; 13],
    /// minMemoryMapAlignment
    pub _f: usize,
    /// minTexelBufferOffsetAlignment .. minStorageBufferOffsetAlignment
    pub _g: [u64; 3],
    /// minTexelOffset .. standardSampleLocations
    pub _h: [u32; 35],
    /// optimalBufferCopyOffsetAlignment .. nonCoherentAtomSize
    pub _i: [u64; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDeviceProperties {
    pub api_version: u32,
    pub driver_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: i32,
    pub device_name: [u8; 256],
    pub pipeline_cache_uuid: [u8; 16],
    pub limits: PhysicalDeviceLimits,
    /// VkPhysicalDeviceSparseProperties (five VkBool32).
    pub sparse_properties: [u32; 5],
}

/// `VkPhysicalDeviceFeatures`: 55 VkBool32, the first is
/// robustBufferAccess.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDeviceFeatures(pub [u32; 55]);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct QueueFamilyProperties {
    pub queue_flags: u32,
    pub queue_count: u32,
    pub timestamp_valid_bits: u32,
    pub min_image_transfer_granularity: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryType {
    pub property_flags: u32,
    pub heap_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryHeap {
    pub size: u64,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PhysicalDeviceMemoryProperties {
    pub memory_type_count: u32,
    pub memory_types: [MemoryType; 32],
    pub memory_heap_count: u32,
    pub memory_heaps: [MemoryHeap; 16],
}

#[repr(C)]
pub struct DeviceQueueCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub queue_family_index: u32,
    pub queue_count: u32,
    pub queue_priorities: *const f32,
}

#[repr(C)]
pub struct DeviceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub queue_create_info_count: u32,
    pub queue_create_infos: *const DeviceQueueCreateInfo,
    pub enabled_layer_count: u32,
    pub enabled_layer_names: *const *const c_char,
    pub enabled_extension_count: u32,
    pub enabled_extension_names: *const *const c_char,
    pub enabled_features: *const PhysicalDeviceFeatures,
}

#[repr(C)]
pub struct BufferCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub size: u64,
    pub usage: u32,
    pub sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub queue_family_indices: *const u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MemoryRequirements {
    pub size: u64,
    pub alignment: u64,
    pub memory_type_bits: u32,
}

#[repr(C)]
pub struct MemoryAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub allocation_size: u64,
    pub memory_type_index: u32,
}

#[repr(C)]
pub struct ShaderModuleCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub code_size: usize,
    pub code: *const u32,
}

#[repr(C)]
pub struct DescriptorSetLayoutBinding {
    pub binding: u32,
    pub descriptor_type: i32,
    pub descriptor_count: u32,
    pub stage_flags: u32,
    pub immutable_samplers: *const Handle,
}

#[repr(C)]
pub struct DescriptorSetLayoutCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub binding_count: u32,
    pub bindings: *const DescriptorSetLayoutBinding,
}

#[repr(C)]
pub struct PushConstantRange {
    pub stage_flags: u32,
    pub offset: u32,
    pub size: u32,
}

#[repr(C)]
pub struct PipelineLayoutCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub set_layout_count: u32,
    pub set_layouts: *const Handle,
    pub push_constant_range_count: u32,
    pub push_constant_ranges: *const PushConstantRange,
}

#[repr(C)]
pub struct PipelineShaderStageCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub stage: u32,
    pub module: Handle,
    pub name: *const c_char,
    pub specialization_info: *const c_void,
}

#[repr(C)]
pub struct ComputePipelineCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub stage: PipelineShaderStageCreateInfo,
    pub layout: Handle,
    pub base_pipeline_handle: Handle,
    pub base_pipeline_index: i32,
}

#[repr(C)]
pub struct DescriptorPoolSize {
    pub descriptor_type: i32,
    pub descriptor_count: u32,
}

#[repr(C)]
pub struct DescriptorPoolCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub max_sets: u32,
    pub pool_size_count: u32,
    pub pool_sizes: *const DescriptorPoolSize,
}

#[repr(C)]
pub struct DescriptorSetAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub descriptor_pool: Handle,
    pub descriptor_set_count: u32,
    pub set_layouts: *const Handle,
}

#[repr(C)]
pub struct DescriptorBufferInfo {
    pub buffer: Handle,
    pub offset: u64,
    pub range: u64,
}

#[repr(C)]
pub struct WriteDescriptorSet {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub dst_set: Handle,
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    pub descriptor_type: i32,
    pub image_info: *const c_void,
    pub buffer_info: *const DescriptorBufferInfo,
    pub texel_buffer_view: *const Handle,
}

#[repr(C)]
pub struct CommandPoolCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub queue_family_index: u32,
}

#[repr(C)]
pub struct CommandBufferAllocateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub command_pool: Handle,
    pub level: i32,
    pub command_buffer_count: u32,
}

#[repr(C)]
pub struct CommandBufferBeginInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
    pub inheritance_info: *const c_void,
}

#[repr(C)]
pub struct MemoryBarrier {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub src_access_mask: u32,
    pub dst_access_mask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BufferCopy {
    pub src_offset: u64,
    pub dst_offset: u64,
    pub size: u64,
}

#[repr(C)]
pub struct SubmitInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub wait_semaphore_count: u32,
    pub wait_semaphores: *const Handle,
    pub wait_dst_stage_mask: *const u32,
    pub command_buffer_count: u32,
    pub command_buffers: *const CommandBuffer,
    pub signal_semaphore_count: u32,
    pub signal_semaphores: *const Handle,
}

#[repr(C)]
pub struct FenceCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: u32,
}

/// `PFN_vkVoidFunction`: what the loader hands out before the cast to
/// the real signature.
pub type VoidFn = unsafe extern "system" fn();

pub type GetInstanceProcAddr =
    unsafe extern "system" fn(instance: Instance, name: *const c_char) -> Option<VoidFn>;

/// Declares a table of Vulkan functions, loaded by name.
macro_rules! functions {
    ($table:ident { $( $field:ident = $sym:literal : fn( $($arg:ty),* ) $(-> $ret:ty)? ; )* }) => {
        pub struct $table {
            $( pub $field: unsafe extern "system" fn($($arg),*) $(-> $ret)?, )*
        }

        impl $table {
            /// Looks up every function with `get`; `None` if one is
            /// missing.
            ///
            /// # Safety
            /// `get` must return, for each name, null or the address of
            /// that Vulkan function (whose C signature the table repeats).
            pub unsafe fn load(get: &mut dyn FnMut(&CStr) -> Option<VoidFn>) -> Option<$table> {
                Some($table {
                    $( $field: {
                        let f = get($sym)?;
                        // SAFETY: by the caller's promise `f` is the named
                        // function, whose signature this is.
                        unsafe {
                            std::mem::transmute::<VoidFn, unsafe extern "system" fn($($arg),*) $(-> $ret)?>(f)
                        }
                    }, )*
                })
            }
        }
    };
}

functions! {
    EntryFns {
        create_instance = c"vkCreateInstance":
            fn(*const InstanceCreateInfo, *const c_void, *mut Instance) -> Result;
    }
}

functions! {
    InstanceFns {
        destroy_instance = c"vkDestroyInstance": fn(Instance, *const c_void);
        enumerate_physical_devices = c"vkEnumeratePhysicalDevices":
            fn(Instance, *mut u32, *mut PhysicalDevice) -> Result;
        get_physical_device_properties = c"vkGetPhysicalDeviceProperties":
            fn(PhysicalDevice, *mut PhysicalDeviceProperties);
        get_physical_device_features = c"vkGetPhysicalDeviceFeatures":
            fn(PhysicalDevice, *mut PhysicalDeviceFeatures);
        get_physical_device_queue_family_properties = c"vkGetPhysicalDeviceQueueFamilyProperties":
            fn(PhysicalDevice, *mut u32, *mut QueueFamilyProperties);
        get_physical_device_memory_properties = c"vkGetPhysicalDeviceMemoryProperties":
            fn(PhysicalDevice, *mut PhysicalDeviceMemoryProperties);
        create_device = c"vkCreateDevice":
            fn(PhysicalDevice, *const DeviceCreateInfo, *const c_void, *mut Device) -> Result;
        get_device_proc_addr = c"vkGetDeviceProcAddr":
            fn(Device, *const c_char) -> Option<VoidFn>;
    }
}

functions! {
    DeviceFns {
        destroy_device = c"vkDestroyDevice": fn(Device, *const c_void);
        get_device_queue = c"vkGetDeviceQueue": fn(Device, u32, u32, *mut Queue);
        device_wait_idle = c"vkDeviceWaitIdle": fn(Device) -> Result;
        create_buffer = c"vkCreateBuffer":
            fn(Device, *const BufferCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_buffer = c"vkDestroyBuffer": fn(Device, Handle, *const c_void);
        get_buffer_memory_requirements = c"vkGetBufferMemoryRequirements":
            fn(Device, Handle, *mut MemoryRequirements);
        allocate_memory = c"vkAllocateMemory":
            fn(Device, *const MemoryAllocateInfo, *const c_void, *mut Handle) -> Result;
        free_memory = c"vkFreeMemory": fn(Device, Handle, *const c_void);
        bind_buffer_memory = c"vkBindBufferMemory": fn(Device, Handle, Handle, u64) -> Result;
        map_memory = c"vkMapMemory":
            fn(Device, Handle, u64, u64, u32, *mut *mut c_void) -> Result;
        create_shader_module = c"vkCreateShaderModule":
            fn(Device, *const ShaderModuleCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_shader_module = c"vkDestroyShaderModule": fn(Device, Handle, *const c_void);
        create_descriptor_set_layout = c"vkCreateDescriptorSetLayout":
            fn(Device, *const DescriptorSetLayoutCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_descriptor_set_layout = c"vkDestroyDescriptorSetLayout":
            fn(Device, Handle, *const c_void);
        create_pipeline_layout = c"vkCreatePipelineLayout":
            fn(Device, *const PipelineLayoutCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_pipeline_layout = c"vkDestroyPipelineLayout": fn(Device, Handle, *const c_void);
        create_compute_pipelines = c"vkCreateComputePipelines":
            fn(Device, Handle, u32, *const ComputePipelineCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_pipeline = c"vkDestroyPipeline": fn(Device, Handle, *const c_void);
        create_descriptor_pool = c"vkCreateDescriptorPool":
            fn(Device, *const DescriptorPoolCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_descriptor_pool = c"vkDestroyDescriptorPool": fn(Device, Handle, *const c_void);
        allocate_descriptor_sets = c"vkAllocateDescriptorSets":
            fn(Device, *const DescriptorSetAllocateInfo, *mut Handle) -> Result;
        update_descriptor_sets = c"vkUpdateDescriptorSets":
            fn(Device, u32, *const WriteDescriptorSet, u32, *const c_void);
        create_command_pool = c"vkCreateCommandPool":
            fn(Device, *const CommandPoolCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_command_pool = c"vkDestroyCommandPool": fn(Device, Handle, *const c_void);
        reset_command_pool = c"vkResetCommandPool": fn(Device, Handle, u32) -> Result;
        allocate_command_buffers = c"vkAllocateCommandBuffers":
            fn(Device, *const CommandBufferAllocateInfo, *mut CommandBuffer) -> Result;
        begin_command_buffer = c"vkBeginCommandBuffer":
            fn(CommandBuffer, *const CommandBufferBeginInfo) -> Result;
        end_command_buffer = c"vkEndCommandBuffer": fn(CommandBuffer) -> Result;
        cmd_bind_pipeline = c"vkCmdBindPipeline": fn(CommandBuffer, i32, Handle);
        cmd_bind_descriptor_sets = c"vkCmdBindDescriptorSets":
            fn(CommandBuffer, i32, Handle, u32, u32, *const Handle, u32, *const u32);
        cmd_push_constants = c"vkCmdPushConstants":
            fn(CommandBuffer, Handle, u32, u32, u32, *const c_void);
        cmd_dispatch = c"vkCmdDispatch": fn(CommandBuffer, u32, u32, u32);
        cmd_copy_buffer = c"vkCmdCopyBuffer":
            fn(CommandBuffer, Handle, Handle, u32, *const BufferCopy);
        cmd_pipeline_barrier = c"vkCmdPipelineBarrier":
            fn(CommandBuffer, u32, u32, u32, u32, *const MemoryBarrier, u32, *const c_void, u32, *const c_void);
        queue_submit = c"vkQueueSubmit": fn(Queue, u32, *const SubmitInfo, Handle) -> Result;
        create_fence = c"vkCreateFence":
            fn(Device, *const FenceCreateInfo, *const c_void, *mut Handle) -> Result;
        destroy_fence = c"vkDestroyFence": fn(Device, Handle, *const c_void);
        reset_fences = c"vkResetFences": fn(Device, u32, *const Handle) -> Result;
        wait_for_fences = c"vkWaitForFences":
            fn(Device, u32, *const Handle, u32, u64) -> Result;
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Sizes and offsets as the C compiler gives them for
    /// `vulkan_core.h` on x86-64 and aarch64.
    #[test]
    fn layouts_match_c() {
        assert_eq!(size_of::<ApplicationInfo>(), 48);
        assert_eq!(size_of::<InstanceCreateInfo>(), 64);
        assert_eq!(size_of::<PhysicalDeviceLimits>(), 504);
        assert_eq!(size_of::<PhysicalDeviceProperties>(), 824);
        assert_eq!(offset_of!(PhysicalDeviceProperties, limits), 296);
        assert_eq!(
            offset_of!(PhysicalDeviceLimits, max_storage_buffer_range),
            28
        );
        assert_eq!(
            offset_of!(PhysicalDeviceLimits, max_compute_shared_memory_size),
            216
        );
        assert_eq!(
            offset_of!(PhysicalDeviceLimits, max_compute_work_group_count),
            220
        );
        assert_eq!(offset_of!(PhysicalDeviceLimits, _f), 304);
        assert_eq!(offset_of!(PhysicalDeviceLimits, _g) + 16, 328);
        assert_eq!(offset_of!(PhysicalDeviceLimits, _h) + 88, 424);
        assert_eq!(offset_of!(PhysicalDeviceLimits, _i) + 16, 496);
        assert_eq!(size_of::<QueueFamilyProperties>(), 24);
        assert_eq!(size_of::<PhysicalDeviceMemoryProperties>(), 520);
        assert_eq!(
            offset_of!(PhysicalDeviceMemoryProperties, memory_heaps),
            264
        );
        assert_eq!(size_of::<PhysicalDeviceFeatures>(), 220);
        assert_eq!(size_of::<DeviceQueueCreateInfo>(), 40);
        assert_eq!(size_of::<DeviceCreateInfo>(), 72);
        assert_eq!(size_of::<BufferCreateInfo>(), 56);
        assert_eq!(size_of::<MemoryRequirements>(), 24);
        assert_eq!(size_of::<MemoryAllocateInfo>(), 32);
        assert_eq!(size_of::<ShaderModuleCreateInfo>(), 40);
        assert_eq!(size_of::<DescriptorSetLayoutBinding>(), 24);
        assert_eq!(size_of::<DescriptorSetLayoutCreateInfo>(), 32);
        assert_eq!(size_of::<PushConstantRange>(), 12);
        assert_eq!(size_of::<PipelineLayoutCreateInfo>(), 48);
        assert_eq!(size_of::<PipelineShaderStageCreateInfo>(), 48);
        assert_eq!(size_of::<ComputePipelineCreateInfo>(), 96);
        assert_eq!(offset_of!(ComputePipelineCreateInfo, layout), 72);
        assert_eq!(size_of::<DescriptorPoolSize>(), 8);
        assert_eq!(size_of::<DescriptorPoolCreateInfo>(), 40);
        assert_eq!(size_of::<DescriptorSetAllocateInfo>(), 40);
        assert_eq!(size_of::<DescriptorBufferInfo>(), 24);
        assert_eq!(size_of::<WriteDescriptorSet>(), 64);
        assert_eq!(size_of::<CommandPoolCreateInfo>(), 24);
        assert_eq!(size_of::<CommandBufferAllocateInfo>(), 32);
        assert_eq!(size_of::<CommandBufferBeginInfo>(), 32);
        assert_eq!(size_of::<MemoryBarrier>(), 24);
        assert_eq!(size_of::<BufferCopy>(), 24);
        assert_eq!(size_of::<SubmitInfo>(), 72);
        assert_eq!(size_of::<FenceCreateInfo>(), 24);
    }
}
