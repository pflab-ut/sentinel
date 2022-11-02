// memory manager is implemented based on gVisor's mm package.
// cf) https://github.com/google/gvisor/blob/master/pkg/sentry/mm/mm.go

mod memory_manager;

pub use memory_manager::*;
