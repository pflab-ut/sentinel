use utils::SysResult;

use crate::Addr;

pub trait Context {
    fn copy_out_bytes(&self, addr: Addr, src: &[u8]) -> SysResult<usize>;
    fn copy_in_bytes(&self, addr: Addr, dst: &mut [u8]) -> SysResult<usize>;
}
