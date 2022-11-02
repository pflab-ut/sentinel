pub mod capability_set;
pub mod credentials;
pub mod id;
pub mod user_namespace;

use credentials::Credentials;

pub trait Context {
    fn credentials(&self) -> &Credentials;
}
