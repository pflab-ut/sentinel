use std::ops::{Add, Sub};

#[derive(Clone, Copy, Default, Debug, PartialEq, PartialOrd)]
pub struct Time {
    ns: u128,
}

impl Time {
    pub fn from_unix(s: i64, ns: i64) -> Self {
        if s > i64::MAX / (1e9 as i64) {
            Self {
                ns: i64::MAX as u128,
            }
        } else {
            let t = s * (1e9 as i64);
            if t > i64::MAX - ns {
                Self {
                    ns: i64::MAX as u128,
                }
            } else {
                Self {
                    ns: (t + ns) as u128,
                }
            }
        }
    }

    pub fn as_libc_timespec(&self) -> libc::timespec {
        libc::timespec {
            tv_sec: (self.ns / 1e9 as u128) as i64,
            tv_nsec: (self.ns % 1e9 as u128) as i64,
        }
    }

    pub fn seconds(&self) -> i64 {
        (self.ns / (1e9 as u128)) as i64
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, rhs: Self) -> Self::Output {
        Time {
            ns: self.ns + rhs.ns,
        }
    }
}

impl Sub for Time {
    type Output = Time;
    fn sub(self, rhs: Self) -> Self::Output {
        Time {
            ns: self.ns - rhs.ns,
        }
    }
}

pub trait Clock {
    fn now(&self) -> Time;
    fn sleep(&self, duration: Time);
}

#[derive(Clone, Copy, Debug)]
pub struct HostClock;

impl Clock for HostClock {
    fn now(&self) -> Time {
        let now = std::time::SystemTime::now();
        let ns = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        if ns > i64::MAX as u128 {
            panic!("currenttime overflows i64")
        }
        Time { ns }
    }

    fn sleep(&self, duration: Time) {
        std::thread::sleep(std::time::Duration::from_nanos(duration.ns as u64));
    }
}

pub trait Context {
    fn now(&self) -> Time;
}
