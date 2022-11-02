pub const NO_ID: u32 = u32::MAX;

macro_rules! define_id {
    ($id:ident) => {
        #[derive(Default, PartialEq, Eq, Clone, Copy, Debug)]
        pub struct $id(pub u32);
        impl $id {
            pub const fn is_ok(&self) -> bool {
                self.0 != NO_ID
            }
            pub const fn or_overflow(self) -> Self {
                if self.0 == NO_ID {
                    Self(65534)
                } else {
                    self
                }
            }
        }
    };
}

define_id!(Uid);
define_id!(Gid);
define_id!(Kuid);
define_id!(Kgid);

macro_rules! define_nobody {
    ($id:ident) => {
        impl $id {
            pub const fn nobody() -> Self {
                Self(65534)
            }
        }
    };
}

define_nobody!(Kuid);
define_nobody!(Kgid);

macro_rules! define_root {
    ($id:ident) => {
        impl $id {
            pub const fn root() -> Self {
                Self(0)
            }
        }
    };
}

define_root!(Uid);
define_root!(Gid);
define_root!(Kuid);
define_root!(Kgid);
