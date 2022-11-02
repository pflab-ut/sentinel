use std::ffi::CString;

use nix::sys::memfd;

pub fn create_mem_fd(name: &str, flags: i32) -> nix::Result<i32> {
    let name = CString::new(name).unwrap();
    memfd::memfd_create(
        &name,
        memfd::MemFdCreateFlag::from_bits(flags as u32).unwrap(),
    )
}
