use linux::Capability;
use utils::bit;

#[derive(Clone, Copy, Default, Debug)]
pub struct CapabilitySet(pub u64);

impl CapabilitySet {
    pub fn from_capability(cp: &Capability) -> CapabilitySet {
        CapabilitySet(bit::mask_of::<u64>(cp.0))
    }

    pub const fn all() -> Self {
        Self((1 << (Capability::last_cap().0 + 1)) - 1)
    }
}

pub struct TaskCapabilities {
    pub permitted_caps: CapabilitySet,
    pub inheritable_caps: CapabilitySet,
    pub effective_caps: CapabilitySet,
    pub bounding_caps: CapabilitySet,
    pub ambient_caps: CapabilitySet,
}
