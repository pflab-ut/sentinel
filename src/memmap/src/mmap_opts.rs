use std::{cell::RefCell, rc::Rc};

use mem::{AccessType, Addr};

use super::Mappable;

#[derive(Default, Debug)]
pub struct MmapOpts {
    pub length: u64,
    pub mappable: Option<Rc<RefCell<dyn Mappable>>>,
    pub offset: u64,
    pub addr: Addr,
    pub private: bool,
    pub fixed: bool,
    pub unmap: bool,
    pub map32bit: bool,
    pub grows_down: bool,
    pub precommit: bool,
    pub perms: AccessType,
    pub max_perms: AccessType,
    pub mlock_mode: MLockMode,
    pub force: bool,
}

#[derive(PartialEq, PartialOrd, Copy, Clone, Debug)]
pub enum MLockMode {
    None_,
    Eager,
    Lazy,
}

impl Default for MLockMode {
    fn default() -> Self {
        MLockMode::None_
    }
}
