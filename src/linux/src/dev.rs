pub fn make_device_id(major: u16, minor: u32) -> u32 {
    (minor & 0xff) | (((major as u32) & 0xfff) << 8) | ((minor >> 8) << 20)
}
