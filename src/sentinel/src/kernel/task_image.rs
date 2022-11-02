use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

use arch::ArchContext;
use fs::mount::MountNamespace;
use mem::Addr;
use platform::PtraceAddressSpace;

use crate::{loader::Loader, mm::MemoryManager};

#[derive(Debug)]
pub enum MemoryManagerState {
    Empty,
    Loaded(Rc<RefCell<MemoryManager>>),
}

#[derive(Debug)]
pub struct TaskImage {
    pub memory_manager: MemoryManagerState,
}

impl TaskImage {
    pub fn new() -> Self {
        Self {
            memory_manager: MemoryManagerState::Empty,
        }
    }

    pub fn load<P: AsRef<Path>>(
        &mut self,
        executable_path: P,
        argv: Vec<String>,
        envv: &HashMap<String, String>,
        extra_auxv: &HashMap<u64, Addr>,
        mount: &MountNamespace,
    ) -> anyhow::Result<ArchContext> {
        let mut mm = MemoryManager::new();
        let mut loader = Loader::new(&mut mm, argv, envv, mount);
        let arch_context = loader.load(executable_path, extra_auxv)?;
        self.memory_manager = MemoryManagerState::Loaded(Rc::new(RefCell::new(mm)));
        Ok(arch_context)
    }

    pub fn set_address_space(&self, address_space: PtraceAddressSpace) {
        match self.memory_manager {
            MemoryManagerState::Loaded(ref mm) => {
                mm.borrow_mut()
                    .set_address_space(Some(Box::new(address_space)));
            }
            MemoryManagerState::Empty => panic!("MemoryManager is not loaded yet"),
        }
    }
}
