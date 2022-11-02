use nix::unistd::Pid;

pub trait Context {
    fn tid(&self) -> Pid;
    fn task_init_regs(&self) -> libc::user_regs_struct;
    fn ptrace_set_regs(&self, regs: libc::user_regs_struct) -> nix::Result<()>;
}
