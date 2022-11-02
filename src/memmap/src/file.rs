use mem::{block_seq::BlockSeq, AccessType};
use utils::{FileRange, SysResult};

pub trait MemmapFile: std::fmt::Debug + std::any::Any {
    fn map_internal(&mut self, fr: FileRange, at: AccessType) -> SysResult<BlockSeq>;
    fn fd(&self) -> (i32, bool);
    fn close(&self);
}
