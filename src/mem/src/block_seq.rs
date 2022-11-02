use std::cmp::min;

use utils::SysResult;

use super::block::{copy, zero, Block};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSeq {
    data: Vec<Block>,
    offset: i32,
    limit: u64,
}

impl BlockSeq {
    pub fn from_blocks(mut blocks: Vec<Block>) -> BlockSeq {
        let slice = match first_non_empty_index(&blocks) {
            Some(i) => blocks.drain(i..).collect::<Vec<_>>(),
            None => blocks,
        };
        let limit: u64 = slice.iter().fold(0, |acc, d| {
            acc.checked_add(d.len() as u64).expect("overflow")
        });
        Self {
            data: slice,
            offset: 0,
            limit,
        }
    }

    pub fn from_block(b: Block) -> BlockSeq {
        BlockSeq {
            data: vec![b],
            offset: 0,
            limit: b.len() as u64,
        }
    }

    pub fn head(&self) -> Block {
        self.data
            .first()
            .expect("empty BlockSeq")
            .drop_first(self.offset)
            .take_first64(self.limit)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn num_bytes(&self) -> u64 {
        self.limit
    }

    pub fn drop_first(&mut self, n: i32) {
        if n < 0 {
            panic!("invalid n: {}", n);
        }
        self.drop_first64(n as u64)
    }

    pub fn drop_first64(&mut self, mut n: u64) {
        if n >= self.limit {
            self.limit = 0;
            self.offset = 0;
            self.data.clear();
            return;
        }
        loop {
            let head_len = {
                let block = self.data.first().unwrap();
                (block.len() - self.offset) as u64
            };
            if n < head_len {
                self.offset += n as i32;
                self.limit -= n;
                return;
            }
            n -= head_len;
            self.drop_head();
        }
    }

    pub fn cut_first(&self, mut n: u64) -> BlockSeq {
        if n >= self.limit {
            return BlockSeq::default();
        }
        let mut bs = self.clone();
        loop {
            let head_len = {
                let block = bs.data.first().unwrap();
                (block.len() - bs.offset) as u64
            };
            if n < head_len {
                bs.offset += n as i32;
                bs.limit -= n;
                return bs;
            }
            n -= head_len;
            bs = bs.tail();
        }
    }

    pub fn take_first(&self, n: i32) -> BlockSeq {
        if n < 0 {
            panic!("invalid n: {}", n);
        }
        self.take_first64(n as u64)
    }

    pub fn take_first64(&self, n: u64) -> BlockSeq {
        if n == 0 {
            BlockSeq::default()
        } else {
            let mut cloned = self.clone();
            cloned.limit = min(cloned.limit, n);
            cloned
        }
    }

    pub fn tail(&self) -> BlockSeq {
        if self.data.is_empty() {
            panic!("empty BlockSeq");
        }
        let head = self.data.first().unwrap().drop_first(self.offset);
        let head_len = head.len() as u64;
        if head_len > self.limit {
            BlockSeq::default()
        } else {
            let tail_slices = skip_empty(&self.data[1..]);
            BlockSeq {
                data: tail_slices.to_vec(),
                offset: 0,
                limit: self.limit - head_len,
            }
        }
    }

    fn drop_head(&mut self) {
        if self.data.is_empty() {
            panic!("empty BlockSeq");
        }
        let head = self.data.first().unwrap().drop_first(self.offset);
        let head_len = head.len() as u64;
        if head_len > self.limit {
            self.data.clear();
            self.offset = 0;
            self.limit = 0;
        } else {
            self.data = skip_empty(&self.data[1..]).to_vec();
            self.limit -= head_len;
            self.offset = 0;
        }
    }

    pub fn as_view(&self) -> BlockSeqView<'_> {
        BlockSeqView {
            data: &self.data,
            offset: self.offset,
            limit: self.limit,
        }
    }
}

#[derive(Default, Copy, Clone)]
pub struct BlockSeqView<'a> {
    data: &'a [Block],
    offset: i32,
    limit: u64,
}

impl<'a> BlockSeqView<'a> {
    pub fn from_slice(data: &'a [Block]) -> Self {
        let data = skip_empty(data);
        let limit: u64 = data.iter().fold(0, |acc, d| {
            acc.checked_add(d.len() as u64).expect("overflow")
        });
        Self {
            data,
            offset: 0,
            limit,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn drop_first(&mut self, mut n: u64) {
        if n >= self.limit {
            self.limit = 0;
            self.offset = 0;
            self.data = &[];
            return;
        }
        loop {
            let head_len = {
                let block = self.data.first().unwrap();
                (block.len() - self.offset) as u64
            };
            if n < head_len {
                self.offset += n as i32;
                self.limit -= n;
                return;
            }
            n -= head_len;
            self.drop_head();
        }
    }

    pub fn drop_head(&mut self) {
        if self.data.is_empty() {
            panic!("empty BlockSeq");
        }
        let head = self.data.first().unwrap().drop_first(self.offset);
        let head_len = head.len() as u64;
        if head_len > self.limit {
            self.data = &[];
            self.offset = 0;
            self.limit = 0;
        } else {
            self.data = skip_empty(&self.data[1..]);
            self.limit -= head_len;
            self.offset = 0;
        }
    }

    pub fn head(&self) -> Block {
        self.data
            .first()
            .expect("empty BlockSeq")
            .drop_first(self.offset)
            .take_first64(self.limit)
    }

    pub fn take_first(&self, n: u64) -> Self {
        if n == 0 {
            Self::default()
        } else {
            Self {
                data: self.data,
                offset: 0,
                limit: min(self.limit, n),
            }
        }
    }

    pub fn tail(&self) -> Self {
        if self.data.is_empty() {
            panic!("empty BlockSeq");
        }
        let head = self.data.first().unwrap().drop_first(self.offset);
        let head_len = head.len() as u64;
        if head_len > self.limit {
            Self::default()
        } else {
            let tail_slices = skip_empty(&self.data[1..]);
            let tail_limit = self.limit - head_len;
            Self {
                data: tail_slices,
                offset: 0,
                limit: tail_limit,
            }
        }
    }

    pub fn num_bytes(&self) -> u64 {
        self.limit
    }
}

pub fn copy_seq(mut dsts: BlockSeqView, mut srcs: BlockSeqView) -> SysResult<usize> {
    let mut done = 0;
    while !dsts.is_empty() && !srcs.is_empty() {
        let mut dst = dsts.head();
        let src = srcs.head();
        let n = copy(&mut dst, &src)?;
        if n == 0 {
            return Ok(done);
        }
        done += n;
        dsts.drop_first(n as u64);
        srcs.drop_first(n as u64);
    }
    Ok(done)
}

pub fn zero_seq(mut dsts: BlockSeqView) -> SysResult<usize> {
    let mut done = 0;
    while !dsts.is_empty() {
        let n = zero(&mut dsts.head())?;
        done += n;
        dsts.drop_first(n as u64);
    }
    Ok(done)
}

fn first_non_empty_index(blocks: &[Block]) -> Option<usize> {
    blocks.iter().position(|&b| !b.is_empty())
}

fn skip_empty(slice: &[Block]) -> &[Block] {
    for i in 0..slice.len() {
        if !slice[i].is_empty() {
            return slice.get(i..).unwrap();
        }
    }
    slice.get(slice.len()..).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    use once_cell::sync::Lazy;

    #[derive(Clone, Default, Debug)]
    struct BlockSeqTest {
        _desc: String,
        pieces: Vec<String>,
        offset: Option<u64>,
        limit: Option<u64>,
        want: String,
    }

    impl BlockSeqTest {
        fn block_seq(&self) -> BlockSeq {
            let blocks = self
                .pieces
                .iter()
                .map(|s| Block::from_slice(s.as_bytes(), false))
                .collect::<Vec<_>>();
            let mut bs = BlockSeq::from_blocks(blocks);
            if let Some(offset) = self.offset {
                bs.drop_first64(offset);
            }
            if let Some(limit) = self.limit {
                bs = bs.take_first64(limit);
            }
            bs
        }

        fn non_empty_byte_slices(&mut self) -> Vec<&[u8]> {
            let mut slices = Vec::with_capacity(self.pieces.len());
            for s in &self.pieces {
                let mut s = s.as_bytes();
                if let Some(offset) = self.offset {
                    let s_offset = min(offset, s.len() as u64);
                    s = s.get(s_offset as usize..).unwrap();
                    self.offset = Some(offset - s_offset);
                }
                if let Some(limit) = self.limit {
                    let s_limit = min(limit, s.len() as u64);
                    s = s.get(..s_limit as usize).unwrap();
                    self.limit = Some(limit - s_limit);
                }
                if !s.is_empty() {
                    slices.push(s);
                }
            }
            slices
        }
    }

    static BLOCK_SEQ_TESTS: Lazy<[BlockSeqTest; 7]> = Lazy::new(|| {
        [
            BlockSeqTest {
                _desc: "Empty sequence".to_string(),
                ..BlockSeqTest::default()
            },
            BlockSeqTest {
                _desc: "Sequence of length 1".to_string(),
                pieces: vec!["foobar".to_string()],
                want: "foobar".to_string(),
                ..BlockSeqTest::default()
            },
            BlockSeqTest {
                _desc: "Sequence of length 2".to_string(),
                pieces: vec!["foo".to_string(), "bar".to_string()],
                want: "foobar".to_string(),
                ..BlockSeqTest::default()
            },
            BlockSeqTest {
                _desc: "Empty Blocks".to_string(),
                pieces: vec![
                    "".to_string(),
                    "foo".to_string(),
                    "".to_string(),
                    "".to_string(),
                    "bar".to_string(),
                    "".to_string(),
                ],
                want: "foobar".to_string(),
                ..BlockSeqTest::default()
            },
            BlockSeqTest {
                _desc: "Sequence with non-zero offset".to_string(),
                pieces: vec!["foo".to_string(), "bar".to_string()],
                offset: Some(2),
                limit: None,
                want: "obar".to_string(),
            },
            BlockSeqTest {
                _desc: "Sequence with non-maximal limit".to_string(),
                pieces: vec!["foo".to_string(), "bar".to_string()],
                offset: None,
                limit: Some(5),
                want: "fooba".to_string(),
            },
            BlockSeqTest {
                _desc: "Sequence with offset and limit".to_string(),
                pieces: vec!["foo".to_string(), "bar".to_string()],
                offset: Some(2),
                limit: Some(3),
                want: "oba".to_string(),
            },
        ]
    });

    #[test]
    fn block_seq_num_bytes() {
        for test in BLOCK_SEQ_TESTS.iter() {
            let got = test.block_seq().num_bytes();
            assert_eq!(got, test.want.len() as u64)
        }
    }

    #[test]
    fn block_seq_iter_blocks() {
        for test in BLOCK_SEQ_TESTS.iter() {
            let mut test = test.clone();
            let mut srcs = test.block_seq();
            let mut slices = Vec::new();
            while !srcs.is_empty() {
                let src = srcs.head();
                slices.push(unsafe { src.as_slice() }.to_vec());
                let want = srcs.num_bytes() - src.len() as u64;
                srcs = srcs.tail();
                assert_eq!(srcs.num_bytes(), want);
            }
            let want_slices = test.non_empty_byte_slices();
            assert_eq!(want_slices, slices);
        }
    }

    #[test]
    fn block_seq_iter_bytes() {
        for test in BLOCK_SEQ_TESTS.iter() {
            let mut srcs = test.block_seq();
            let mut bytes = Vec::new();
            while !srcs.is_empty() {
                let src = srcs.head();
                let data = vec![0];
                let mut block = Block::from_slice(&data, false);
                let n = copy(&mut block, &src).unwrap();
                assert_eq!(n, 1);
                bytes.push(unsafe { block.as_slice()[0] });
                let want = srcs.num_bytes() - 1;
                srcs.drop_first(1);
                assert_eq!(srcs.num_bytes(), want);
            }
            assert_eq!(std::str::from_utf8(&bytes).unwrap(), &test.want);
        }
    }

    #[test]
    fn block_seq_drop_beyond_limit() {
        let blocks = vec![
            Block::from_slice("123".as_bytes(), false),
            Block::from_slice("4".as_bytes(), false),
        ];
        let mut bs = BlockSeq::from_blocks(blocks);
        assert_eq!(bs.num_bytes(), 4);
        bs = bs.take_first(1);
        assert_eq!(bs.num_bytes(), 1);
        bs.drop_first(2);
        assert_eq!(bs.num_bytes(), 0);
    }
}
