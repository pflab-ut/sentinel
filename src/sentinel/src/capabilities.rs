use caps::{CapSet, CapsHashSet};
use libcontainer::capabilities::CapabilityExt;
use oci_spec::runtime::{Capabilities, LinuxCapabilities};

pub fn set_capabilities(cset: CapSet, value: &CapsHashSet) -> anyhow::Result<()> {
    match cset {
        CapSet::Bounding => {
            let all = caps::all();
            for c in all.difference(value) {
                caps::drop(None, CapSet::Bounding, *c)?;
            }
        }
        _ => {
            caps::set(None, cset, value)?;
        }
    }
    Ok(())
}

pub fn drop_privileges(cs: &LinuxCapabilities) -> anyhow::Result<()> {
    if let Some(bounding) = cs.bounding() {
        set_capabilities(CapSet::Bounding, &to_set(bounding))?;
    }
    if let Some(effective) = cs.effective() {
        set_capabilities(CapSet::Effective, &to_set(effective))?;
    }
    if let Some(permitted) = cs.permitted() {
        set_capabilities(CapSet::Permitted, &to_set(permitted))?;
    }
    if let Some(inheritable) = cs.inheritable() {
        set_capabilities(CapSet::Inheritable, &to_set(inheritable))?;
    }
    if let Some(ambient) = cs.ambient() {
        set_capabilities(CapSet::Ambient, &to_set(ambient))?;
    }
    Ok(())
}

fn to_set(caps: &Capabilities) -> CapsHashSet {
    let mut capabilities = CapsHashSet::new();
    for c in caps {
        capabilities.insert(c.to_cap());
    }
    capabilities
}
