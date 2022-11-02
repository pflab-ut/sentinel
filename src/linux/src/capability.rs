#[derive(Clone, Copy)]
pub struct Capability(pub i32);

impl Capability {
    pub const fn dac_override() -> Self {
        Self(1)
    }
    pub const fn dac_read_search() -> Self {
        Self(2)
    }
    pub const fn fowner() -> Self {
        Self(3)
    }
    pub const fn net_raw() -> Self {
        Self(13)
    }
    pub const fn ipc_lock() -> Self {
        Self(14)
    }
    pub const fn cap_sys_nice() -> Self {
        Self(23)
    }
    pub const fn cap_sys_resource() -> Self {
        Self(24)
    }
    pub const fn audit_read() -> Self {
        Self(37)
    }
    pub const fn last_cap() -> Self {
        Self::audit_read()
    }
}
