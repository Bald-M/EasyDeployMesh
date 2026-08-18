use easydeploymesh_service::{
    ActivityRepository, ControlPlane, DeviceRegistry, ImageLibrary, JobRepository,
};
use std::{error::Error, path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let data_dir = arguments
        .next()
        .ok_or("usage: control_plane <data-dir> [bind-address] [port]")?;
    let bind_address = arguments.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = arguments
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(7760);

    let data_dir = PathBuf::from(data_dir);
    let registry = Arc::new(DeviceRegistry::open(data_dir.join("devices"))?);
    let jobs = Arc::new(JobRepository::open(data_dir.join("jobs"))?);
    let images = Arc::new(ImageLibrary::open(data_dir.join("library"))?);
    let activities = Arc::new(ActivityRepository::open(data_dir.join("activities.json"))?);
    let control_plane = ControlPlane::new(registry, jobs, images, activities);
    let status = control_plane.start(&bind_address, port).await?;

    println!(
        "ENDPOINT={}",
        status.endpoint.as_deref().unwrap_or_default()
    );
    println!(
        "ENROLLMENT_TOKEN={}",
        status.enrollment_token.as_deref().unwrap_or_default()
    );
    println!("Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    control_plane.stop().await?;
    Ok(())
}
