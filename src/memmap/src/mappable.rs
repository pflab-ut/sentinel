use std::{rc::Weak, sync::RwLock};

use mem::{AccessType, AddrRange};
use utils::{FileRange, Range, SysResult};

use super::file::MemmapFile;

#[derive(Debug)]
pub struct Translation {
    source: MappableRange,
    file: Weak<RwLock<dyn MemmapFile>>,
    offset: u64,
    perm: AccessType,
}

impl Translation {
    pub fn new(
        source: MappableRange,
        file: Weak<RwLock<dyn MemmapFile>>,
        offset: u64,
        perm: AccessType,
    ) -> Self {
        Self {
            source,
            file,
            offset,
            perm,
        }
    }

    pub fn source(&self) -> MappableRange {
        self.source
    }

    pub fn file(&self) -> &Weak<RwLock<dyn MemmapFile>> {
        &self.file
    }

    pub fn perms(&self) -> AccessType {
        self.perm
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn file_range(&self) -> FileRange {
        FileRange {
            start: self.offset,
            end: self.offset + self.source.len(),
        }
    }
}

pub type MappableRange = Range<u64>;

pub trait Mappable: std::fmt::Debug {
    fn translate(
        &self,
        required: MappableRange,
        optional: MappableRange,
        at: AccessType,
    ) -> (Vec<Translation>, SysResult<()>);
    fn add_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> SysResult<()>;
    fn remove_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool);
    fn copy_mapping(
        &mut self,
        src_ar: AddrRange,
        dst_ar: AddrRange,
        offset: u64,
        writable: bool,
    ) -> SysResult<()>;
}
