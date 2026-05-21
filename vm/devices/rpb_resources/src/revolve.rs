// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource resolver definitions for RPB devices.

//use crate::VirtioDevice;
use guestmem::GuestMemory;
use vm_resource::CanResolveTo;
use vm_resource::kind::RpbDeviceHandle;
use vmcore::vm_task::VmTaskDriverSource;

impl CanResolveTo<ResolvedVirtioDevice> for RpbDeviceHandle {
    type Input<'a> = RpbResolveInput<'a>;
}

/// A resolved virtio device.
pub struct ResolvedVirtioDevice(pub Box<dyn VirtioDevice>);

impl<T: 'static + VirtioDevice> From<T> for ResolvedVirtioDevice {
    fn from(value: T) -> Self {
        Self(Box::new(value))
    }
}

/// Resolver input for [`RpbDeviceHandle`].
pub struct RpbResolveInput<'a> {
    /// The VM driver source.
    pub driver_source: &'a VmTaskDriverSource,
    /// The guest memory for virtio device DMA.
    pub guest_memory: &'a GuestMemory,
}
