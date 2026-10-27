//! Beatlumen desktop entry point (Linux and Windows).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

fn main() {
    let args = match bl_app::LaunchArgs::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    #[cfg(target_os = "linux")]
    let result = linux::run(args);
    #[cfg(windows)]
    let result = windows::run(args);
    #[cfg(not(any(target_os = "linux", windows)))]
    let result: Result<(), String> = {
        let _ = args;
        Err("unsupported platform".to_string())
    };
    if let Err(e) = result {
        eprintln!("beatlumen: {e}");
        #[cfg(windows)]
        windows::error_box(&e);
        std::process::exit(1);
    }
}
