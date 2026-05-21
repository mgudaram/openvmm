// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TODO RPB

#![expect(missing_docs)]
#![forbid(unsafe_code)]

pub mod resolver;
// use crate::rpb_device::RpbDeviceHandle;
use mesh::MeshPayload;
use vm_resource::Resource;
use vm_resource::ResourceId;
// use vm_resource::kind::NetEndpointHandleKind;
// use rpb_resources::rpb_device::RpbDeviceHandle;
use vm_resource::kind::PciDeviceHandleKind;
use vm_resource::kind::RpbDeviceHandle;
use vm_resource::kind::VirtioDeviceHandle;

#[derive(MeshPayload)]
pub struct RpbControllerHandle {
    pub instance_id: guid::Guid,
    pub pci_id: String,
    // pub rpb_resource: Resource<RpbDeviceHandle>,
}

//pub struct RpbResolver;

impl ResourceId<PciDeviceHandleKind> for RpbControllerHandle {
    const ID: &'static str = "rpb";
}

pub mod rpb_device {
    use mesh::MeshPayload;
    use vm_resource::ResourceId;
    use vm_resource::kind::VirtioDeviceHandle;

    #[derive(MeshPayload)]
    pub struct RpbDeviceHandle {
        pub path: String,
    }

    impl ResourceId<VirtioDeviceHandle> for RpbDeviceHandle {
        const ID: &'static str = "virtio-rpb";
    }
}
