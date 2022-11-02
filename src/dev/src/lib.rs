use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use once_cell::sync::Lazy;

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct Id {
    pub major: u64,
    pub minor: u64,
}

impl Id {
    pub fn device_id(&self) -> u64 {
        linux::dev::make_device_id(self.major as u16, self.minor as u32) as u64
    }
}

pub struct Registry {
    last_anonymous_device_minor: AtomicU64,
    devices: HashMap<Id, Arc<Mutex<Device>>>,
}

impl Registry {
    fn new_anonymous_id(&self) -> Id {
        self.last_anonymous_device_minor
            .fetch_add(1, Ordering::SeqCst);
        Id {
            major: 0,
            minor: self.last_anonymous_device_minor.load(Ordering::SeqCst),
        }
    }

    pub fn new_anonymous_device(&mut self) -> Arc<Mutex<Device>> {
        let id = self.new_anonymous_id();
        let d = Arc::new(Mutex::new(Device {
            id,
            last: AtomicU64::new(0),
        }));
        self.devices.insert(id, Arc::clone(&d));
        d
    }
}

pub struct Device {
    id: Id,
    last: AtomicU64, // last generated inode
}

impl Device {
    pub fn new_anonymous_device() -> Arc<Mutex<Device>> {
        SIMPLE_DEVICES.lock().unwrap().new_anonymous_device()
    }

    pub fn device_id(&self) -> u64 {
        linux::dev::make_device_id(self.id.major as u16, self.id.minor as u32) as u64
    }

    pub fn next_ino(&self) -> u64 {
        self.last.fetch_add(1, Ordering::SeqCst);
        self.last.load(Ordering::SeqCst)
    }
}

static SIMPLE_DEVICES: Lazy<Mutex<Registry>> = Lazy::new(|| {
    Mutex::new(Registry {
        last_anonymous_device_minor: AtomicU64::new(0),
        devices: HashMap::new(),
    })
});
