// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![forbid(unsafe_code)]

//! Virtual PCI relay
//!
//! This module provides a virtual PCI relay for the OpenHCL paravisor. It
//! consumes VPCI buses from the host and relays them to the guest, filtering
//! them as needed.

#[cfg(target_os = "linux")]
pub mod linux_mmio;

// Exported to make it easier to define filters without explicitly pulling in
// `pci_core`.
pub use pci_core::spec::hwid::ClassCode;
pub use pci_core::spec::hwid::ProgrammingInterface;
pub use pci_core::spec::hwid::Subclass;

use anyhow::Context as _;
use chipset_device::ChipsetDevice;
use chipset_device::io::IoResult;
use chipset_device::pci::PciConfigSpace;
use futures::StreamExt as _;
use inspect::Inspect;
use inspect::InspectMut;
use memory_range::MemoryRange;
use openhcl_tdisp::TdispVirtualDeviceInterface;
use openhcl_tdisp::TdispReportType;
use pci_core::spec::hwid::HardwareIds;
use sha2::Digest;
use sha2::Sha384;
use state_unit::StateUnits;
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;
use user_driver::DmaClient;
use user_driver::memory::MemoryBlock;
use vmbus_client::driver::OpenParams;
use vmbus_server::Guid;
use vmcore::device_state::ChangeDeviceState;
use vmcore::save_restore::RestoreError;
use vmcore::save_restore::SaveError;
use vmcore::save_restore::SaveRestore;
use vmcore::save_restore::SavedStateNotSupported;
use vmcore::vm_task::VmTaskDriverSource;
use vmcore::vpci_msi::VpciInterruptMapper;
use vmotherboard::ChipsetDevices;
use vmotherboard::DynamicDeviceUnit;
use vpci_client::MemoryAccess;
use vpci_client::VpciClient;
use vpci_client::VpciDevice;
use vpci_client::VpciDeviceEject;

/// TODO TDISP: Required for the tdisp crate to be built in the meantime.
#[expect(unused_imports)]
use tdisp::TdispHostDeviceInterface;
use tdisp::test_helpers::TDISP_MOCK_DEVICE_ID;
use tdisp::test_helpers::TDISP_MOCK_GUEST_PROTOCOL;
use tdisp::test_helpers::TDISP_MOCK_SUPPORTED_FEATURES;

/// Trait for creating memory access instances.
pub trait CreateMemoryAccess: 'static + Send + Sync {
    /// Creates a new memory access instance for the given guest physical address.
    fn create_memory_access(&self, gpa: u64) -> anyhow::Result<Box<dyn MemoryAccess>>;
}

/// The size of the MMIO region required for each VPCI device.
pub const VPCI_RELAY_MMIO_PER_DEVICE: u64 = vpci_client::MMIO_SIZE;

/// Flags for controlling optional behavior of the VPCI relay.
#[derive(Inspect, Debug, Default, Copy, Clone)]
pub struct VpciRelayOptions {
    /// When set, the relay will exercise a mock TDISP flow for emulated TDISP
    /// devices produced by OpenVMM tests.
    pub test_tdisp_flow: bool,
    /// When set, the relay will attempt to run the real TDISP startup flow for
    /// TDISP-capable devices before exposing them to the guest.
    pub startup_tdisp_flow: bool,
}

/// Virtual PCI relay.
#[derive(Inspect)]
pub struct VpciRelay {
    #[inspect(skip)]
    driver_source: VmTaskDriverSource,
    dma_client: Arc<dyn DmaClient>,
    #[inspect(skip)]
    new_buses: Vec<vmbus_client::OfferInfo>,
    #[inspect(skip)]
    bus_recv: mesh::Receiver<vmbus_client::OfferInfo>,
    #[inspect(skip)]
    vmbus: Arc<vmbus_server::VmbusServerControl>,
    #[inspect(iter_by_key)]
    devices: slab::Slab<RelayedDevice>,
    mmio_range: MemoryRange,
    #[inspect(skip)]
    mmio_access: Box<dyn CreateMemoryAccess>,
    #[inspect(iter_by_index)]
    allowed_devices: Vec<AllowedDevice>,
    #[inspect(hex)]
    vtom: Option<u64>,
    options: VpciRelayOptions,
}

#[derive(Inspect)]
struct RelayedDevice {
    bus_instance_id: Guid,
    bus_client: VpciClient,
    #[inspect(skip)]
    removed: VpciDeviceEject,
    #[inspect(skip)]
    bus_unit: DynamicDeviceUnit,
    #[inspect(skip)]
    device_unit: DynamicDeviceUnit,
    ready_to_remove: bool,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TdiRangeSizeOffset {
    pub range_size: u32,
    pub range_offset: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DmarTarget {
    pub vm_idx: u8,
    pub reserved: [u8; 7],
}


impl RelayedDevice {
    async fn remove(self) {
        self.bus_unit.remove().await;
        self.device_unit.remove().await;
        self.bus_client.shutdown().await;
    }
}

/// An allowed device description.
///
/// Fields that are `Some` must match the device being evaluated to be allowed.
#[derive(Inspect, Copy, Clone, Debug)]
pub struct AllowedDevice {
    /// The vendor ID of the device.
    #[inspect(hex)]
    pub vendor_id: Option<u16>,
    /// The device ID of the device.
    #[inspect(hex)]
    pub device_id: Option<u16>,
    /// The revision ID of the device.
    #[inspect(hex)]
    pub revision_id: Option<u8>,
    /// The programming interface of the device.
    pub prog_if: Option<ProgrammingInterface>,
    /// The subclass of the device.
    pub sub_class: Option<Subclass>,
    /// The base class of the device.
    pub base_class: Option<ClassCode>,
    /// The sub-vendor ID.
    #[inspect(hex)]
    pub sub_vendor_id: Option<u16>,
    /// The sub-system ID.
    #[inspect(hex)]
    pub sub_system_id: Option<u16>,
}

impl AllowedDevice {
    fn allows(&self, hw: &HardwareIds) -> bool {
        let Self {
            vendor_id,
            device_id,
            revision_id,
            prog_if,
            sub_class,
            base_class,
            sub_vendor_id,
            sub_system_id,
        } = *self;
        vendor_id.is_none_or(|x| x == hw.vendor_id)
            && device_id.is_none_or(|x| x == hw.device_id)
            && revision_id.is_none_or(|x| x == hw.revision_id)
            && prog_if.is_none_or(|x| x == hw.prog_if)
            && sub_class.is_none_or(|x| x == hw.sub_class)
            && base_class.is_none_or(|x| x == hw.base_class)
            && sub_vendor_id.is_none_or(|x| x == hw.type0_sub_vendor_id)
            && sub_system_id.is_none_or(|x| x == hw.type0_sub_system_id)
    }
}

impl VpciRelay {
    fn tdcall_tdi_rd(gfunction_id: u64, field: u64, out_buf: u64) -> anyhow::Result<u64> {
        let mshv = hcl::ioctl::Mshv::new().context("failed to open /dev/mshv")?;
        let vtl = mshv
            .create_vtl()
            .context("failed to open mshv_vtl device")?;

        vtl.tdx_tdi_rd_via_tdcall(gfunction_id, field, out_buf)
            .map_err(|err| anyhow::anyhow!("tdcall tdi rd failed: {err:?}"))
    }

    fn tdcall_tdi_start(gfunction_id: u64, bind_session_id: u64) -> anyhow::Result<()> {
        let mshv = hcl::ioctl::Mshv::new().context("failed to open /dev/mshv")?;
        let vtl = mshv
            .create_vtl()
            .context("failed to open mshv_vtl device")?;

        vtl.tdx_tdi_start_via_tdcall(gfunction_id, bind_session_id)
            .map_err(|err| anyhow::anyhow!("tdcall tdi start failed: {err:?}"))
    }

    fn tdcall_tdi_mmio_accept(
        mmio_base_addr: u64,
        mmio_range_idx: u64,
        gfunction_id: u64,
        range_size_offset: u64,
    ) -> anyhow::Result<()> {
        let mshv = hcl::ioctl::Mshv::new().context("failed to open /dev/mshv")?;
        let vtl = mshv
            .create_vtl()
            .context("failed to open mshv_vtl device")?;

        vtl.tdx_tdi_mmio_accept_via_tdcall(
            mmio_base_addr,
            mmio_range_idx,
            gfunction_id,
            range_size_offset,
        )
        .map_err(|output| {
            anyhow::anyhow!(
                "tdcall tdi mmio accept failed: status={:?} gpa_address={:#x} range_size_offset={:#x}",
                output.status,
                output.gpa_address,
                output.range_size_offset
            )
        })
    }

    fn tdcall_dmar_accept(gfunction_id: u64, target: u64) -> anyhow::Result<()> {
        let mshv = hcl::ioctl::Mshv::new().context("failed to open /dev/mshv")?;
        let vtl = mshv
            .create_vtl()
            .context("failed to open mshv_vtl device")?;

        vtl.tdx_dmar_accept_via_tdcall(gfunction_id, target)
            .map_err(|err| anyhow::anyhow!("tdcall dmar accept failed: {err:?}"))
    }

    /// Creates a new VPCI relay.
    pub fn new(
        driver_source: VmTaskDriverSource,
        offers: vmbus_client::ConnectResult,
        vmbus: Arc<vmbus_server::VmbusServerControl>,
        dma_client: Arc<dyn DmaClient>,
        mmio_range: MemoryRange,
        mmio_access: Box<dyn CreateMemoryAccess>,
        vtom: Option<u64>,
        options: VpciRelayOptions,
    ) -> Self {
        Self {
            driver_source,
            dma_client,
            new_buses: offers.offers,
            bus_recv: offers.offer_recv,
            vmbus,
            devices: slab::Slab::new(),
            mmio_range,
            mmio_access,
            allowed_devices: Vec::new(),
            vtom,
            options,
        }
    }

    /// Adds an allowed device to the list. If one of the hardware ID is `!0`
    /// then it is treated as a wildcard.
    ///
    /// Note that if no devices are on the list, then all devices are allowed.
    pub fn add_allowed_device(&mut self, dev: AllowedDevice) {
        self.allowed_devices.push(dev);
    }

    /// Wait for the relay to be ready. This might never return. This call is cancellable.
    pub async fn wait_ready(&mut self) {
        poll_fn(|cx| {
            if !self.new_buses.is_empty() {
                return Poll::Ready(());
            }
            if self.devices.iter_mut().any(|(_, dev)| {
                let p = dev.ready_to_remove || dev.removed.poll_next_unpin(cx).is_ready();
                if p {
                    dev.ready_to_remove = true;
                }
                p
            }) {
                return Poll::Ready(());
            }
            if let Poll::Ready(Some(bus)) = self.bus_recv.poll_next_unpin(cx) {
                self.new_buses.push(bus);
                return Poll::Ready(());
            }
            Poll::Pending
        })
        .await
    }

    /// Process any waiting activity. This call is not cancellable.
    pub async fn process(
        &mut self,
        chipset: &ChipsetDevices,
        units: &mut StateUnits,
    ) -> anyhow::Result<()> {
        let mut i = 0;
        while i < self.devices.len() {
            if self.devices[i].ready_to_remove {
                let dev = self.devices.remove(i);
                dev.remove().await;
            } else {
                i += 1;
            }
        }
        while let Some(bus) = self.new_buses.pop() {
            self.relay_vpci_bus(chipset, units, bus).await?;
        }
        Ok(())
    }

    async fn relay_vpci_bus(
        &mut self,
        chipset: &ChipsetDevices,
        state_units: &mut StateUnits,
        offer_info: vmbus_client::OfferInfo,
    ) -> anyhow::Result<()> {
        let device_slot = {
            let entry = self.devices.vacant_entry();
            if (entry.key() as u64 + 1) * vpci_client::MMIO_SIZE > self.mmio_range.len() {
                anyhow::bail!("not enough MMIO space left");
            }
            entry.key()
        };

        let instance_id = offer_info.offer.instance_id;

        let mmio_gpa = self.mmio_range.start() + (device_slot as u64) * vpci_client::MMIO_SIZE;
        let mmio_size = vpci_client::MMIO_SIZE;
        let mmio = self.mmio_access.create_memory_access(mmio_gpa)?;

        let channel = vmbus_client::driver::open_channel(
            self.driver_source.simple(),
            offer_info,
            OpenParams {
                ring_pages: 20,
                ring_offset_in_pages: 10,
            },
            self.dma_client.as_ref(),
        )
        .await?;

        // FUTURE: handle more than one device. Note, though, that Hyper-V
        // doesn't really do this in practice.
        let (devices, _devices_recv) = mesh::channel();
        let (vpci_client, devices) =
            VpciClient::connect(self.driver_source.simple(), channel, mmio, devices).await?;

        let Some(vpci_device) = devices.into_iter().next() else {
            tracing::info!(%instance_id, "no device on VPCI bus");
            return Ok(());
        };

        let hw_ids = vpci_device.hw_ids();

        if !self.allowed_devices.is_empty()
            && !self.allowed_devices.iter().any(|d| d.allows(hw_ids))
        {
            let prog_if = hw_ids.prog_if;
            let sub_class = hw_ids.sub_class;
            let base_class = hw_ids.base_class;
            tracing::warn!(
                %instance_id,
                vendor_id = hw_ids.vendor_id,
                device_id = hw_ids.device_id,
                ?prog_if,
                ?sub_class,
                ?base_class,
                "device not allowed on VPCI bus"
            );
            return Ok(());
        }

        // Extract owned values before moving vpci_device in init()
        let vendor_id = hw_ids.vendor_id;
        let device_id = hw_ids.device_id;

        tracing::info!(%instance_id, vendor_id, device_id, "vpci relay device arrived");
        tracing::info!(
            %instance_id,
            mmio_start_gpa = mmio_gpa,
            mmio_size,
            mmio_end_gpa = mmio_gpa + mmio_size,
            "assigned VPCI MMIO space for relayed device"
        );

        let (vpci_device, removed) = vpci_device
            .init()
            .await
            .context("failed to initialize vpci device")?;
        let vpci_device = Arc::new(vpci_device);

        tracing::info!("relay_vpci_bus: vpci_device creation\n");

        if self.options.test_tdisp_flow {
            Self::tdisp_test_mock_flow(vpci_device.clone())
                .await
                .expect("failed to exercise TDISP flow test");
        } else if self.options.startup_tdisp_flow {
            self.tdisp_startup_flow(vpci_device.clone(), mmio_gpa, mmio_size)
                .await?;
        }

        let device_name = format!("assigned_device:vpci-{instance_id}");
        tracing::info!(
            %instance_id,
            vendor_id,
            device_id,
            %device_name,
            "relaying VPCI device from VTL2 to VTL0"
        );

        let (device_unit, device) = chipset
            .add_dyn_device(&self.driver_source, state_units, device_name, async |_| {
                Ok(RelayedVpciDevice(vpci_device.clone()))
            })
            .await?;

        let interrupt_mapper = VpciInterruptMapper::new(vpci_device);

        let (bus_unit, _) = {
            let vpci_bus_name = format!("vpci:{instance_id}");
            chipset
                .add_dyn_device(
                    &self.driver_source,
                    state_units,
                    vpci_bus_name,
                    async |mmio| {
                        let bus = vpci::bus::VpciBus::new(
                            &self.driver_source,
                            instance_id,
                            device,
                            mmio,
                            self.vmbus.as_ref(),
                            interrupt_mapper,
                            self.vtom,
                        )
                        .await?;

                        anyhow::Ok(bus)
                    },
                )
                .await?
        };

        let entry = self.devices.vacant_entry();
        assert_eq!(entry.key(), device_slot);
        entry.insert(RelayedDevice {
            bus_instance_id: instance_id,
            bus_client: vpci_client,
            removed,
            bus_unit,
            device_unit,
            ready_to_remove: false,
        });

        state_units.start_stopped_units().await;
        Ok(())
    }

    /// Exercises a mocked TDISP flow for emulated TDISP devices produced by OpenVMM tests.
    async fn tdisp_test_mock_flow(device: Arc<VpciDevice>) -> anyhow::Result<()> {
        // For now, exercise just the "get device interface" flow and ensure that the device responds as
        // TDISP capable and with the right mocked device information.

        tracing::info!(
            "tdisp_test_mock_flow: exercising TDISP flow because OPENHCL_TEST_CONFIG=TDISP_VPCI_FLOW_TEST was set"
        );

        let device_interface_info = device
            .tdisp_get_device_interface_info()
            .await
            .context("tdisp_test_mock_flow: failed to get device interface info over vpci")?;

        tracing::info!(
            "tdisp_test_mock_flow: device interface info: {:?}",
            device_interface_info
        );

        assert_eq!(
            device_interface_info.guest_protocol_type,
            TDISP_MOCK_GUEST_PROTOCOL as i32
        );
        assert_eq!(device_interface_info.tdisp_device_id, TDISP_MOCK_DEVICE_ID);
        assert_eq!(
            device_interface_info.supported_features,
            TDISP_MOCK_SUPPORTED_FEATURES
        );

        Ok(())
    }

    /// Runs the startup TDISP sequence for TDISP-capable devices.
    async fn tdisp_startup_flow(
        &mut self,
        device: Arc<VpciDevice>,
        mmio_gpa: u64,
        mmio_size: u64,
    ) -> anyhow::Result<()> {
        let device_interface_info = match device.tdisp_get_device_interface_info().await {
            Ok(info) => info,
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "device did not negotiate TDISP protocol, skipping TDISP startup"
                );
                return Ok(());
            }
        };

        tracing::info!(
            ?device_interface_info,
            "TDISP-capable device detected, executing startup flow"
        );

        let tdi_support = device
            .tdisp_get_tdi_support()
            .await
            .context("failed to retrieve TDI support")?;

        tracing::info!(
            tdi_support_len = tdi_support.len(),
            "received TDI support during TDISP startup"
        );

        tracing::info!("tdi_support: {:x?}\n", tdi_support);

        let tdi_device_id = device
            .tdisp_get_tdi_device_id()
            .await
            .context("failed to retrieve TDI device ID")?;

        tracing::info!(
            "received TDI device ID: {:x} during TDISP startup", tdi_device_id
        );

        let bind_result = device.tdisp_bind_interface().await;
        match &bind_result {
            Ok(()) => {
                tracing::info!(
                    tdi_device_id,
                    "TDISP interface bind succeeded"
                );
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    tdi_device_id,
                    "TDISP interface bind failed with error 0xc0350071 or similar; continuing to diagnose"
                );
                // Don't fail immediately; continue to get interface status for diagnostics
            }
        }

        let tdi_status = device
            .tdisp_get_tdi_interface_status()
            .await
            .context("failed to retrieve TDI interface status")?;

        tracing::info!(
            "received TDI tdi_status: {:x} during TDISP startup", tdi_status
        );

        const TDI_RD_FIELD_GET_STATE: u64 = 2;

        let tdi_rd_state_value = Self::tdcall_tdi_rd(tdi_device_id, TDI_RD_FIELD_GET_STATE, 0)?;
        tracing::info!(
            gfunction_id = tdi_device_id,
            field = TDI_RD_FIELD_GET_STATE,
            tdi_rd_state_value,
            "TDG.TDI.RD tdcall completed during TDISP startup"
        );

        const TDI_RD_FIELD_BIND_SESSION: u64 = 5;

        let tdi_rd_bindsession_value = Self::tdcall_tdi_rd(tdi_device_id, TDI_RD_FIELD_BIND_SESSION, 0)?;
        tracing::info!(
            gfunction_id = tdi_device_id,
            field = TDI_RD_FIELD_BIND_SESSION,
            tdi_rd_bindsession_value,
            "TDG.TDI.RD tdcall completed during TDISP startup"
        );

        const TDI_RD_FIELD_REPORT_HASH: u64 = 3;
        let tdi_rd_report_hash_gpa = 0;

        match Self::tdcall_tdi_rd(
            tdi_device_id,
            TDI_RD_FIELD_REPORT_HASH,
            0,
        ) {
            Ok(tdi_rd_report_hash_value) => {
                tracing::info!(
                    gfunction_id = tdi_device_id,
                    field = TDI_RD_FIELD_REPORT_HASH,
                    gpa = tdi_rd_report_hash_gpa,
                    tdi_rd_report_hash_value,
                    "TDG.TDI.RD tdcall completed during TDISP startup"
                );
            }
            Err(err) => {
                tracing::warn!(
                    gfunction_id = tdi_device_id,
                    field = TDI_RD_FIELD_REPORT_HASH,
                    gpa = tdi_rd_report_hash_gpa,
                    error = %err,
                    "TDG.TDI.RD failed; continuing TDISP startup"
                );
            }
        }



        /*let requested_hash_bytes = usize::try_from(tdi_rd_report_hash_len).unwrap_or(usize::MAX);
        let hash_bytes_len = requested_hash_bytes.min(tdi_rd_report_page.len());
        if requested_hash_bytes > tdi_rd_report_page.len() {
            tracing::warn!(
                requested_hash_bytes,
                buffer_len = tdi_rd_report_page.len(),
                "TDG.TDI.RD requested hash length exceeds scratch page; truncating"
            );
        }

        let tdi_rd_report_hash_bytes = &tdi_rd_report_page[..hash_bytes_len];
        let tdi_rd_report_hash_hex = tdi_rd_report_hash_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        tracing::info!(
            requested_hash_bytes,
            hash_bytes_len,
            tdi_rd_report_hash_hex = %tdi_rd_report_hash_hex,
            "read device attestation hash bytes from TDG.TDI.RD GPA buffer"
        );

        const TDI_RD_FIELD_DEVICE_ATTESTATION_HASH: u64 = 4;
        // Allocate a 4KB scratch buffer and pass its address as the input
        // for TDG.TDI.RD in this prototype flow.
        let tdi_rd_attestation_page = vec![0u8; 4096];
        let tdi_rd_attestation_pageaddr: u64 = tdi_rd_attestation_page.as_ptr() as u64;



        let tdi_rd_device_attestion_hash_len = Self::tdcall_tdi_rd(tdi_device_id, TDI_RD_FIELD_DEVICE_ATTESTATION_HASH, tdi_rd_attestation_pageaddr)?;
        tracing::info!(
            gfunction_id = tdi_device_id,
            field = TDI_RD_FIELD_DEVICE_ATTESTATION_HASH,
            tdi_rd_device_attestion_hash_len,
            "TDG.TDI.RD tdcall completed during TDISP startup"
        );
        let requested_hash_bytes_1 = usize::try_from(tdi_rd_device_attestion_hash_len).unwrap_or(usize::MAX);
        let hash_bytes_len_1 = requested_hash_bytes_1.min(tdi_rd_attestation_page.len());
        if requested_hash_bytes_1 > tdi_rd_attestation_page.len() {
            tracing::warn!(
                requested_hash_bytes_1,
                buffer_len = tdi_rd_attestation_page.len(),
                "TDG.TDI.RD requested hash length exceeds scratch page; truncating"
            );
        }

        let tdi_rd_device_attestion_hash_bytes = &tdi_rd_attestation_page[..hash_bytes_len];
        let tdi_rd_device_attestion_hash_hex = tdi_rd_device_attestion_hash_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        tracing::info!(
            requested_hash_bytes_1,
            hash_bytes_len_1,
            tdi_rd_device_attestion_hash_hex = %tdi_rd_device_attestion_hash_hex,
            "read device attestation hash bytes from TDG.TDI.RD GPA buffer"
        );*/

        let tdi_report_buffer = device
            .tdisp_get_device_report(&TdispReportType::InterfaceReport)
            .await
            .context("failed to retrieve TDI report")?;

        tracing::info!(
            ?tdi_report_buffer,
            "received TDI report during TDISP startup"
        );

        let tdi_report_sha384 = Sha384::digest(&tdi_report_buffer);
        let tdi_report_sha384_hex = tdi_report_sha384
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        tracing::info!(
            tdi_report_len = tdi_report_buffer.len(),
            tdi_report_sha384 = %tdi_report_sha384_hex,
            "computed SHA-384 digest for TDI report"
        );

        let tdi_report = tdisp::devicereport::deserialize_tdi_report(&tdi_report_buffer)
            .context("failed to deserialize TDI report")?;

        tracing::info!(
            ?tdi_report,
            "received TDI report during TDISP startup"
        );

        match Self::tdcall_tdi_start(tdi_device_id, tdi_rd_bindsession_value) {
            Ok(()) => {
                tracing::info!(
                    gfunction_id = tdi_device_id,
                    bind_session_id = tdi_rd_bindsession_value,
                    "TDG.TDI.START tdcall completed during TDISP startup"
                );
            }
            Err(err) => {
                tracing::error!(
                    gfunction_id = tdi_device_id,
                    bind_session_id = tdi_rd_bindsession_value,
                    error = %err,
                    "TDG.TDI.START tdcall failed during TDISP startup"
                );
                return Err(err).context("failed TDG.TDI.START tdcall");
            }
        }

        let start_result = device.tdisp_start_device().await;
        match &start_result {
            Ok(()) => {
                tracing::info!(
                    tdi_device_id,
                    "TDISP interface start succeeded"
                );
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    tdi_device_id,
                    "TDISP interface start failed with error; continuing to diagnose"
                );
                // Don't fail immediately; continue to for diagnostics
            }
        }

        if !mmio_size.is_multiple_of(user_driver::memory::PAGE_SIZE64) {
            anyhow::bail!("MMIO size is not page-aligned for TDI range size field");
        }

        let mmio_range_size = u32::try_from(mmio_size / user_driver::memory::PAGE_SIZE64)
            .context("MMIO page count exceeds 32-bit TDI range size field")?;

        // Find the private (TEE) MMIO range from the TDI report
        let range_offset = tdi_report
            .mmio_interface_info
            .iter()
            .find(|range| !range.flags.is_non_tee_mem())
            .map(|range| range.range_id as u32)
            .context("no private MMIO range found in TDI report")?;

        tracing::info!(
            mmio_range_size,
            range_offset,
            "extracted TDI range parameters for TDISP startup"
        );

        /*let mmio_range_size_offset = TdiRangeSizeOffset {
            range_size: mmio_range_size,
            range_offset: range_offset,
        };
        let mmio_range_size_offset_u64 =
            (u64::from(mmio_range_size_offset.range_offset) << 32)
                | u64::from(mmio_range_size_offset.range_size);


        match Self::tdcall_tdi_mmio_accept(mmio_gpa, mmio_range_size_offset_u64, tdi_device_id, mmio_size) {
            Ok(()) => {
                tracing::info!(
                    gfunction_id = tdi_device_id,
                    mmio_base_addr = mmio_gpa,
                    mmio_range_idx = 0,
                    range_size_offset = mmio_size,
                    "TDG.TDI.MMIO.ACCEPT tdcall completed during TDISP startup"
                );
            }
            Err(err) => {
                tracing::error!(
                    gfunction_id = tdi_device_id,
                    mmio_base_addr = mmio_gpa,
                    mmio_range_idx = 0,
                    range_size_offset = mmio_size,
                    error = %err,
                    "TDG.TDI.MMIO.ACCEPT tdcall failed during TDISP startup"
                );
                return Err(err).context("failed TDG.TDI.MMIO.ACCEPT tdcall");
            }
        }*/

        let dmar_target = DmarTarget {
            vm_idx: 1,
            reserved: [0; 7],
        };
        
        let dmar_target_u64 = u64::from_le_bytes([
            dmar_target.vm_idx,
            dmar_target.reserved[0],
            dmar_target.reserved[1],
            dmar_target.reserved[2],
            dmar_target.reserved[3],
            dmar_target.reserved[4],
            dmar_target.reserved[5],
            dmar_target.reserved[6],
        ]);

        // Accept DMAR
        match Self::tdcall_dmar_accept(tdi_device_id, dmar_target_u64) {
            Ok(()) => {
                tracing::info!(
                    gfunction_id = tdi_device_id,
                    target = dmar_target_u64,
                    "TDG.DMAR.ACCEPT tdcall completed during TDISP startup"
                );
            }
            Err(err) => {
                tracing::warn!(
                    gfunction_id = tdi_device_id,
                    target = dmar_target_u64,
                    error = %err,
                    "TDG.DMAR.ACCEPT tdcall failed; continuing TDISP startup"
                );
            }
        }

        tracing::info!("TDISP startup flow completed");

        Ok(())
    }
}

#[derive(InspectMut)]
#[inspect(transparent)]
struct RelayedVpciDevice(Arc<VpciDevice>);

impl ChipsetDevice for RelayedVpciDevice {
    fn supports_pci(&mut self) -> Option<&mut dyn PciConfigSpace> {
        Some(self)
    }
}

impl PciConfigSpace for RelayedVpciDevice {
    fn pci_cfg_read(&mut self, offset: u16, value: &mut u32) -> IoResult {
        *value = self.0.read_cfg(offset);
        IoResult::Ok
    }

    fn pci_cfg_write(&mut self, offset: u16, value: u32) -> IoResult {
        self.0.write_cfg(offset, value);
        IoResult::Ok
    }
}

impl ChangeDeviceState for RelayedVpciDevice {
    fn start(&mut self) {}

    async fn stop(&mut self) {}

    async fn reset(&mut self) {}
}

impl SaveRestore for RelayedVpciDevice {
    type SavedState = SavedStateNotSupported;

    fn save(&mut self) -> Result<Self::SavedState, SaveError> {
        Err(SaveError::NotSupported)
    }

    fn restore(&mut self, state: Self::SavedState) -> Result<(), RestoreError> {
        match state {}
    }
}
