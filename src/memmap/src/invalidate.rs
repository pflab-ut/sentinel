use mem::AddrRange;

#[derive(Clone, Copy, Debug)]
pub struct InvalidateOpts {
    pub invalidate_private: bool,
}

pub trait MemoryInvalidator {
    fn invalidate(&mut self, ar: AddrRange, opts: InvalidateOpts);
}
