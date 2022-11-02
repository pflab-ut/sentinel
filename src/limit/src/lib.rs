use utils::{bail_libc, SysError, SysResult};

#[derive(Clone, Copy, Default, Debug)]
pub struct Limit {
    pub cur: u64,
    pub max: u64,
}

impl Limit {
    #[inline]
    pub fn from_libc_rlimit64(rlimit64: &libc::rlimit64) -> Self {
        Self {
            cur: rlimit64.rlim_cur,
            max: rlimit64.rlim_max,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LimitSet {
    cpu: Option<Limit>,
    file_size: Option<Limit>,
    data: Option<Limit>,
    stack: Option<Limit>,
    core: Option<Limit>,
    rss: Option<Limit>,
    process_count: Option<Limit>,
    number_of_files: Option<Limit>,
    memory_locked: Option<Limit>,
    address_space: Option<Limit>,
    locks: Option<Limit>,
    signals_pending: Option<Limit>,
    message_queue_bytes: Option<Limit>,
    nice: Option<Limit>,
    real_time_priority: Option<Limit>,
    rtt_time: Option<Limit>,
}

impl Default for LimitSet {
    fn default() -> Self {
        Self {
            cpu: None,
            file_size: None,
            data: None,
            stack: Some(Limit {
                cur: 0x800000,
                max: INFINITY,
            }),
            core: None,
            rss: None,
            process_count: None,
            number_of_files: None,
            memory_locked: None,
            address_space: None,
            locks: None,
            signals_pending: None,
            message_queue_bytes: None,
            nice: None,
            real_time_priority: None,
            rtt_time: None,
        }
    }
}

pub fn is_valid_resource(resource: u32) -> bool {
    matches!(
        resource,
        libc::RLIMIT_CPU
            | libc::RLIMIT_FSIZE
            | libc::RLIMIT_DATA
            | libc::RLIMIT_STACK
            | libc::RLIMIT_CORE
            | libc::RLIMIT_RSS
            | libc::RLIMIT_NPROC
            | libc::RLIMIT_NOFILE
            | libc::RLIMIT_MEMLOCK
            | libc::RLIMIT_AS
            | libc::RLIMIT_LOCKS
            | libc::RLIMIT_SIGPENDING
            | libc::RLIMIT_MSGQUEUE
            | libc::RLIMIT_NICE
            | libc::RLIMIT_RTPRIO
            | libc::RLIMIT_RTTIME
    )
}

pub const INFINITY: u64 = u64::MAX;

macro_rules! get_field {
    ($fn:ident, $field:ident) => {
        pub fn $fn(&self) -> Limit {
            self.$field.unwrap_or(Limit {
                cur: INFINITY,
                max: INFINITY,
            })
        }
    };
}

macro_rules! set_field {
    ($fn:ident, $field:ident) => {
        pub fn $fn(&mut self, v: Limit, privileged: bool) -> SysResult<Limit> {
            let old = self.$field;
            if let Some(limit) = old {
                if limit.max < v.max && !privileged {
                    bail_libc!(libc::EPERM);
                }
                if v.cur > v.max {
                    bail_libc!(libc::EINVAL);
                }
            }
            self.$field = Some(v);
            Ok(old.unwrap_or_else(|| Limit::default()))
        }
    };
}

impl LimitSet {
    get_field!(get_cpu, cpu);
    get_field!(get_file_size, file_size);
    get_field!(get_data, data);
    get_field!(get_stack, stack);
    get_field!(get_core, core);
    get_field!(get_rss, rss);
    get_field!(get_process_count, process_count);
    get_field!(get_number_of_files, number_of_files);
    get_field!(get_memory_locked, memory_locked);
    get_field!(get_address_space, address_space);
    get_field!(get_locks, locks);
    get_field!(get_signals_pending, signals_pending);
    get_field!(get_message_queue_bytes, message_queue_bytes);
    get_field!(get_nice, nice);
    get_field!(get_real_time_priority, real_time_priority);
    get_field!(get_rtt_time, rtt_time);

    set_field!(set_cpu, cpu);
    set_field!(set_file_size, file_size);
    set_field!(set_data, data);
    set_field!(set_stack, stack);
    set_field!(set_core, core);
    set_field!(set_rss, rss);
    set_field!(set_process_count, process_count);
    set_field!(set_number_of_files, number_of_files);
    set_field!(set_memory_locked, memory_locked);
    set_field!(set_address_space, address_space);
    set_field!(set_locks, locks);
    set_field!(set_signals_pending, signals_pending);
    set_field!(set_message_queue_bytes, message_queue_bytes);
    set_field!(set_nice, nice);
    set_field!(set_real_time_priority, real_time_priority);
    set_field!(set_rtt_time, rtt_time);

    pub fn get_resource(&self, resource: u32) -> Limit {
        match resource {
            libc::RLIMIT_CPU => self.get_cpu(),
            libc::RLIMIT_FSIZE => self.get_file_size(),
            libc::RLIMIT_DATA => self.get_data(),
            libc::RLIMIT_STACK => self.get_stack(),
            libc::RLIMIT_CORE => self.get_core(),
            libc::RLIMIT_RSS => self.get_rss(),
            libc::RLIMIT_NPROC => self.get_process_count(),
            libc::RLIMIT_NOFILE => self.get_number_of_files(),
            libc::RLIMIT_MEMLOCK => self.get_memory_locked(),
            libc::RLIMIT_AS => self.get_address_space(),
            libc::RLIMIT_LOCKS => self.get_locks(),
            libc::RLIMIT_SIGPENDING => self.get_signals_pending(),
            libc::RLIMIT_MSGQUEUE => self.get_message_queue_bytes(),
            libc::RLIMIT_NICE => self.get_nice(),
            libc::RLIMIT_RTPRIO => self.get_real_time_priority(),
            libc::RLIMIT_RTTIME => self.get_rtt_time(),
            _ => unreachable!("invalid case should be handled before entering this function"),
        }
    }

    pub fn get_resource_capped(&self, resource: u32, max: u64) -> u64 {
        let s = self.get_resource(resource);
        if s.cur == INFINITY || s.cur > max {
            max
        } else {
            s.cur
        }
    }

    pub fn set_resource(&mut self, resource: u32, v: Limit, privileged: bool) -> SysResult<Limit> {
        match resource {
            libc::RLIMIT_CPU => self.set_cpu(v, privileged),
            libc::RLIMIT_FSIZE => self.set_file_size(v, privileged),
            libc::RLIMIT_DATA => self.set_data(v, privileged),
            libc::RLIMIT_STACK => self.set_stack(v, privileged),
            libc::RLIMIT_CORE => self.set_core(v, privileged),
            libc::RLIMIT_RSS => self.set_rss(v, privileged),
            libc::RLIMIT_NPROC => self.set_process_count(v, privileged),
            libc::RLIMIT_NOFILE => self.set_number_of_files(v, privileged),
            libc::RLIMIT_MEMLOCK => self.set_memory_locked(v, privileged),
            libc::RLIMIT_AS => self.set_address_space(v, privileged),
            libc::RLIMIT_LOCKS => self.set_locks(v, privileged),
            libc::RLIMIT_SIGPENDING => self.set_signals_pending(v, privileged),
            libc::RLIMIT_MSGQUEUE => self.set_message_queue_bytes(v, privileged),
            libc::RLIMIT_NICE => self.set_nice(v, privileged),
            libc::RLIMIT_RTPRIO => self.set_real_time_priority(v, privileged),
            libc::RLIMIT_RTTIME => self.set_rtt_time(v, privileged),
            _ => bail_libc!(libc::EINVAL),
        }
    }
}

pub trait Context {
    fn limits(&self) -> LimitSet;
}
