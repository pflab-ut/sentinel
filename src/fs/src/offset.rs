use mem::Addr;

pub fn offset_page_end(offset: i64) -> u64 {
    Addr(offset as u64)
        .round_up()
        .expect("impossible overflow")
        .0
}

pub fn read_end_offset(offset: i64, length: i64, size: i64) -> i64 {
    if offset >= size {
        return offset;
    }
    let end = offset + length;
    if end < offset || end > size {
        size
    } else {
        end
    }
}

pub fn write_end_offset(offset: i64, length: i64) -> i64 {
    read_end_offset(offset, length, i64::MAX)
}
