use auth::Context as AuthContext;


use crate::context;

// getuid implements linux syscall getuid(2)
pub fn getuid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    let creds = ctx.credentials();
    let ruid = creds
        .user_namespace
        .map_from_kuid(&creds.real_kuid)
        .or_overflow()
        .0;
    Ok(ruid as usize)
}

// geteuid implements linux syscall geteuid(2)
pub fn geteuid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    let creds = ctx.credentials();
    let euid = creds
        .user_namespace
        .map_from_kuid(&creds.effective_kuid)
        .or_overflow()
        .0;
    Ok(euid as usize)
}

// getgid implements linux syscall getgid(2)
pub fn getgid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    let creds = ctx.credentials();
    let rgid = creds
        .user_namespace
        .map_from_kgid(&creds.real_kgid)
        .or_overflow()
        .0;
    Ok(rgid as usize)
}

// getegid implements linux syscall getegid(2)
pub fn getegid(_regs: &libc::user_regs_struct) -> super::Result {
    let ctx = context::context();
    let creds = ctx.credentials();
    let egid = creds
        .user_namespace
        .map_from_kgid(&creds.effective_kgid)
        .or_overflow()
        .0;
    Ok(egid as usize)
}
