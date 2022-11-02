use crate::context;

use mem::Addr;

// uname implements linux syscall uname(2)
pub fn uname(regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    let kernel = ctx.kernel();
    let version = kernel.version();
    let task = ctx.task();
    let uts = task.uts_namespace();

    fn string_to_field(s: &str) -> [i8; 65] {
        let mut field = [0; 65];
        let bytes = s.as_bytes();
        field[..bytes.len()].clone_from_slice(&bytes.iter().map(|b| *b as i8).collect::<Vec<i8>>());
        field
    }

    let utsname = libc::utsname {
        sysname: string_to_field(&version.sysname),
        nodename: string_to_field(uts.host_name()),
        release: string_to_field(&version.release),
        version: string_to_field(&version.version),
        machine: string_to_field("x86_64"),
        domainname: string_to_field(uts.domain_name()),
    };

    let utsname = unsafe {
        std::slice::from_raw_parts(
            &utsname as *const _ as *const u8,
            std::mem::size_of::<libc::utsname>(),
        )
    };

    task.copy_out_bytes(Addr(regs.rdi), utsname).map(|_| 0)
}
