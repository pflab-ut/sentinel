pub mod memory;

#[derive(PartialEq, Copy, Clone, Debug)]
pub enum MemoryKind {
    System,
    Anonymous,
    PageCache,
    Tmpfs,
    Ramdiskfs,
    Mapped,
}
