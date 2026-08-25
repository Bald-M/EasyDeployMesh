//! Cross-platform service primitives used by the privileged EasyDeployMesh host.

use easydeploymesh_core::RuntimeStatus;
use if_addrs::IfAddr;
use serde::Serialize;

mod activity_repository;
mod control_plane;
mod device_registry;
mod image_library;
mod job_repository;
#[cfg(target_os = "macos")]
mod macos_privileged;
mod pxe;
mod wimlib;

pub use activity_repository::{ActivityQuery, ActivityRepository, ActivityRepositoryError};
pub use control_plane::{ControlPlane, ControlPlaneError};
pub use device_registry::{DEVICE_VERIFICATION_WINDOW, DeviceRegistry, DeviceRegistryError};
pub use image_library::{
    ImageLibrary, ImageLibraryError, PreparedGhoDeployment, PreparedGhoImageFile,
    PreparedGhoImageSet,
};
pub use job_repository::{JobRepository, JobRepositoryError};
#[cfg(target_os = "macos")]
pub use macos_privileged::run_privileged_socket_helper_from_args;
pub use pxe::{BootPackage, PxeService, PxeServiceError, validate_pxe_config};
pub use wimlib::{WimlibCapability, configure_wimlib, wimlib_capability};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceSummary {
    pub name: String,
    pub address: String,
    pub netmask: String,
    pub is_loopback: bool,
    pub is_up: bool,
}

pub fn runtime_status() -> RuntimeStatus {
    RuntimeStatus {
        service_state: "idle".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
        active_interface: None,
        connected_devices: 0,
        queued_jobs: 0,
    }
}

pub fn list_network_interfaces() -> Result<Vec<NetworkInterfaceSummary>, String> {
    let mut interfaces = if_addrs::get_if_addrs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|interface| {
            let address = interface.ip().to_string();
            let netmask = match &interface.addr {
                IfAddr::V4(address) => address.netmask.to_string(),
                IfAddr::V6(address) => address.netmask.to_string(),
            };
            let is_loopback = interface.is_loopback();
            let is_up = interface.is_oper_up();

            NetworkInterfaceSummary {
                name: interface.name,
                address,
                netmask,
                is_loopback,
                is_up,
            }
        })
        .collect::<Vec<_>>();

    interfaces.sort_by(|left, right| {
        left.is_loopback
            .cmp(&right.is_loopback)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.address.cmp(&right.address))
    });
    interfaces.dedup();

    Ok(interfaces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_safe_before_network_service_starts() {
        let status = runtime_status();
        assert_eq!(status.service_state, "idle");
        assert_eq!(status.connected_devices, 0);
        assert!(status.active_interface.is_none());
    }

    #[test]
    fn local_host_has_at_least_one_interface() {
        let interfaces = list_network_interfaces().expect("interfaces should be readable");
        assert!(!interfaces.is_empty());
    }
}
