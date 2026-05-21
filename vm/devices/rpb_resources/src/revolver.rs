#![expect(missing_docs)]
#![forbid(unsafe_code)]

use crate::RpbControllerHandle;
//use crate::RpbDeviceHandle;
use async_trait::async_trait;

//use mesh::MeshPayload;
use pci_resources::ResolvePciDeviceHandleParams;
use pci_resources::ResolvedPciDevice;
// use pci_resources::RpbControllerHandle;
//use rpb_resources::RpbResolver;
use thiserror::Error;
use vm_resource::AsyncResolveResource;
use vm_resource::ResolveError;
// use vm_resource::Resource;
// use vm_resource::ResourceId;
// use virtio::VirtioMmioDevice;
//use net_backend_resources::null::NullHandle;
use virtio::PciInterruptModel;
use virtio::VirtioPciDevice;
//use virtio::resolve::VirtioResolveInput;
use vm_resource::ResourceResolver;
use vm_resource::declare_static_async_resolver;
// use vm_resource::kind::NetEndpointHandleKind;
use virtio::DeviceTraits;
use virtio::LegacyVirtioDevice;
use virtio::LegacyWrapper;
//use virtio::TestDevice;
//use crate::rpb_device::RpbDeviceHandle;
//use crate::Device;
//use std::fs::File;
use virtio::VirtioQueueCallbackWork;
use virtio::VirtioQueueWorkerContext;
use virtio::VirtioState;
use vm_resource::kind::PciDeviceHandleKind;
use vm_resource::kind::RpbDeviceHandle;
use vm_resource::kind::VirtioDeviceHandle;

pub struct RpbResolver;

declare_static_async_resolver! {
    RpbResolver,
    (PciDeviceHandleKind, RpbControllerHandle),
}

/// Error returned by [`NvmeFaultControllerResolver`].
#[derive(Debug, Error)]
#[expect(missing_docs)]
pub enum ResolveRpbError {
    #[error("failed to resolve RPB device")]
    Virtio(#[source] ResolveError),
    #[error("failed to create RPB device")]
    Pci(#[source] std::io::Error),
}

#[async_trait]
impl AsyncResolveResource<PciDeviceHandleKind, RpbControllerHandle> for RpbResolver {
    type Output = ResolvedPciDevice;
    type Error = ResolveRpbError;

    async fn resolve(
        &self,
        _resolver: &ResourceResolver,
        _resource: RpbControllerHandle,
        input: ResolvePciDeviceHandleParams<'_>,
    ) -> Result<Self::Output, Self::Error> {
        //let file = fs_err::File::open(resource.pci_id).into();
        //let device = Device::new(input.driver_source, input.guest_memory.clone(), file, false)?;

        let virtio_device = Box::new(LegacyWrapper::new(
            input.driver_source,
            RpbDevice::new(DeviceTraits {
                device_id: 3,
                device_features: 2,
                max_queues: 2,
                device_register_length: 0,
                ..Default::default()
            }),
            input.guest_memory,
        ));
        let device = VirtioPciDevice::new(
            virtio_device,
            PciInterruptModel::Msix(input.register_msi),
            input.doorbell_registration,
            input.register_mmio,
            input.shared_mem_mapper,
        )
        .map_err(ResolveRpbError::Pci)?;
        /*
        let inner = resolver
            .resolve(
                resource.rpb_resource,
                VirtioResolveInput {
                    driver_source: input.driver_source,
                    guest_memory: input.guest_memory,
                },
            )
            .await
            .map_err(ResolveRpbError::Virtio)?;

        let virtio_device = Box::new(LegacyWrapper::new(
            input.driver_source,
            RpbDevice::new(DeviceTraits {
                device_id: 3,
                device_features: 2,
                max_queues: 2,
                device_register_length: 0,
                ..Default::default()
            }),
            input.guest_memory,
        ));
        let device = VirtioPciDevice::new(
            virtio_device,
            PciInterruptModel::Msix(input.register_msi),
            input.doorbell_registration,
            input.register_mmio,
            input.shared_mem_mapper,
        )
        .map_err(ResolveRpbError::Pci)?;
        */
        Ok(device.into())
    }
}

struct RpbDevice {
    traits: DeviceTraits,
}

impl RpbDevice {
    fn new(traits: DeviceTraits) -> Self {
        Self { traits }
    }
}

impl LegacyVirtioDevice for RpbDevice {
    fn traits(&self) -> DeviceTraits {
        self.traits
    }

    fn read_registers_u32(&self, _offset: u16) -> u32 {
        0
    }

    fn write_registers_u32(&mut self, _offset: u16, _val: u32) {}

    fn get_work_callback(&mut self, _index: u16) -> Box<dyn VirtioQueueWorkerContext + Send> {
        Box::new(RpbTestWork {})
    }

    fn state_change(&mut self, _state: &VirtioState) {}
}

struct RpbTestWork {}

#[async_trait]
impl VirtioQueueWorkerContext for RpbTestWork {
    async fn process_work(&mut self, work: anyhow::Result<VirtioQueueCallbackWork>) -> bool {
        if let Err(_err) = work {
            return false;
        }
        return true;
    }
}
