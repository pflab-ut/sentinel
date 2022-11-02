use mem::Addr;

pub const SIGNAL_STACK_FLAG_ON_STACK: u32 = 1;
pub const SIGNAL_STACK_FLAG_DISABLE: u32 = 2;

#[derive(Default, Clone, Copy, Debug)]
#[repr(C)]
pub struct SignalStack {
    pub addr: usize,
    pub flags: u32,
    pub size: u64,
}

impl SignalStack {
    pub fn contains(&self, sp: Addr) -> bool {
        let addr = self.addr as u64;
        addr < sp.0 && sp.0 <= (addr + self.size)
    }

    pub unsafe fn as_bytes(&self) -> &[u8] {
        let size = std::mem::size_of::<SignalStack>();
        std::slice::from_raw_parts((self as *const SignalStack) as *const u8, size)
    }

    pub unsafe fn from_bytes(bytes: &[u8]) -> Self {
        std::ptr::read(bytes.as_ptr() as *const SignalStack)
    }
}
