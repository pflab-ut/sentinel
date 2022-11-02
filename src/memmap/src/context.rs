use std::{cell::RefCell, rc::Rc};

use super::MemoryInvalidator;

pub trait Context {
    fn mm(&self) -> Rc<RefCell<dyn MemoryInvalidator>>;
}
