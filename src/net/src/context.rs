use std::sync::RwLockWriteGuard;

use smoltcp::{
    iface::{Interface, SocketHandle},
    phy::TunTapInterface,
    socket::Socket,
    time::Duration,
};

pub trait Context: mem::Context {
    fn add_socket(&self, socket: Socket<'static>) -> SocketHandle;
    fn poll_wait(&self, once: bool);
    fn gen_local_port(&self) -> u16;
    fn remove_local_port(&self, p: u16);
    fn wait(&self, duration: Option<Duration>);
    fn network_interface_mut(&self) -> RwLockWriteGuard<'_, Interface<'static, TunTapInterface>>;

    fn as_net_context(&self) -> &dyn Context;
}
