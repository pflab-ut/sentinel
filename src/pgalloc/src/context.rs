use crate::MemoryFileProvider;

pub trait Context {
    fn memory_file_provider(&self) -> &dyn MemoryFileProvider;
}
