use std::rc::Rc;

use linux::Capability;

use super::{
    capability_set::{CapabilitySet, TaskCapabilities},
    id::{Kgid, Kuid},
    user_namespace::UserNamespace,
};

#[derive(Clone, Debug)]
pub struct Credentials {
    pub real_kuid: Kuid,
    pub effective_kuid: Kuid,
    pub saved_kuid: Kuid,
    pub real_kgid: Kgid,
    pub effective_kgid: Kgid,
    pub saved_kgid: Kgid,
    pub extra_kgids: Vec<Kgid>,
    pub permitted_caps: CapabilitySet,
    pub effective_caps: CapabilitySet,
    pub inheritable_caps: CapabilitySet,
    pub bounding_caps: CapabilitySet,
    pub user_namespace: Rc<UserNamespace>,
}

impl Credentials {
    pub fn new_anonymous() -> Self {
        Self {
            real_kuid: Kuid::nobody(),
            effective_kuid: Kuid::nobody(),
            saved_kuid: Kuid::nobody(),
            real_kgid: Kgid::nobody(),
            effective_kgid: Kgid::nobody(),
            saved_kgid: Kgid::nobody(),
            extra_kgids: Vec::new(),
            permitted_caps: CapabilitySet::default(),
            effective_caps: CapabilitySet::default(),
            inheritable_caps: CapabilitySet::default(),
            bounding_caps: CapabilitySet::default(),
            user_namespace: Rc::new(UserNamespace::new_root()),
        }
    }

    pub fn new_root(ns: Rc<UserNamespace>) -> Self {
        Self {
            real_kuid: Kuid::root(),
            effective_kuid: Kuid::root(),
            saved_kuid: Kuid::root(),
            real_kgid: Kgid::root(),
            effective_kgid: Kgid::root(),
            saved_kgid: Kgid::root(),
            extra_kgids: Vec::new(),
            permitted_caps: CapabilitySet::all(),
            effective_caps: CapabilitySet::all(),
            inheritable_caps: CapabilitySet::default(),
            bounding_caps: CapabilitySet::all(),
            user_namespace: ns,
        }
    }

    pub fn new_user(
        kuid: Kuid,
        kgid: Kgid,
        capabilities: Option<&TaskCapabilities>,
        ns: Rc<UserNamespace>,
    ) -> Self {
        let mut creds = Credentials::new_root(ns);
        let uid = kuid;

        creds.real_kuid = uid;
        creds.effective_kuid = uid;
        creds.saved_kuid = uid;

        let gid = kgid;
        creds.real_kgid = gid;
        creds.effective_kgid = gid;
        creds.saved_kgid = gid;
        match capabilities {
            Some(capabilities) => {
                creds.permitted_caps = capabilities.permitted_caps;
                creds.effective_caps = capabilities.effective_caps;
                creds.bounding_caps = capabilities.bounding_caps;
                creds.inheritable_caps = capabilities.inheritable_caps;
            }
            None => {
                if kuid == Kuid::root() {
                    creds.permitted_caps = CapabilitySet::all();
                    creds.effective_caps = CapabilitySet::all();
                } else {
                    creds.permitted_caps = CapabilitySet(0);
                    creds.effective_caps = CapabilitySet(0);
                }
                creds.bounding_caps = CapabilitySet::all();
            }
        }
        creds
    }

    pub fn has_capability_in(&self, cp: &Capability, mut ns: Rc<UserNamespace>) -> bool {
        loop {
            if Rc::as_ptr(&self.user_namespace) == Rc::as_ptr(&ns) {
                return CapabilitySet::from_capability(cp).0 & self.effective_caps.0 != 0;
            }
            match ns.parent.upgrade() {
                Some(ref parent) => {
                    if Rc::as_ptr(&self.user_namespace) == Rc::as_ptr(parent)
                        && self.effective_kuid == ns.owner
                    {
                        return true;
                    } else {
                        ns = Rc::clone(parent);
                    }
                }
                None => return false,
            };
        }
    }

    pub fn has_capability(&self, cp: &Capability) -> bool {
        self.has_capability_in(cp, self.user_namespace.clone())
    }

    pub fn in_group(&self, kgid: Kgid) -> bool {
        if self.effective_kgid == kgid {
            return true;
        }
        self.extra_kgids
            .iter()
            .any(|extra_kgid| *extra_kgid == kgid)
    }
}
