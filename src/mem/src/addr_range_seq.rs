use super::AddrRange;

#[derive(Default, Debug, PartialEq, Eq)]
pub struct AddrRangeSeq {
    data: Vec<AddrRange>,
    length: usize,
    offset: usize,
    limit: usize,
}

impl AddrRangeSeq {
    pub fn as_view(&self) -> AddrRangeSeqView<'_> {
        match self.data.len() {
            0 => AddrRangeSeqView::default(),
            _ => AddrRangeSeqView {
                data: &self.data,
                length: self.length,
                offset: self.offset,
                limit: self.limit,
            },
        }
    }

    pub fn from(range: AddrRange) -> Self {
        Self {
            data: vec![range],
            length: 1,
            offset: range.start as usize,
            limit: range.len() as usize,
        }
    }

    pub fn from_slice(slice: &[AddrRange]) -> Self {
        let limit = slice.iter().fold(0, |acc: usize, x| {
            acc.checked_add(x.len() as usize).expect("overflow")
        });
        match slice.len() {
            0 => AddrRangeSeq::default(),
            1 => AddrRangeSeq {
                data: Vec::new(),
                length: 1,
                offset: slice[0].start as usize,
                limit,
            },
            n => AddrRangeSeq {
                data: slice.to_vec(),
                length: n,
                offset: 0,
                limit,
            },
        }
    }

    pub fn truncate_to_first(&mut self, n: usize) {
        self.limit = std::cmp::min(self.limit, n);
    }

    pub fn num_bytes(&self) -> usize {
        self.limit
    }

    pub fn num_ranges(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn head(&self) -> AddrRange {
        match self.length {
            0 => panic!("empty AddrRangeSeq"),
            1 => AddrRange {
                start: self.offset as u64,
                end: (self.offset + self.limit) as u64,
            },
            _ => {
                let mut ar = *self.data.first().unwrap();
                ar.start += self.offset as u64;
                if ar.len() > self.limit as u64 {
                    ar.end = ar.start + self.limit as u64;
                }
                ar
            }
        }
    }

    fn clear(&mut self) {
        self.data.clear();
        self.length = 0;
        self.offset = 0;
        self.limit = 0;
    }

    pub fn drop_first(&mut self, mut n: usize) {
        if n > self.limit {
            return self.clear();
        }

        if self.length == 0 {
            return self.clear();
        } else if self.length == 1 {
            if self.limit == 0 {
                return self.clear();
            }
        } else {
            let raw_head_len = self.data.first().unwrap().len() as usize;
            if self.offset == raw_head_len {
                self.external_drop_head();
            }
        }

        while n != 0 {
            let head_len = if self.length == 1 {
                self.limit
            } else {
                self.data.first().unwrap().len() as usize - self.offset
            };
            if n < head_len {
                self.offset += n;
                self.limit -= n;
                return;
            }
            n -= head_len;
            self.drop_head();
        }
    }

    fn drop_head(&mut self) {
        match self.length {
            0 => panic!("empty AddrRangeSeq"),
            1 => self.clear(),
            _ => self.external_drop_head(),
        }
    }

    fn external_drop_head(&mut self) {
        let head_len = self.data.first().unwrap().len() as usize - self.offset;
        let tail_limit = if self.limit > head_len {
            self.limit - head_len
        } else {
            0
        };
        match self.data.len() - 1 {
            0 => self.clear(),
            1 => {
                self.length = 1;
                self.offset = self.data[1].start as usize;
                self.data.clear();
                self.limit = tail_limit;
            }
            n => {
                self.data = self.data.drain(1..).collect();
                self.length = n;
                self.offset = 0;
                self.limit = tail_limit;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AddrRangeSeqView<'a> {
    data: &'a [AddrRange],
    length: usize,
    offset: usize,
    limit: usize,
}

impl<'a> AddrRangeSeqView<'a> {
    pub fn from_slice(data: &'a [AddrRange]) -> Self {
        let limit = data.iter().fold(0, |sum: usize, x| {
            sum.checked_add(x.len() as usize).expect("overflow")
        });
        Self::from_slice_and_limit(data, limit)
    }

    fn from_slice_and_limit(data: &'a [AddrRange], limit: usize) -> Self {
        match data.len() {
            0 => Self::default(),
            1 => Self {
                data: &[],
                length: 1,
                offset: data[0].start as usize,
                limit,
            },
            n => Self {
                data,
                length: n,
                offset: 0,
                limit,
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn num_bytes(&self) -> usize {
        self.limit
    }

    pub fn num_ranges(&self) -> usize {
        self.length
    }

    pub fn head(&self) -> AddrRange {
        match self.length {
            0 => panic!("empty AddrRangeSeq"),
            1 => AddrRange {
                start: self.offset as u64,
                end: (self.offset + self.limit) as u64,
            },
            _ => {
                let mut ar = *self.data.first().unwrap();
                ar.start += self.offset as u64;
                if ar.len() > self.limit as u64 {
                    ar.end = ar.start + self.limit as u64;
                }
                ar
            }
        }
    }

    pub fn take_first(mut self, n: usize) -> Self {
        self.limit = std::cmp::min(self.limit, n);
        self
    }

    pub fn drop_first(mut self, mut n: usize) -> Self {
        if n > self.limit {
            return Self::default();
        }

        if self.length == 0 {
            return Self::default();
        } else if self.length == 1 {
            if self.limit == 0 {
                return Self::default();
            }
        } else {
            let raw_head_len = self.data.first().unwrap().len();
            if self.offset == raw_head_len as usize {
                self = self.external_tail();
            }
        }

        while n != 0 {
            let head_len = if self.length == 1 {
                self.limit
            } else {
                self.data.first().unwrap().len() as usize - self.offset
            };
            if n < head_len {
                self.offset += n;
                self.limit -= n;
                return self;
            }
            n -= head_len;
            self = self.tail();
        }
        self
    }

    pub fn tail(self) -> Self {
        match self.length {
            0 => panic!("empty AddrRangeSeq"),
            1 => Self::default(),
            _ => self.external_tail(),
        }
    }

    fn external_tail(self) -> Self {
        let head_len = self.data.first().unwrap().len() as usize - self.offset;
        let limit = if self.limit > head_len {
            self.limit - head_len
        } else {
            0
        };
        Self::from_slice_and_limit(&self.data[1..], limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use once_cell::sync::Lazy;

    struct Test {
        _desc: String,
        ranges: Vec<AddrRange>,
    }

    static ADDR_RANGE_SEQ_TESTS: Lazy<[Test; 6]> = Lazy::new(|| {
        [
            Test {
                _desc: "Empty sequence".to_string(),
                ranges: Vec::new(),
            },
            Test {
                _desc: "Single empty AddrRange".to_string(),
                ranges: vec![AddrRange {
                    start: 0x10,
                    end: 0x10,
                }],
            },
            Test {
                _desc: "Single non-empty AddrRange of length 1".to_string(),
                ranges: vec![AddrRange {
                    start: 0x10,
                    end: 0x11,
                }],
            },
            Test {
                _desc: "Single non-empty AddrRange of length 2".to_string(),
                ranges: vec![AddrRange {
                    start: 0x10,
                    end: 0x12,
                }],
            },
            Test {
                _desc: "Multiple non-empty AddrRange".to_string(),
                ranges: vec![
                    AddrRange {
                        start: 0x10,
                        end: 0x11,
                    },
                    AddrRange {
                        start: 0x20,
                        end: 0x22,
                    },
                ],
            },
            Test {
                _desc: "Multiple AddrRanges including empty AddrRanges".to_string(),
                ranges: vec![
                    AddrRange {
                        start: 0x10,
                        end: 0x10,
                    },
                    AddrRange {
                        start: 0x20,
                        end: 0x20,
                    },
                    AddrRange {
                        start: 0x30,
                        end: 0x33,
                    },
                    AddrRange {
                        start: 0x40,
                        end: 0x44,
                    },
                    AddrRange {
                        start: 0x50,
                        end: 0x50,
                    },
                    AddrRange {
                        start: 0x60,
                        end: 0x60,
                    },
                    AddrRange {
                        start: 0x70,
                        end: 0x77,
                    },
                    AddrRange {
                        start: 0x80,
                        end: 0x88,
                    },
                    AddrRange {
                        start: 0x90,
                        end: 0x90,
                    },
                    AddrRange {
                        start: 0xa0,
                        end: 0xa0,
                    },
                ],
            },
        ]
    });

    fn equality_with_tail_iteration(mut ars: AddrRangeSeqView, want_ranges: &[AddrRange]) {
        let mut want_len = 0;
        for ar in want_ranges {
            want_len += ar.len();
        }

        let mut i = 0;
        while !ars.is_empty() {
            assert_eq!(ars.num_bytes(), want_len as usize);
            assert_eq!(ars.num_ranges(), want_ranges.len() - i);
            let got = ars.head();
            assert!(i < want_ranges.len());
            assert_eq!(want_ranges[i], got);
            ars = ars.tail();
            want_len -= got.len();
            i += 1;
        }
        assert_eq!(ars.num_bytes(), 0);
        assert_eq!(want_len, 0);
        assert_eq!(ars.num_ranges(), 0);
    }

    #[test]
    fn tail_iteration() {
        for test in ADDR_RANGE_SEQ_TESTS.iter() {
            equality_with_tail_iteration(AddrRangeSeqView::from_slice(&test.ranges), &test.ranges);
        }
    }

    #[test]
    fn drop_first_empty() {
        let mut empty = AddrRangeSeq::default();
        empty.drop_first(1);
        assert_eq!(empty, empty);
    }

    #[test]
    fn drop_single_byte_iteration() {
        for test in ADDR_RANGE_SEQ_TESTS.iter() {
            let mut want_len = 0;
            let mut want_ranges = Vec::new();
            for range in &test.ranges {
                let mut ar = *range;
                want_len += ar.len() as usize;
                want_ranges.push(ar);
                if ar.is_empty() {
                    continue;
                }
                ar.start += 1;
                while !ar.is_empty() {
                    want_ranges.push(ar);
                    ar.start += 1;
                }
            }

            let mut ars = AddrRangeSeq::from_slice(&test.ranges);
            let mut i = 0;
            while !ars.is_empty() {
                assert_eq!(ars.num_bytes(), want_len);
                let got = ars.head();
                assert!(i < want_ranges.len() as i32);
                assert_eq!(want_ranges[i as usize], got);
                if got.is_empty() {
                    ars.drop_first(0)
                } else {
                    want_len -= 1;
                    ars.drop_first(1)
                }
                i += 1;
            }
            assert!(ars.num_bytes() == 0);
            assert!(want_len == 0);
        }
    }

    #[test]
    fn take_first_empty() {
        let mut empty = AddrRangeSeq::default();
        empty.truncate_to_first(1);
        assert_eq!(empty, AddrRangeSeq::default());
    }

    #[test]
    fn take_first() {
        let ranges = vec![
            AddrRange {
                start: 0x10,
                end: 0x11,
            },
            AddrRange {
                start: 0x20,
                end: 0x22,
            },
            AddrRange {
                start: 0x30,
                end: 0x30,
            },
            AddrRange {
                start: 0x40,
                end: 0x44,
            },
            AddrRange {
                start: 0x50,
                end: 0x55,
            },
            AddrRange {
                start: 0x60,
                end: 0x60,
            },
            AddrRange {
                start: 0x70,
                end: 0x77,
            },
        ];
        let ars = AddrRangeSeqView::from_slice(&ranges).take_first(5);
        let want = vec![
            AddrRange {
                start: 0x10,
                end: 0x11,
            },
            AddrRange {
                start: 0x20,
                end: 0x22,
            },
            AddrRange {
                start: 0x30,
                end: 0x30,
            },
            AddrRange {
                start: 0x40,
                end: 0x42,
            },
            AddrRange {
                start: 0x50,
                end: 0x50,
            },
            AddrRange {
                start: 0x60,
                end: 0x60,
            },
            AddrRange {
                start: 0x70,
                end: 0x70,
            },
        ];
        equality_with_tail_iteration(ars, &want);
    }
}
