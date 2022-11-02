use super::FeatureSet;

#[derive(Debug)]
pub struct ArchContext {
    pub regs: libc::user_regs_struct,
    pub feature_set: FeatureSet,
}

impl ArchContext {
    pub fn new() -> Self {
        Self {
            regs: utils::init_libc_regs(),
            feature_set: FeatureSet::new(),
        }
    }

    pub fn cpuid_emulate(&mut self) {
        let orig_rax = self.regs.rax as u32;
        let orig_rcx = self.regs.rcx as u32;
        let (rax, rbx, rcx, rdx) = self.feature_set.emulate_id(orig_rax, orig_rcx);
        self.regs.rax = rax as u64;
        self.regs.rbx = rbx as u64;
        self.regs.rcx = rcx as u64;
        self.regs.rdx = rdx as u64;
        println!(
            "CPUID({}, {}): {}, {}, {}, {}",
            orig_rax, orig_rcx, rax, rbx, rcx, rdx
        )
    }
}
