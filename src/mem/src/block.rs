use std::cmp::min;

use utils::SysResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    start: *const u8,
    length: i32,
    need_safe_copy: bool,
}

impl Default for Block {
    fn default() -> Block {
        Block {
            start: std::ptr::null(),
            length: 0,
            need_safe_copy: false,
        }
    }
}

//TODO: implement Drop for Block

impl Block {
    pub fn new(start: *const u8, length: i32, need_safe_copy: bool) -> Block {
        Block {
            start,
            length,
            need_safe_copy,
        }
    }

    pub fn from_slice(slice: &[u8], need_safe_copy: bool) -> Self {
        if slice.is_empty() {
            Block::default()
        } else {
            Block {
                start: slice.as_ptr(),
                length: slice.len() as i32,
                need_safe_copy,
            }
        }
    }

    #[inline]
    pub fn start(&self) -> *const u8 {
        self.start
    }

    #[inline]
    pub fn start_mut(&mut self) -> *mut u8 {
        self.start as *mut _
    }

    #[inline]
    pub fn len(&self) -> i32 {
        self.length
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[inline]
    pub fn need_safe_copy(&self) -> bool {
        self.need_safe_copy
    }

    pub fn take_first64(&self, n: u64) -> Block {
        if n == 0 {
            Block::default()
        } else if n >= self.length as u64 {
            *self
        } else {
            Block {
                length: n as i32,
                ..*self
            }
        }
    }

    pub fn drop_first(&self, n: i32) -> Block {
        if n < 0 {
            panic!("invalid n: {}", n);
        }
        self.drop_first64(n as u64)
    }

    pub fn drop_first64(&self, n: u64) -> Block {
        if n >= self.length as u64 {
            Block::default()
        } else {
            Block::new(
                unsafe { self.start.offset(n as isize) },
                self.length - n as i32,
                self.need_safe_copy,
            )
        }
    }

    pub unsafe fn as_slice(&self) -> &[u8] {
        std::slice::from_raw_parts(self.start, self.length as usize)
    }

    pub unsafe fn as_slice_mut(&mut self) -> &mut [u8] {
        std::slice::from_raw_parts_mut(self.start as *mut _, self.length as usize)
    }
}

pub fn copy(dst: &mut Block, src: &Block) -> SysResult<usize> {
    let count = min(src.len(), dst.len()) as usize;
    unsafe { std::ptr::copy_nonoverlapping(src.start, dst.start_mut(), count) };
    Ok(count)
}

pub fn zero(dst: &mut Block) -> SysResult<usize> {
    if !dst.need_safe_copy {
        let bs = unsafe { dst.as_slice_mut() };
        bs.fill(0);
        Ok(bs.len())
    } else {
        todo!();
    }
}
