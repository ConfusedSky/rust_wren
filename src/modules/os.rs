use wren::VERSION;
use wren_macros::foreign_static_method;

use super::{source_file, Class, Module};
use std::env::args;
use std::env::current_dir;

pub fn init_module<'wren>() -> Module<'wren> {
    let mut platform_class = Class::new();
    platform_class
        .static_methods
        .insert("isPosix", foreign_is_posix);
    platform_class.static_methods.insert("name", foreign_name);
    platform_class
        .static_methods
        .insert("homePath", foreign_home_path);

    let mut process_class = Class::new();
    process_class
        .static_methods
        .insert("allArguments", foreign_all_arguments);
    process_class
        .static_methods
        .insert("version", foreign_version);
    process_class.static_methods.insert("cwd", foreign_cwd);
    process_class.static_methods.insert("pid", foreign_pid);
    process_class.static_methods.insert("ppid", foreign_ppid);

    let mut module = Module::new(source_file!("os.wren"));
    module.classes.insert("Process", process_class);
    module.classes.insert("Platform", platform_class);

    module
}

#[foreign_static_method]
fn is_posix() -> bool {
    std::env::consts::FAMILY == "unix"
}

#[foreign_static_method]
const fn name() -> &'static str {
    std::env::consts::OS
}

#[foreign_static_method]
fn home_path() -> Result<String, &'static str> {
    let dir = dirs::home_dir().ok_or("Cannot get the user's home directory")?;
    Ok(dir.to_string_lossy().to_string())
}

#[foreign_static_method]
fn all_arguments() -> Vec<String> {
    args().collect()
}

#[foreign_static_method]
fn version() -> std::ffi::CString {
    std::ffi::CString::from_vec_with_nul(VERSION.to_vec()).expect("Version string should be valid")
}

#[foreign_static_method]
fn cwd() -> Result<String, &'static str> {
    let dir = current_dir().map_err(|_| "Cannot get current working directory.")?;
    Ok(dir.to_string_lossy().to_string())
}

#[foreign_static_method]
fn pid() -> f64 {
    f64::from(std::process::id())
}

#[foreign_static_method]
fn ppid() -> Result<f64, &'static str> {
    #[cfg(target_family = "unix")]
    let result = Ok(std::os::unix::process::parent_id().into());

    #[cfg(not(target_family = "unix"))]
    let result = Err("PPID is not implemented outside of unix based operating systems!");

    result
}
