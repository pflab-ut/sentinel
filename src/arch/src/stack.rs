use std::collections::HashMap;

use mem::{Addr, IoOpts};
use utils::SysResult;

pub struct Stack {
    bottom: Addr,
}

impl Stack {
    pub fn new(bottom: Addr) -> Self {
        Self { bottom }
    }

    pub fn bottom(&self) -> u64 {
        self.bottom.0
    }

    pub fn push(&mut self, val: StackVal<'_>, mm: &mut dyn mem::io::Io) -> SysResult<Addr> {
        match val {
            StackVal::Byte(b) => {
                self.copy_out_byte(b, mm)?;
            }
            StackVal::Bytes(bytes) => {
                self.copy_out_byte(0, mm)?;
                self.copy_out_bytes(bytes, mm)?;
            }
            StackVal::Addr(addr) => {
                self.copy_out_u64(addr.0, mm)?;
            }
            StackVal::AddrSlice(addrs) => {
                self.copy_out_u64(0, mm)?;
                for v in addrs.iter().rev() {
                    self.copy_out_u64(v.0, mm)?;
                }
            }
        }
        Ok(self.bottom)
    }

    pub fn load(
        &mut self,
        args: &[String],
        envs: &HashMap<String, String>,
        auxv: &HashMap<u64, Addr>,
        mm: &mut dyn mem::io::Io,
    ) -> SysResult<StackLayout> {
        let mut layout = StackLayout::default();
        self.align(16);

        layout.envv_end = self.bottom;
        let env_len = envs.len();
        let envv_addrs: Vec<_> = envs
            .iter()
            .map(|(k, v)| {
                let e = format!("{}={}", k, v);
                logger::info!("loading stack env: {} {}", self.bottom, e);
                self.push(StackVal::Bytes(e.as_bytes()), mm).unwrap() //FIXME: don't unwrap
            })
            .collect();
        layout.envv_start = self.bottom;

        layout.argv_end = self.bottom;
        let args_len = args.len();
        let argv_addrs: Vec<_> = args
            .iter()
            .map(|a| {
                logger::info!("loading stack argv: {}", self.bottom);
                self.push(StackVal::Bytes(a.as_bytes()), mm).unwrap() //FIXME: don't unwrap
            })
            .collect();
        layout.argv_start = self.bottom;

        let argv_size = 8 * (args_len as u64 + 1);
        let envv_size = 8 * (env_len as u64 + 1);
        let auxv_size = 8 * 2 * (auxv.len() as u64 + 1);
        let total = Addr(argv_size) + Addr(envv_size) + Addr(auxv_size) + Addr(8);
        let expected_bottom = self.bottom - total;
        if expected_bottom.0 % 32 != 0 {
            self.bottom.0 -= expected_bottom.0 % 32;
        }

        let mut auxv: Vec<Addr> = auxv
            .iter().flat_map(|(k, v)| vec![Addr(*k), *v])
            .collect();
        auxv.push(Addr(0));

        self.push(StackVal::AddrSlice(&auxv), mm)?;
        self.push(StackVal::AddrSlice(&envv_addrs), mm)?;
        self.push(StackVal::AddrSlice(&argv_addrs), mm)?;
        self.push(StackVal::Addr(Addr(args_len as u64)), mm)?;
        logger::info!("loading stack fin: {}", self.bottom);

        Ok(layout)
    }

    pub fn align(&mut self, offset: i32) {
        let r = self.bottom.0 % (offset as u64);
        if r != 0 {
            self.bottom.0 -= r;
        }
    }

    fn copy_out_byte(&mut self, b: u8, mm: &mut dyn mem::io::Io) -> SysResult<usize> {
        let n = mm.copy_out(self.bottom - Addr(1), &[b], &IoOpts::default())?;
        if n == 1 {
            self.bottom -= Addr(1);
            Ok(1)
        } else {
            todo!();
        }
    }

    fn copy_out_u64(&mut self, n: u64, mm: &mut dyn mem::io::Io) -> SysResult<usize> {
        let src = n.to_le_bytes();
        let n = mm.copy_out(self.bottom - Addr(8), &src, &IoOpts::default())?;
        if n == 8 {
            self.bottom -= Addr(8);
            Ok(8)
        } else {
            todo!();
        }
    }

    fn copy_out_bytes(&mut self, src: &[u8], mm: &mut dyn mem::io::Io) -> SysResult<usize> {
        let c = src.len();
        let n = mm.copy_out(self.bottom - Addr(c as u64), src, &IoOpts::default())?;
        if n == c {
            self.bottom -= Addr(n as u64);
            Ok(n)
        } else {
            todo!();
        }
    }
}

#[derive(Default)]
pub struct StackLayout {
    pub argv_start: Addr,
    pub argv_end: Addr,
    pub envv_start: Addr,
    pub envv_end: Addr,
}

pub enum StackVal<'a> {
    Byte(u8),
    Bytes(&'a [u8]),
    Addr(Addr),
    AddrSlice(&'a [Addr]),
}
