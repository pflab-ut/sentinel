#![feature(map_first_last)]

use std::{cmp::min, collections::BTreeMap, ops::Bound::*};

use anyhow::bail;
use mem::PAGE_SIZE;
use utils::{FileRange, Range};

pub const CHUNK_SHIFT: i32 = 30;
pub const CHUNK_SIZE: i64 = 1 << CHUNK_SHIFT;
pub const CHUNK_MASK: i64 = CHUNK_SIZE - 1;
pub const MAX_PAGE: u64 = u64::MAX & !(PAGE_SIZE as u64 - 1u64);

type MaybeRange<K> = Range<Option<K>>;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Gap<K: num::Integer + num::Bounded> {
    range: MaybeRange<K>,
    prev_key: Option<Range<K>>,
}

impl<K: num::Integer + num::Bounded + Copy> Gap<K> {
    fn new(range: MaybeRange<K>, prev_key: Option<Range<K>>) -> Self {
        Gap { range, prev_key }
    }

    pub fn minimum() -> Self {
        Gap {
            range: MaybeRange {
                start: Some(K::zero()),
                end: Some(K::zero()),
            },
            prev_key: None,
        }
    }

    fn maybe_end(&self) -> Option<K> {
        self.range.end
    }

    pub fn range(&self) -> Range<K> {
        let start = self.range.start;
        let end = self.range.end;
        Range {
            start: start.unwrap_or_else(K::min_value),
            end: end.unwrap_or_else(K::max_value),
        }
    }

    pub fn start(&self) -> K {
        self.range.start.unwrap_or_else(K::min_value)
    }

    pub fn end(&self) -> K {
        self.range.end.unwrap_or_else(K::max_value)
    }

    fn is_superset_of(&self, r: &Range<K>) -> bool {
        self.range().is_superset_of(r)
    }

    fn is_empty(&self) -> bool {
        self.start() == self.end()
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Seg<K: num::Integer + num::Bounded + Copy> {
    range: Range<K>,
    prev_key: Option<Range<K>>,
    next_key: Option<Range<K>>,
}

impl<K: num::Integer + num::Bounded + Copy> Seg<K> {
    fn new(range: Range<K>, prev_key: Option<Range<K>>, next_key: Option<Range<K>>) -> Self {
        Seg {
            range,
            prev_key,
            next_key,
        }
    }

    #[inline]
    pub fn start(&self) -> K {
        self.range.start
    }

    #[inline]
    pub fn end(&self) -> K {
        self.range.end
    }

    #[inline]
    pub fn range(&self) -> Range<K> {
        self.range
    }
}

#[derive(Debug)]
pub enum SegOrGap<K: num::Integer + num::Bounded + Copy> {
    Segment(Seg<K>),
    Gap(Gap<K>),
}

pub trait SetOperations {
    type K;
    type V;
    fn merge(
        &self,
        r1: Range<Self::K>,
        v1: &Self::V,
        r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V>;
    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V);
}

pub struct Set<K: num::Integer + num::Bounded, V> {
    map: BTreeMap<Range<K>, V>,
    operations: Box<dyn SetOperations<K = K, V = V>>,
}

impl<K: num::Integer + num::Bounded + std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug
    for Set<K, V>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("").field(&self.map).finish()
    }
}

impl<
        K: num::Integer
            + num::Bounded
            + num::ToPrimitive
            + std::ops::AddAssign
            + std::fmt::Display
            + std::fmt::Debug
            + Copy
            + Clone,
        V: std::cmp::PartialEq + Clone,
    > Set<K, V>
{
    pub fn new(operations: Box<dyn SetOperations<K = K, V = V>>) -> Self {
        Self {
            map: BTreeMap::new(),
            operations,
        }
    }

    pub fn inner_map(&self) -> &BTreeMap<Range<K>, V> {
        &self.map
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn print_keys(&self) {
        for key in self.map.keys() {
            logger::debug!("[{:x?}, {:x?})", key.start, key.end);
        }
    }

    pub fn span(&self) -> K {
        let mut sz = K::zero();
        let mut maybe_seg = self.first_segment();
        while let Some(seg) = maybe_seg {
            sz += seg.range.len();
            maybe_seg = self.next_segment_of_seg(&seg);
        }
        sz
    }

    pub fn span_range(&self, r: Range<K>) -> K {
        if r.end < r.start {
            panic!("invalid range: {:?}", r);
        }
        if r.start == r.end {
            return K::min_value();
        }
        let mut sz = K::zero();
        let mut maybe_seg = self.lower_bound_segment(r.start);
        while maybe_seg.map_or(false, |s| s.start() < r.end) {
            let seg = maybe_seg.unwrap();
            sz += seg.range.intersect(&r).len();
            maybe_seg = self.next_segment_of_seg(&seg);
        }
        sz
    }

    pub fn last_segment(&self) -> Option<Seg<K>> {
        let mut back = self.map.iter().rev();
        let last_range = back.next().map(|(k, _)| *k)?;
        let second_last_key = back.next().map(|(k, _)| *k);
        Some(Seg::new(last_range, second_last_key, None))
    }

    pub fn first_segment(&self) -> Option<Seg<K>> {
        let mut front = self.map.iter();
        let first_range = front.next().map(|(k, _)| *k)?;
        let next_key = front.next().map(|(k, _)| *k);
        Some(Seg::new(first_range, None, next_key))
    }

    pub fn last_gap(&self) -> Option<Gap<K>> {
        let (prev_key, gap_start) = match self.map.last_key_value() {
            Some((k, _)) => (Some(*k), k.end),
            None => (None, K::min_value()),
        };
        if gap_start == K::max_value() {
            None
        } else {
            Some(Gap::new(
                MaybeRange {
                    start: Some(gap_start),
                    end: None,
                },
                prev_key,
            ))
        }
    }

    pub fn first_gap(&self) -> Option<Gap<K>> {
        let gap_end = match self.map.first_key_value() {
            Some((k, _)) => k.start,
            None => {
                return Some(Gap::new(
                    MaybeRange {
                        start: None,
                        end: None,
                    },
                    None,
                ))
            }
        };
        if gap_end.is_zero() {
            None
        } else {
            Some(Gap::new(
                MaybeRange {
                    start: None,
                    end: Some(gap_end),
                },
                None,
            ))
        }
    }

    pub fn find_available_range(
        &self,
        mut file_size: i64,
        length: u64,
        alignment: u64,
    ) -> Option<FileRange> {
        let alignment_mask = alignment - 1;
        let last_gap = self.last_gap().unwrap();
        let mut gap = last_gap;
        loop {
            let end = min(gap.end().to_u64().unwrap(), file_size as u64);
            if end < length {
                break;
            }
            let unaligned_start = end - length;
            let start = unaligned_start & !alignment_mask;
            if start >= gap.start().to_u64().unwrap() {
                return Some(FileRange {
                    start,
                    end: start + length,
                });
            }
            gap = match self.prev_large_enough_gap(&gap, length) {
                Some(g) => g,
                None => break,
            };
        }

        let min = last_gap.start().to_u64().unwrap();
        let min = (min + alignment_mask) & !alignment_mask;
        min.checked_add(length)?;
        loop {
            let mut new_file_size = 2 * file_size;
            if new_file_size <= file_size {
                if file_size != 0 {
                    return None;
                }
                new_file_size = CHUNK_SIZE;
            }
            file_size = new_file_size;

            if (file_size as u64) < length {
                continue;
            }
            let unaligned_start = file_size as u64 - length;
            let start = unaligned_start & !alignment_mask;
            if start >= min {
                return Some(FileRange {
                    start,
                    end: start + length,
                });
            }
        }
    }

    pub fn find_gap(&self, key: K) -> Option<Gap<K>> {
        let r = Range {
            start: key,
            end: key,
        };
        let mut before = self.map.range(..r).rev();
        let mut after = self.map.range(r..);
        let prev_seg = before.next().map(|(k, _)| *k);
        let next_seg = after.next().map(|(k, _)| *k);
        let start = match prev_seg {
            Some(prev_seg) => {
                if prev_seg.end > key {
                    return None;
                } else {
                    Some(prev_seg.end)
                }
            }
            None => None,
        };
        let end = match next_seg {
            Some(next_seg) => {
                if key < next_seg.start {
                    Some(next_seg.start)
                } else {
                    return None;
                }
            }
            None => None,
        };
        Some(Gap::new(MaybeRange { start, end }, prev_seg))
    }

    pub fn lower_bound_gap(&self, min: K) -> Option<Gap<K>> {
        match self.find_gap(min) {
            Some(gap) => Some(gap),
            None => {
                let seg = self
                    .find_segment(min)
                    .unwrap_or_else(|| panic!("no gap or segment corresponding the key {}", min));
                let start = seg.range.end;
                let end = seg.next_key.map(|k| k.start);
                let gap = Gap::new(
                    MaybeRange {
                        start: Some(start),
                        end,
                    },
                    Some(seg.range),
                );
                Some(gap)
            }
        }
    }

    pub fn upper_bound_gap(&self, max: K) -> Option<Gap<K>> {
        match self.find_gap(max) {
            Some(gap) => Some(gap),
            None => {
                let seg = self
                    .find_segment(max)
                    .unwrap_or_else(|| panic!("no gap or segment corresponding the key {}", max));
                let prev_key = seg.prev_key?;
                let gap = Gap::new(
                    MaybeRange {
                        start: Some(prev_key.end),
                        end: Some(seg.range.start),
                    },
                    Some(prev_key),
                );
                Some(gap)
            }
        }
    }

    pub fn lower_bound_segment(&self, min: K) -> Option<Seg<K>> {
        match self.find_segment(min) {
            Some(seg) => Some(seg),
            None => {
                let gap = self
                    .find_gap(min)
                    .unwrap_or_else(|| panic!("seg and gap for key {} are both None.", min));
                self.next_segment_of_gap(&gap)
            }
        }
    }

    pub fn upper_bound_segment(&self, max: K) -> Option<Seg<K>> {
        match self.find_segment(max) {
            Some(seg) => Some(seg),
            None => {
                let gap = self
                    .find_gap(max)
                    .unwrap_or_else(|| panic!("seg and gap for key {} are both None.", max));
                self.prev_segment_of_gap(&gap)
            }
        }
    }

    pub fn find_segment(&self, key: K) -> Option<Seg<K>> {
        let r = Range {
            start: key,
            end: key,
        };
        let mut it = self.map.range(..r).rev();
        if let Some(back) = it.next().map(|(k, _)| *k) {
            if back.contains(key) {
                let prev_key = it.next().map(|(k, _)| *k);
                let next_key = {
                    let mut it = self.map.range((Excluded(back), Unbounded));
                    it.next().map(|(k, _)| *k)
                };
                let seg = Seg::new(back, prev_key, next_key);
                return Some(seg);
            }
        }

        let mut it = self.map.range(r..);
        if let Some(next) = it.next().map(|(k, _)| *k) {
            if next.contains(key) {
                let next_key = it.next().map(|(k, _)| *k);
                let prev_key = {
                    let mut it = self.map.range(..next).rev();
                    it.next().map(|(k, _)| *k)
                };
                let seg = Seg::new(next, prev_key, next_key);
                return Some(seg);
            }
        }

        None
    }

    pub fn add(&mut self, range: Range<K>, val: V) -> bool {
        let start = range.start;
        let end = range.end;
        if end <= start {
            panic!("invalid segment range: ({}, {})", start, end);
        }
        let gap = match self.find_gap(start) {
            Some(gap) => gap,
            None => return false,
        };
        if end > gap.end() {
            false
        } else {
            self.insert(range, val);
            true
        }
    }

    pub fn add_without_merging(&mut self, range: Range<K>, val: V) -> bool {
        let start = range.start;
        let end = range.end;
        if end <= start {
            panic!("invalid segment range: ({}, {})", start, end);
        }
        let gap = match self.find_gap(start) {
            Some(gap) => gap,
            None => return false,
        };
        if end > gap.end() {
            false
        } else {
            self.insert_without_merging(&gap, range, val);
            true
        }
    }

    pub fn insert(&mut self, range: Range<K>, val: V) -> Seg<K> {
        let start = range.start;
        let end = range.end;
        if end < start {
            panic!("invalid segment range: ({}, {})", start, end);
        }
        let (prev, second_prev_start) = {
            let mut it = self.map.range(..Range { start, end: start }).rev();
            let prev = it.next().map(|(k, _)| *k);
            let second_prev_start = it.next().map(|(k, _)| *k);
            (prev, second_prev_start)
        };
        let (next, second_next_start) = {
            let mut it = self.map.range(Range { start: end, end }..);
            let next = it.next().map(|(k, _)| *k);
            let second_next_start = it.next().map(|(k, _)| *k);
            (next, second_next_start)
        };
        if prev.as_ref().map_or(false, |p| p.end > range.start) {
            panic!("new segment {:?} overlaps predecessor {:?}", range, prev);
        }
        if next.as_ref().map_or(false, |n| n.start < range.end) {
            panic!("new segment {:?} overlaps successor {:?}", range, next);
        }

        if let Some(prev) = prev {
            if prev.end == start {
                let prev_value = self.map.get(&prev).unwrap();
                if let Some(ref mval) = self.operations.merge(prev, prev_value, range, &val) {
                    let new_key = Range {
                        start: prev.start,
                        end,
                    };
                    self.map.remove(&prev).unwrap();
                    self.map.insert(new_key, mval.clone());
                    if let Some(next) = next {
                        let next_val = self.map.get(&next).unwrap();
                        if next.start == end {
                            if let Some(mval) = self.operations.merge(new_key, mval, next, next_val)
                            {
                                self.map.remove(&new_key).unwrap();
                                let new_key = Range {
                                    start: prev.start,
                                    end: next.end,
                                };
                                let removed = self.remove(next);
                                self.map.insert(new_key, mval);
                                return self.prev_segment_of_gap(&removed).unwrap();
                            }
                        }
                    }
                    return Seg::new(new_key, second_prev_start, next);
                }
            }
        }
        if let Some(next) = next {
            if end == next.start {
                let next_val = self.map.get(&next).unwrap();
                if let Some(ref mval) = self.operations.merge(range, &val, next, next_val) {
                    self.map.remove(&next).unwrap();
                    let new_key = Range {
                        start,
                        end: next.end,
                    };
                    self.map.insert(new_key, mval.clone());
                    return Seg::new(new_key, prev, second_next_start);
                }
            }
        }
        self.map.insert(range, val);
        Seg::new(range, prev, next)
    }

    pub fn insert_without_merging(&mut self, gap: &Gap<K>, range: Range<K>, val: V) -> Seg<K> {
        let start = range.start;
        let end = range.end;
        if end <= start {
            panic!("invalid segment range {:?}", range);
        }
        if !gap.is_superset_of(&range) {
            panic!(
                "cannot insert segment range {:?} into gap range {:?}",
                range, gap.range
            );
        }
        self.map.insert(range, val);
        let prev_key = {
            let mut it = self.map.range(..range).rev();
            it.next().map(|(k, _)| *k)
        };
        let next_key = {
            let mut it = self.map.range(Range { start: end, end }..);
            it.next().map(|(k, _)| *k)
        };
        Seg::new(range, prev_key, next_key)
    }

    pub fn merge_unchecked(&mut self, first: Seg<K>, second: Seg<K>) -> Option<Seg<K>> {
        if first.end() == second.start() {
            let first_val = self.value(&first);
            let second_val = self.value(&second);
            if let Some(mval) =
                self.operations
                    .merge(first.range, first_val, second.range, second_val)
            {
                let new_key = Range {
                    start: first.start(),
                    end: second.end(),
                };
                self.map.remove(&first.range).unwrap();
                self.map.insert(new_key, mval);
                let rm = self.remove(second.range);
                return self.prev_segment_of_gap(&rm);
            }
        }
        None
    }

    pub fn merge(&mut self, first: Seg<K>, second: Seg<K>) -> Option<Seg<K>> {
        if self.next_segment_of_seg(&first).unwrap().range() != second.range() {
            panic!(
                "attempt to merge non-neighboring segments {:?}, {:?} (next segment of first is {:?})",
                first, second, self.next_segment_of_seg(&first)
            );
        }
        self.merge_unchecked(first, second)
    }

    pub fn merge_range(&mut self, r: Range<K>) {
        let start = r.start;
        let end = r.end;
        let mut seg = match self.lower_bound_segment(start) {
            Some(s) => s,
            None => return,
        };
        let mut next = self.next_segment_of_seg(&seg);
        while next.map_or(false, |n| n.range.start < end) {
            let next_inner = next.unwrap();
            match self.merge_unchecked(seg, next_inner) {
                Some(mseg) => {
                    seg = mseg;
                    next = self.next_segment_of_seg(&mseg);
                }
                None => {
                    seg = next_inner;
                    next = self.next_segment_of_seg(&next_inner);
                }
            }
        }
    }

    pub fn merge_adjacant(&mut self, r: Range<K>) {
        let first = self.find_segment(r.start);
        if let Some(first) = first {
            if let Some(prev) = self.prev_segment_of_seg(&first) {
                self.merge(prev, first);
            }
        }
        let last = self.find_segment(r.end - K::one());
        if let Some(last) = last {
            if let Some(next) = self.next_segment_of_seg(&last) {
                self.merge(last, next);
            }
        }
    }

    pub fn remove(&mut self, seg: Range<K>) -> Gap<K> {
        self.map.remove(&seg).unwrap();
        let prev_key = {
            let mut it = self.map.range(..seg).rev();
            it.next().map(|(k, _)| *k)
        };
        Gap::new(
            MaybeRange {
                start: Some(seg.start),
                end: Some(seg.end),
            },
            prev_key,
        )
    }

    pub fn next_non_empty(&self, seg: &Seg<K>) -> Option<SegOrGap<K>> {
        if let Some(gap) = self.next_gap_of_seg(seg) {
            let start = gap.range.start.unwrap();
            let end = gap.range.end.unwrap_or_else(K::max_value);
            if end > start {
                return Some(SegOrGap::Gap(gap));
            }
        }
        let seg = self.next_segment_of_seg(seg)?;
        if seg.end() > seg.start() {
            Some(SegOrGap::Segment(seg))
        } else {
            None
        }
    }

    pub fn next_segment_of_seg(&self, seg: &Seg<K>) -> Option<Seg<K>> {
        let mut it = self.map.range((Excluded(seg.range), Unbounded));
        let next_range = it.next().map(|(k, _)| *k)?;
        let next_key = it.next().map(|(k, _)| *k);
        Some(Seg::new(next_range, Some(seg.range), next_key))
    }

    pub fn next_segment_of_gap(&self, gap: &Gap<K>) -> Option<Seg<K>> {
        let gap_end = gap.maybe_end()?;
        let prev_key = match gap.range.start {
            Some(k) => {
                let mut it = self.map.range(..Range { start: k, end: k }).rev();
                it.next().map(|(k, _)| *k)
            }
            None => None,
        };
        let mut it = self.map.range(
            Range {
                start: gap_end,
                end: gap_end,
            }..,
        );
        match it.next() {
            Some((next_range, _)) => {
                let next_key = it.next().map(|(k, _)| *k);
                Some(Seg::new(*next_range, prev_key, next_key))
            }
            None => Some(Seg::new(
                Range {
                    start: gap_end,
                    end: K::max_value(),
                },
                prev_key,
                None,
            )),
        }
    }

    pub fn next_gap_of_seg(&self, seg: &Seg<K>) -> Option<Gap<K>> {
        let gap_start = seg.end();
        if gap_start == K::max_value() {
            return None;
        }
        let gap_end = {
            let mut it = self.map.range((Excluded(seg.range), Unbounded));
            it.next().map(|(k, _)| k.start)
        };
        Some(Gap::new(
            MaybeRange {
                start: Some(gap_start),
                end: gap_end,
            },
            Some(seg.range),
        ))
    }

    pub fn next_gap_of_gap(&self, gap: &Gap<K>) -> Option<Gap<K>> {
        let seg_start = gap.maybe_end()?;
        let mut range = self.map.range(
            Range {
                start: seg_start,
                end: seg_start,
            }..,
        );
        let prev_seg_range = range.next().unwrap().0;
        let gap_start = prev_seg_range.end;
        let gap_end = range.next().map(|(k, _)| k.start);
        Some(Gap::new(
            MaybeRange {
                start: Some(gap_start),
                end: gap_end,
            },
            Some(*prev_seg_range),
        ))
    }

    pub fn next_large_enough_gap(&self, gap: &Gap<K>, min_size: K) -> Option<Gap<K>> {
        let seg_start = gap.maybe_end()?;
        let mut range = self.map.range(
            Range {
                start: seg_start,
                end: seg_start,
            }..,
        );
        let mut prev_range = *range.next()?.0;
        let mut gap_start = prev_range.end;
        for (r, _) in range {
            let gap_end = r.start;
            if gap_end - gap_start >= min_size {
                let gap_end = if gap_end == K::max_value() {
                    None
                } else {
                    Some(gap_end)
                };
                return Some(Gap::new(
                    MaybeRange {
                        start: Some(gap_start),
                        end: gap_end,
                    },
                    Some(prev_range),
                ));
            }
            gap_start = r.end;
            prev_range = *r;
        }
        if K::max_value() - gap_start >= min_size {
            Some(Gap::new(
                MaybeRange {
                    start: Some(gap_start),
                    end: None,
                },
                Some(prev_range),
            ))
        } else {
            None
        }
    }

    pub fn prev_non_empty(&self, seg: &Seg<K>) -> Option<SegOrGap<K>> {
        if let Some(gap) = self.prev_gap_of_seg(seg) {
            let start = gap.range.start.unwrap_or_else(K::zero);
            let end = gap.range.end.unwrap();
            if end > start {
                return Some(SegOrGap::Gap(gap));
            }
        }
        let seg = self.prev_segment_of_seg(seg)?;
        if seg.range.end > seg.range.start {
            Some(SegOrGap::Segment(seg))
        } else {
            None
        }
    }

    pub fn prev_segment_of_seg(&self, seg: &Seg<K>) -> Option<Seg<K>> {
        let mut it = self.map.range(..seg.range).rev();
        let prev_seg_range = it.next().map(|(k, _)| *k)?;
        let prev_key = it.next().map(|(k, _)| *k);
        Some(Seg::new(prev_seg_range, prev_key, Some(seg.range)))
    }

    pub fn prev_segment_of_gap(&self, gap: &Gap<K>) -> Option<Seg<K>> {
        let mut range = self.map.range(..gap.range()).rev();
        let next_key = {
            let mut it = self.map.range(gap.range()..);
            it.next().map(|(k, _)| *k)
        };
        range
            .next()
            .map(|(k, _)| Seg::new(*k, range.next().map(|(k, _)| *k), next_key))
    }

    pub fn prev_gap_of_seg(&self, seg: &Seg<K>) -> Option<Gap<K>> {
        let mut range = self.map.range(..seg.range).rev();
        let end = seg.range.start;
        if end == K::zero() {
            return None;
        }
        let prev_seg = range.next();
        Some(Gap::new(
            MaybeRange {
                start: prev_seg.map(|(k, _)| k.end),
                end: Some(end),
            },
            prev_seg.map(|(k, _)| *k),
        ))
    }

    pub fn prev_gap_of_gap(&self, gap: &Gap<K>) -> Option<Gap<K>> {
        let mut range = self.map.range(..gap.range()).rev();
        let gap_end = range.next().map(|(k, _)| k.start)?;
        let prev_range = range.next().map(|(k, _)| *k);
        Some(Gap::new(
            MaybeRange {
                start: prev_range.map(|r| r.end),
                end: Some(gap_end),
            },
            prev_range,
        ))
    }

    pub fn prev_large_enough_gap(&self, gap: &Gap<K>, min_size: u64) -> Option<Gap<K>> {
        let mut range = self.map.range(..gap.range()).rev();
        let mut gap_end = range.next().map(|(k, _)| k.start)?;
        let mut cur_start_end = None;
        for (r, _) in range {
            if let Some((start, end)) = cur_start_end {
                return Some(Gap::new(
                    MaybeRange {
                        start: Some(start),
                        end: Some(end),
                    },
                    Some(*r),
                ));
            }
            let gap_start = r.end;
            if (gap_end - gap_start).to_u64().unwrap() >= min_size {
                cur_start_end = Some((gap_start, gap_end));
            }
            gap_end = r.start;
        }
        match cur_start_end {
            Some((start, end)) => Some(Gap::new(
                MaybeRange {
                    start: Some(start),
                    end: Some(end),
                },
                None,
            )),
            None => {
                if (gap_end - K::min_value()).to_u64().unwrap() < min_size {
                    None
                } else {
                    Some(Gap::new(
                        MaybeRange {
                            start: None,
                            end: Some(gap_end),
                        },
                        None,
                    ))
                }
            }
        }
    }

    pub fn split_at(&mut self, split: K) -> bool {
        match self.find_segment(split) {
            Some(seg) => {
                if seg.range.can_split_at(split) {
                    let start = seg.range.start;
                    let end = seg.range.end;
                    let v = self.map.remove(&seg.range).unwrap();
                    self.map.insert(Range { start, end: split }, v.clone());
                    self.map.insert(Range { start: split, end }, v);
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    // precondition: seg.start < key < seg.end
    fn split_unchecked(&mut self, seg: &Seg<K>, split: K) -> (Seg<K>, Seg<K>) {
        let (val1, val2) = self.operations.split(seg.range(), self.value(seg), split);
        let key1 = Range {
            start: seg.start(),
            end: split,
        };
        let key2 = Range {
            start: split,
            end: seg.end(),
        };
        self.map.remove(&seg.range).unwrap();
        self.map.insert(key1, val1);
        self.map.insert(key2, val2);
        let seg = {
            let mut iter = self.map.range(..key1).rev();
            let prev_key = iter.next().map(|(k, _)| *k);
            Seg::new(key1, prev_key, Some(key2))
        };
        let seg2 = {
            let mut iter = self.map.range((Excluded(key2), Unbounded));
            let next_key = iter.next().map(|(k, _)| *k);
            Seg::new(key2, Some(key1), next_key)
        };
        (seg, seg2)
    }

    pub fn isolate(&mut self, seg: &Seg<K>, range: Range<K>) -> Seg<K> {
        let mut seg = *seg;
        if seg.range.can_split_at(range.start) {
            seg = self.split_unchecked(&seg, range.start).1;
        }
        if seg.range.can_split_at(range.end) {
            seg = self.split_unchecked(&seg, range.end).0;
        }
        seg
    }

    pub fn value(&self, seg: &Seg<K>) -> &V {
        self.map.get(&seg.range).unwrap()
    }

    pub fn value_mut(&mut self, seg: &Seg<K>) -> &mut V {
        self.map.get_mut(&seg.range).unwrap()
    }

    pub fn apply_contiguous(
        &mut self,
        r: Range<K>,
        f: fn(&mut BTreeMap<Range<K>, V>, Seg<K>),
    ) -> Option<Gap<K>> {
        let mut seg = match self.find_segment(r.start) {
            Some(s) => s,
            None => return self.find_gap(r.start),
        };
        loop {
            seg = self.isolate(&seg, r);
            f(&mut self.map, seg);
            if seg.end() >= r.end {
                return None;
            }
            let gap = self.next_gap_of_seg(&seg)?;
            if !gap.is_empty() {
                return Some(gap);
            }
            seg = self.next_segment_of_gap(&gap)?;
        }
    }

    #[cfg(test)]
    fn segment_test_check<F: Fn(i32, Range<K>, V) -> anyhow::Result<()>>(
        &self,
        expected_segment: i32,
        seg_fn: Option<F>,
    ) -> anyhow::Result<()> {
        let mut have_prev = false;
        let mut prev = 0i64;
        let mut nr_segments = 0;
        for (key, val) in self.map.iter() {
            let next = key.start.to_u64().unwrap() as i64;
            if have_prev && prev >= next {
                anyhow::bail!(
                    "incorrect order: key {} (segment {}) >= key {} (segment {})",
                    prev,
                    nr_segments - 1,
                    next,
                    nr_segments,
                );
            }
            if let Some(ref seg_fn) = seg_fn {
                seg_fn(nr_segments, *key, val.clone())?;
            }
            prev = next;
            have_prev = true;
            nr_segments += 1;
        }
        if nr_segments != expected_segment {
            anyhow::bail!(
                "incorrect number of segments: got {}, wanted: {}",
                nr_segments,
                expected_segment
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn count_segments(&self) -> usize {
        self.map.len()
    }
}

impl<
        K: num::Integer
            + num::Bounded
            + num::ToPrimitive
            + std::ops::AddAssign
            + std::fmt::Display
            + std::fmt::Debug
            + Copy
            + Clone,
        V: std::cmp::PartialEq + Copy,
    > Set<K, V>
{
    pub fn import_sorted_slices(&mut self, slices: &SegmentDataSlices<K, V>) -> anyhow::Result<()> {
        if !self.is_empty() {
            bail!("cannot import into non-empty set");
        }
        let mut gap_maybe = self.first_gap();
        for i in 0..slices.start.len() {
            let gap = gap_maybe.unwrap();
            let r = Range {
                start: slices.start[i],
                end: slices.end[i],
            };
            if !gap.range().is_superset_of(&r) {
                bail!(
                    "segment overlaps a preceding segment or is incorrectly sorted: [{:?}, {:?})",
                    slices.start[i],
                    slices.end[i],
                );
            }
            let inserted = self.insert_without_merging(&gap, r, slices.values[i]);
            gap_maybe = self.next_gap_of_seg(&inserted);
        }
        Ok(())
    }
}

pub struct SegmentDataSlices<K, V> {
    pub start: Vec<K>,
    pub end: Vec<K>,
    pub values: Vec<V>,
}

impl<K, V> Default for SegmentDataSlices<K, V> {
    fn default() -> Self {
        Self {
            start: Vec::new(),
            end: Vec::new(),
            values: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::prelude::SliceRandom;

    use super::*;

    const TEST_SIZE: i32 = 8000;
    const VALUE_OFFSET: i32 = 100000;
    const INTERVAL_LEN: i32 = 10;

    fn validate<K: std::fmt::Display + num::ToPrimitive>(
        nr: i32,
        range: Range<K>,
        v: i32,
    ) -> anyhow::Result<()> {
        let got = v;
        let want = range.start.to_i32().unwrap() + VALUE_OFFSET;
        if got != want {
            anyhow::bail!(
                "segment {} has key {}, value {} (expected {})",
                nr,
                range.start,
                got,
                want
            );
        }
        Ok(())
    }

    fn shuffle(xs: &mut [i32]) {
        let mut rng = rand::thread_rng();
        xs.shuffle(&mut rng);
    }

    fn rand_interval_permutation(size: i32) -> Vec<i32> {
        let mut perm: Vec<_> = (0..size).map(|v| v * INTERVAL_LEN).collect();
        shuffle(&mut perm);
        perm
    }

    struct Ops;
    impl SetOperations for Ops {
        type K = u64;
        type V = i32;
        fn merge(
            &self,
            _: Range<Self::K>,
            v1: &Self::V,
            _: Range<Self::K>,
            v2: &Self::V,
        ) -> Option<Self::V> {
            if v1 == v2 {
                Some(*v1)
            } else {
                None
            }
        }

        fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
            (*v, *v + (split - r.start) as i32)
        }
    }

    #[test]
    fn add_random() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        let mut order: Vec<i32> = (0..TEST_SIZE).collect();
        shuffle(&mut order);
        let mut nr_iterations = 0;
        for (_, j) in order.iter().enumerate() {
            assert!(s.add_without_merging(
                Range {
                    start: *j as u64,
                    end: *j as u64 + 1
                },
                j + VALUE_OFFSET
            ));
            nr_iterations += 1;
            assert!(s.segment_test_check(nr_iterations, Some(validate)).is_ok());
        }
        assert_eq!(s.count_segments() as i32, nr_iterations);
    }

    #[test]
    fn remove_random() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        for i in 0..TEST_SIZE {
            assert!(s.add_without_merging(
                Range {
                    start: i as u64,
                    end: i as u64 + 1
                },
                i + VALUE_OFFSET
            ));
        }
        let mut order: Vec<i32> = (0..TEST_SIZE).collect();
        shuffle(&mut order);
        let mut nr_removals = 0;
        for (_, j) in order.iter().enumerate() {
            let seg = s.find_segment(*j as u64);
            assert!(seg.is_some());
            let seg = seg.unwrap();
            s.remove(seg.range);
            nr_removals += 1;
            assert!(s
                .segment_test_check(TEST_SIZE - nr_removals, Some(validate))
                .is_ok());
        }
        assert_eq!(s.count_segments() as i32, TEST_SIZE - nr_removals);
    }

    #[test]
    fn next_large_enough_gap() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        let mut order = rand_interval_permutation(TEST_SIZE * 2);
        let order = &mut order[..TEST_SIZE as usize];
        for (_, j) in order.iter().enumerate() {
            let range = Range {
                start: *j as u64,
                end: *j as u64 + rand::random::<u64>() % (INTERVAL_LEN as u64 - 1) + 1,
            };
            assert!(s.add(range, *j + VALUE_OFFSET));
        }
        shuffle(order);
        let order = &order[..TEST_SIZE as usize / 2];
        for j in order.iter() {
            let seg = match s.find_segment(*j as u64) {
                Some(s) => s,
                None => continue,
            };
            s.remove(seg.range);
        }

        let min_size = 7;
        let mut gaps1 = Vec::new();
        let mut gap_maybe = {
            let g = s.lower_bound_gap(0).unwrap();
            s.next_large_enough_gap(&g, min_size)
        };
        while let Some(gap) = gap_maybe {
            let start = gap.range.start.unwrap();
            let end = gap.range.end.unwrap_or(u64::MAX);
            assert!(end - start >= min_size);
            gaps1.push(start);
            gap_maybe = s.next_large_enough_gap(&gap, min_size)
        }

        let mut gaps2 = Vec::new();
        let mut gap_maybe = {
            let g = s.lower_bound_gap(0).unwrap();
            s.next_gap_of_gap(&g)
        };
        while let Some(gap) = gap_maybe {
            let start = gap.range.start.unwrap();
            let end = gap.range.end.unwrap_or(u64::MAX);
            if end - start >= min_size {
                gaps2.push(start);
            }
            gap_maybe = s.next_gap_of_gap(&gap)
        }

        assert_eq!(gaps1.len(), gaps2.len());
        let all_eq = gaps1.iter().zip(&gaps2).all(|(a, b)| a == b);
        assert!(all_eq);
    }

    #[test]
    fn prev_large_enough_gap() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        let mut order = rand_interval_permutation(TEST_SIZE * 2);
        let order = &mut order[..TEST_SIZE as usize];
        for (_, j) in order.iter().enumerate() {
            let range = Range {
                start: *j as u64,
                end: *j as u64 + rand::random::<u64>() % (INTERVAL_LEN as u64 - 1) + 1,
            };
            assert!(s.add(range, *j + VALUE_OFFSET));
        }
        shuffle(order);
        let order = &order[..TEST_SIZE as usize / 2];
        let end = s.last_segment().unwrap().range.end;
        for j in order.iter() {
            let seg = match s.find_segment(*j as u64) {
                Some(s) => s,
                None => continue,
            };
            s.remove(seg.range);
        }

        let min_size = 7;
        let mut gaps1 = Vec::new();
        let mut gap_maybe = {
            let g = s.upper_bound_gap(end + INTERVAL_LEN as u64).unwrap();
            s.prev_large_enough_gap(&g, min_size)
        };
        while let Some(gap) = gap_maybe {
            let start = gap.range.start.unwrap_or(0);
            let end = gap.range.end.unwrap();
            assert!(end - start >= min_size);
            gaps1.push(start);
            gap_maybe = s.prev_large_enough_gap(&gap, min_size)
        }

        let mut gaps2 = Vec::new();
        let mut gap_maybe = {
            let g = s.upper_bound_gap(end + INTERVAL_LEN as u64).unwrap();
            s.prev_gap_of_gap(&g)
        };
        while let Some(gap) = gap_maybe {
            let start = gap.range.start.unwrap_or(0);
            let end = gap.range.end.unwrap_or(u64::MAX);
            if end - start >= min_size {
                gaps2.push(start);
            }
            gap_maybe = s.prev_gap_of_gap(&gap)
        }

        assert_eq!(gaps1.len(), gaps2.len());
        let all_eq = gaps1.iter().zip(&gaps2).all(|(a, b)| a == b);
        assert!(all_eq);
    }

    #[test]
    fn add_sequential_adjacant() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        let mut nr_iterations = 0;
        for i in 0..TEST_SIZE {
            assert!(s.add_without_merging(
                Range {
                    start: i as u64,
                    end: i as u64 + 1
                },
                i + VALUE_OFFSET
            ));
            nr_iterations += 1;
            assert!(s.segment_test_check(nr_iterations, Some(validate)).is_ok());
        }
        assert_eq!(s.count_segments(), nr_iterations as usize);

        let first_seg = s.first_segment().unwrap();
        let want = s.first_gap();
        let got = match s.prev_non_empty(&first_seg) {
            Some(SegOrGap::Segment(_)) => panic!("prev non empty element should be gap"),
            Some(SegOrGap::Gap(gap)) => Some(gap),
            None => None,
        };
        assert_eq!(got, want);

        let want = s.next_segment_of_seg(&first_seg);
        let got = match s.next_non_empty(&first_seg) {
            Some(SegOrGap::Segment(seg)) => Some(seg),
            Some(SegOrGap::Gap(_)) => panic!("next non empty element should be gap"),
            None => None,
        };
        assert_eq!(got, want);

        let last_seg = s.last_segment().unwrap();
        let want = s.prev_segment_of_seg(&last_seg);
        let got = match s.prev_non_empty(&last_seg) {
            Some(SegOrGap::Segment(seg)) => Some(seg),
            Some(SegOrGap::Gap(_)) => panic!("prev non empty element should be segment"),
            None => None,
        };
        assert_eq!(got, want);

        let want = s.last_gap();
        let got = match s.next_non_empty(&last_seg) {
            Some(SegOrGap::Gap(gap)) => Some(gap),
            Some(SegOrGap::Segment(_)) => panic!("next non empty element should be gap"),
            None => None,
        };
        assert_eq!(got, want);

        let mut maybe_seg = s.next_segment_of_seg(&first_seg);
        while let Some(seg) = maybe_seg {
            if seg == last_seg {
                break;
            }

            let want = s.prev_segment_of_seg(&seg);
            let got = match s.prev_non_empty(&seg) {
                Some(SegOrGap::Segment(seg)) => Some(seg),
                Some(SegOrGap::Gap(_)) => panic!("prev non empty element should be segment"),
                None => None,
            };
            assert_eq!(got, want);

            let want = s.next_segment_of_seg(&seg);
            let got = match s.next_non_empty(&seg) {
                Some(seg_or_gap) => match seg_or_gap {
                    SegOrGap::Segment(seg) => Some(seg),
                    SegOrGap::Gap(_) => panic!("next non empty element should be gap"),
                },
                None => None,
            };
            assert_eq!(got, want);
            maybe_seg = s.next_segment_of_seg(&seg);
        }
    }

    #[test]
    fn add_sequential_non_adjacant() {
        let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
        let mut nr_iterations = 0;
        for i in 0..TEST_SIZE {
            assert!(s.add_without_merging(
                Range {
                    start: 2 * i as u64,
                    end: 2 * i as u64 + 1
                },
                2 * i + VALUE_OFFSET
            ));
            nr_iterations += 1;
            assert!(s.segment_test_check(nr_iterations, Some(validate)).is_ok());
        }
        assert_eq!(s.count_segments(), nr_iterations as usize);

        let mut maybe_seg = s.first_segment();
        while let Some(seg) = maybe_seg {
            let want = s.prev_gap_of_seg(&seg);
            let got = match s.prev_non_empty(&seg) {
                Some(SegOrGap::Segment(_)) => panic!("prev non empty element should be gap"),
                Some(SegOrGap::Gap(gap)) => Some(gap),
                None => None,
            };
            assert_eq!(got, want);

            let want = s.next_gap_of_seg(&seg);
            let got = match s.next_non_empty(&seg) {
                Some(SegOrGap::Gap(gap)) => Some(gap),
                Some(SegOrGap::Segment(_)) => panic!("next non empty element should be gap"),
                None => None,
            };
            assert_eq!(got, want);

            maybe_seg = s.next_segment_of_seg(&seg);
        }
    }

    #[test]
    fn merge_split() {
        #[derive(Default)]
        struct Test {
            _name: String,
            initial: Vec<Range<u64>>,
            split_addr: Option<i32>,
            result: Vec<Range<u64>>,
        }

        let tests = vec![
            Test {
                _name: "Add merges after existing segment".to_string(),
                initial: vec![
                    Range {
                        start: 1000,
                        end: 1100,
                    },
                    Range {
                        start: 1100,
                        end: 1200,
                    },
                ],
                result: vec![Range {
                    start: 1000,
                    end: 1200,
                }],
                ..Test::default()
            },
            Test {
                _name: "Add merges before existing segment".to_string(),
                initial: vec![
                    Range {
                        start: 1100,
                        end: 1200,
                    },
                    Range {
                        start: 1000,
                        end: 1100,
                    },
                ],
                result: vec![Range {
                    start: 1000,
                    end: 1200,
                }],
                ..Test::default()
            },
            Test {
                _name: "Add merges between existing segments".to_string(),
                initial: vec![
                    Range {
                        start: 1000,
                        end: 1100,
                    },
                    Range {
                        start: 1200,
                        end: 1300,
                    },
                    Range {
                        start: 1100,
                        end: 1200,
                    },
                ],
                result: vec![Range {
                    start: 1000,
                    end: 1300,
                }],
                ..Test::default()
            },
            Test {
                _name: "split_at does nothing at a free address".to_string(),
                initial: vec![Range {
                    start: 100,
                    end: 200,
                }],
                split_addr: Some(300),
                result: vec![Range {
                    start: 100,
                    end: 200,
                }],
            },
            Test {
                _name: "split_at does nothing at the beginning of a segment".to_string(),
                initial: vec![Range {
                    start: 100,
                    end: 200,
                }],
                split_addr: Some(100),
                result: vec![Range {
                    start: 100,
                    end: 200,
                }],
            },
            Test {
                _name: "split_at does nothing at the end of a segment".to_string(),
                initial: vec![Range {
                    start: 100,
                    end: 200,
                }],
                split_addr: Some(200),
                result: vec![Range {
                    start: 100,
                    end: 200,
                }],
            },
            Test {
                _name: "split_at splits in the middle of a segment".to_string(),
                initial: vec![Range {
                    start: 100,
                    end: 200,
                }],
                split_addr: Some(150),
                result: vec![
                    Range {
                        start: 100,
                        end: 150,
                    },
                    Range {
                        start: 150,
                        end: 200,
                    },
                ],
            },
        ];

        for test in tests {
            let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
            for r in &test.initial {
                assert!(s.add(*r, 0));
            }
            if let Some(split_at) = test.split_addr {
                s.split_at(split_at as u64);
            }
            let mut i = 0;
            let mut maybe_seg = s.first_segment();
            while let Some(seg) = maybe_seg {
                assert!(i < test.result.len());
                assert_eq!(seg.range, test.result[i]);
                i += 1;
                maybe_seg = s.next_segment_of_seg(&seg);
            }
            assert!(i >= test.result.len());
        }
    }

    #[test]
    fn isolate() {
        #[derive(Default, Debug)]
        struct Test<'a> {
            _name: &'a str,
            initial: Range<u64>,
            bounds: Range<u64>,
            result: Vec<Range<u64>>,
        }

        impl<'a> Test<'a> {
            fn is_superset_of(&self, r: Range<u64>) -> bool {
                self.bounds.start <= r.start && r.end <= self.bounds.end
            }
        }

        let tests = vec![
            Test {
                _name: "Isolate does not split a segment that falls inside bounds",
                initial: Range {
                    start: 100,
                    end: 200,
                },
                bounds: Range {
                    start: 100,
                    end: 200,
                },
                result: vec![Range {
                    start: 100,
                    end: 200,
                }],
            },
            Test {
                _name: "Isolate splits at beginning of segment",
                initial: Range {
                    start: 50,
                    end: 200,
                },
                bounds: Range {
                    start: 100,
                    end: 200,
                },
                result: vec![
                    Range {
                        start: 50,
                        end: 100,
                    },
                    Range {
                        start: 100,
                        end: 200,
                    },
                ],
            },
            Test {
                _name: "Isolate splits at end of segment",
                initial: Range {
                    start: 100,
                    end: 250,
                },
                bounds: Range {
                    start: 100,
                    end: 200,
                },
                result: vec![
                    Range {
                        start: 100,
                        end: 200,
                    },
                    Range {
                        start: 200,
                        end: 250,
                    },
                ],
            },
            Test {
                _name: "Isolate splits at beginning and end of segment",
                initial: Range {
                    start: 50,
                    end: 250,
                },
                bounds: Range {
                    start: 100,
                    end: 200,
                },
                result: vec![
                    Range {
                        start: 50,
                        end: 100,
                    },
                    Range {
                        start: 100,
                        end: 200,
                    },
                    Range {
                        start: 200,
                        end: 250,
                    },
                ],
            },
        ];

        for test in tests {
            let mut s: Set<u64, i32> = Set::new(Box::new(Ops {}));
            let seg = s.insert(test.initial, 0);
            let seg = s.isolate(&seg, test.bounds);
            assert!(test.is_superset_of(seg.range));
            let mut i = 0;
            let mut maybe_seg = s.first_segment();
            while let Some(seg) = maybe_seg {
                assert!(i <= test.result.len());
                assert_eq!(seg.range, test.result[i]);
                i += 1;
                maybe_seg = s.next_segment_of_seg(&seg);
            }
            assert!(i >= test.result.len());
        }
    }
}
