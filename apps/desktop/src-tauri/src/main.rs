#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "macos")]
    if let Some(result) = easydeploymesh_service::run_privileged_socket_helper_from_args() {
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    easydeploymesh_desktop_lib::run();
}
