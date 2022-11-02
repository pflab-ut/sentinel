use std::rc::{Rc, Weak};

use utils::Range;

use segment::{Set, SetOperations};

use super::id::{Gid, Kgid, Kuid, Uid, NO_ID};

type IdMapSet = Set<u32, u32>;

#[derive(Copy, Clone)]
struct IdMapSetOperations;
impl SetOperations for IdMapSetOperations {
    type K = u32;
    type V = u32;
    fn merge(
        &self,
        r1: Range<Self::K>,
        v1: &Self::V,
        _r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if *v1 + r1.len() != *v2 {
            None
        } else {
            Some(*v1)
        }
    }

    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
        (*v, *v + (split - r.start))
    }
}

#[derive(Debug)]
pub struct UserNamespace {
    pub parent: Weak<UserNamespace>,
    pub owner: Kuid,
    uid_map_from_parent: IdMapSet,
    uid_map_to_parent: IdMapSet,
    gid_map_from_parent: IdMapSet,
    gid_map_to_parent: IdMapSet,
}

impl Default for UserNamespace {
    fn default() -> Self {
        let ops = Box::new(IdMapSetOperations);
        Self {
            parent: Weak::new(),
            owner: Kuid(0),
            uid_map_from_parent: IdMapSet::new(ops.clone()),
            uid_map_to_parent: IdMapSet::new(ops.clone()),
            gid_map_from_parent: IdMapSet::new(ops.clone()),
            gid_map_to_parent: IdMapSet::new(ops),
        }
    }
}

impl UserNamespace {
    pub fn new_root() -> Self {
        let mut ns = UserNamespace::default();
        let r = Range {
            start: 0,
            end: u32::MAX,
        };
        if !ns.uid_map_from_parent.add(r, 0) {
            panic!("failed to insert into empty ID map");
        }
        if !ns.uid_map_to_parent.add(r, 0) {
            panic!("failed to insert into empty ID map");
        }
        if !ns.gid_map_from_parent.add(r, 0) {
            panic!("failed to insert into empty ID map");
        }
        if !ns.gid_map_to_parent.add(r, 0) {
            panic!("failed to insert into empty ID map");
        }
        ns
    }

    pub fn get_root(ns: &Rc<Self>) -> Rc<Self> {
        match ns.parent.upgrade() {
            Some(ref parent) => Self::get_root(parent),
            None => Rc::clone(ns),
        }
    }

    pub fn map_from_kuid(&self, kuid: &Kuid) -> Uid {
        match self.parent.upgrade() {
            None => Uid(kuid.0),
            Some(ref parent) => {
                Uid(self.map_id(&self.uid_map_from_parent, parent.map_from_kuid(kuid).0))
            }
        }
    }

    pub fn map_from_kgid(&self, kgid: &Kgid) -> Gid {
        match self.parent.upgrade() {
            None => Gid(kgid.0),
            Some(ref parent) => {
                Gid(self.map_id(&self.gid_map_from_parent, parent.map_from_kgid(kgid).0))
            }
        }
    }

    fn map_id(&self, m: &IdMapSet, id: u32) -> u32 {
        if id == NO_ID {
            return NO_ID;
        }
        match m.find_segment(id as u32) {
            Some(it) => m.value(&it) + (id - it.start() as u32),
            None => NO_ID,
        }
    }
}
