use std::{cell::RefCell, cmp::Ordering, collections::HashSet, hash::Hash, rc::Rc};

use mem::{Addr, AddrRange};
use segment::{SegOrGap, Set, SetOperations};
use utils::Range;

use crate::MemoryInvalidator;

use super::{InvalidateOpts, MappableRange};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MappingOfRange {
    addr_range: AddrRange,
    writable: bool,
}

impl Ord for MappingOfRange {
    fn cmp(&self, other: &Self) -> Ordering {
        self.addr_range.cmp(&other.addr_range)
    }
}

impl PartialOrd for MappingOfRange {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl MappingOfRange {
    fn invalidate(&self, opts: InvalidateOpts, mm: &Rc<RefCell<dyn MemoryInvalidator>>) {
        let mut mm = mm.borrow_mut();
        mm.invalidate(self.addr_range, opts);
    }
}

type MappingsOfRange = HashSet<MappingOfRange>;

pub struct MappingSetOperations;
impl SetOperations for MappingSetOperations {
    type K = u64;
    type V = MappingsOfRange;

    fn merge(
        &self,
        _: Range<Self::K>,
        v1: &Self::V,
        r2: Range<Self::K>,
        v2: &Self::V,
    ) -> Option<Self::V> {
        if v1.len() != v2.len() {
            return None;
        }
        let mut merged = MappingsOfRange::new();
        for k1 in v1.iter() {
            let k2 = MappingOfRange {
                addr_range: AddrRange {
                    start: k1.addr_range.end,
                    end: k1.addr_range.end + r2.len(),
                },
                writable: k1.writable,
            };
            if !v2.contains(&k2) {
                return None;
            }
            merged.insert(MappingOfRange {
                addr_range: AddrRange {
                    start: k1.addr_range.start,
                    end: k2.addr_range.end,
                },
                writable: k1.writable,
            });
        }
        Some(merged)
    }

    fn split(&self, r: Range<Self::K>, v: &Self::V, split: Self::K) -> (Self::V, Self::V) {
        if split <= r.start || split >= r.end {
            panic!("split is not within range: {:?}", r);
        }

        let mut m1 = MappingsOfRange::new();
        let mut m2 = MappingsOfRange::new();

        let offset = Addr(split - r.start);
        for k in v.iter() {
            let k1 = MappingOfRange {
                addr_range: AddrRange {
                    start: k.addr_range.start,
                    end: k.addr_range.start + offset.0,
                },
                writable: k.writable,
            };
            m1.insert(k1);

            let k2 = MappingOfRange {
                addr_range: AddrRange {
                    start: k.addr_range.start + offset.0,
                    end: k.addr_range.end,
                },
                writable: k.writable,
            };
            m2.insert(k2);
        }
        (m1, m2)
    }
}

pub type MappingSet = Set<u64, MappingsOfRange>;

pub trait SetU64MappingOfRange {
    fn invalidate(
        &mut self,
        mr: MappableRange,
        opts: InvalidateOpts,
        mm: &Rc<RefCell<dyn MemoryInvalidator>>,
    );
    fn add_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> Vec<MappableRange>;
    fn remove_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> Vec<MappableRange>;
}

impl SetU64MappingOfRange for MappingSet {
    fn invalidate(
        &mut self,
        mr: MappableRange,
        opts: InvalidateOpts,
        mm: &Rc<RefCell<dyn MemoryInvalidator>>,
    ) {
        let mut seg = self.lower_bound_segment(mr.start);
        while seg.map_or(false, |s| s.start() < mr.end) {
            let seg_inner = seg.unwrap();
            let seg_mr = seg_inner.range();
            for m in self.value(&seg_inner).iter() {
                let region = subset_mapping(
                    seg_mr,
                    seg_mr.intersect(&mr),
                    Addr(m.addr_range.start),
                    m.writable,
                );
                region.invalidate(opts, mm);
            }
            seg = self.next_segment_of_seg(&seg_inner);
        }
    }

    fn add_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> Vec<MappableRange> {
        let mr = MappableRange {
            start: offset,
            end: offset + ar.len(),
        };
        let mut mapped = Vec::new();
        let mut seg = self.find_segment(mr.start);
        let mut gap = self.find_gap(mr.start);
        loop {
            if seg.map_or(false, |s| s.start() < mr.end) {
                let seg_inner = self.isolate(&seg.unwrap(), mr);
                let val = self.value_mut(&seg_inner);
                val.insert(subset_mapping(
                    mr,
                    seg_inner.range(),
                    Addr(ar.start),
                    writable,
                ));
                match self.next_non_empty(&seg_inner) {
                    Some(SegOrGap::Gap(g)) => {
                        seg = None;
                        gap = Some(g);
                    }
                    Some(SegOrGap::Segment(s)) => {
                        seg = Some(s);
                        gap = None;
                    }
                    None => {
                        seg = None;
                        gap = None;
                    }
                }
            } else if gap.map_or(false, |g| g.start() < mr.end) {
                let gap_mr = gap.unwrap().range().intersect(&mr);
                mapped.push(gap_mr);
                seg = Some(self.insert(gap_mr, MappingsOfRange::new()));
                gap = None;
            } else {
                return mapped;
            }
        }
    }

    fn remove_mapping(&mut self, ar: AddrRange, offset: u64, writable: bool) -> Vec<MappableRange> {
        let mr = MappableRange {
            start: offset,
            end: offset + ar.len(),
        };
        let mut seg = self
            .find_segment(mr.start)
            .expect("remove_mapping: invalid mr");

        let mut unmapped = Vec::new();
        while seg.start() < mr.end {
            seg = self.isolate(&seg, mr);
            let mappings = self.value_mut(&seg);
            mappings.remove(&subset_mapping(mr, seg.range(), Addr(ar.start), writable));
            seg = if mappings.is_empty() {
                unmapped.push(seg.range());
                let removed = self.remove(seg.range());
                match self.next_segment_of_gap(&removed) {
                    Some(s) => s,
                    None => break,
                }
            } else {
                match self.next_segment_of_seg(&seg) {
                    Some(s) => s,
                    None => break,
                }
            };
        }
        self.merge_adjacant(mr);
        unmapped
    }
}

fn subset_mapping(
    whole_range: MappableRange,
    subset_range: MappableRange,
    addr: Addr,
    writable: bool,
) -> MappingOfRange {
    if !whole_range.is_superset_of(&subset_range) {
        panic!("invalid range");
    }
    let offset = subset_range.start - whole_range.start;
    let start = addr.0 + offset;
    MappingOfRange {
        addr_range: AddrRange {
            start,
            end: start + subset_range.len(),
        },
        writable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_mapping() {
        let ops = MappingSetOperations;
        let mut set = MappingSet::new(Box::new(ops));

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x10000,
                end: 0x12000,
            },
            0x1000,
            true,
        );
        assert_eq!(
            mapped,
            vec![MappableRange {
                start: 0x1000,
                end: 0x3000
            }]
        );

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x20000,
                end: 0x21000,
            },
            0x2000,
            true,
        );
        assert!(mapped.is_empty());

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x30000,
                end: 0x31000,
            },
            0x4000,
            true,
        );
        assert_eq!(
            mapped,
            vec![MappableRange {
                start: 0x4000,
                end: 0x5000
            }]
        );

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x12000,
                end: 0x15000,
            },
            0x3000,
            true,
        );
        assert_eq!(
            mapped,
            vec![
                MappableRange {
                    start: 0x3000,
                    end: 0x4000,
                },
                MappableRange {
                    start: 0x5000,
                    end: 0x6000,
                },
            ]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x10000,
                end: 0x11000,
            },
            0x1000,
            true,
        );
        assert_eq!(
            unmapped,
            vec![MappableRange {
                start: 0x1000,
                end: 0x2000
            }]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x20000,
                end: 0x21000,
            },
            0x2000,
            true,
        );
        assert!(unmapped.is_empty());

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x11000,
                end: 0x15000,
            },
            0x2000,
            true,
        );
        assert_eq!(
            unmapped,
            vec![
                MappableRange {
                    start: 0x2000,
                    end: 0x4000,
                },
                MappableRange {
                    start: 0x5000,
                    end: 0x6000,
                },
            ]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x30000,
                end: 0x31000,
            },
            0x4000,
            true,
        );
        assert_eq!(
            unmapped,
            vec![MappableRange {
                start: 0x4000,
                end: 0x5000
            }]
        );
    }

    // #[test]
    // fn invalidate_whole_mapping() {
    //     let ops = MappingSetOperations;
    //     let mut set = MappingSet::new(Box::new(ops));
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x10000,
    //             end: 0x11000,
    //         },
    //         0,
    //         true,
    //     );
    //     set.invalidate(
    //         MappableRange {
    //             start: 0,
    //             end: 0x1000,
    //         },
    //         &InvalidateOpts {
    //             invalidate_private: false,
    //         },
    //     );
    //     assert_eq!(
    //         ms.borrow().inv,
    //         vec![AddrRange {
    //             start: 0x10000,
    //             end: 0x11000
    //         }]
    //     );
    // }

    // #[test]
    // fn invalidate_partial_mapping() {
    //     let ops = MappingSetOperations;
    //     let mut set = MappingSet::new(Box::new(ops));
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x10000,
    //             end: 0x13000,
    //         },
    //         0,
    //         true,
    //     );
    //     set.invalidate(
    //         MappableRange {
    //             start: 0x1000,
    //             end: 0x2000,
    //         },
    //         &InvalidateOpts {
    //             invalidate_private: false,
    //         },
    //     );
    //     assert_eq!(
    //         ms.borrow().inv,
    //         vec![AddrRange {
    //             start: 0x11000,
    //             end: 0x12000
    //         }]
    //     );
    // }

    // #[test]
    // fn invalidate_multiple_mappings() {
    //     let ops = MappingSetOperations;
    //     let mut set = MappingSet::new(Box::new(ops));
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x10000,
    //             end: 0x11000,
    //         },
    //         0,
    //         true,
    //     );
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x20000,
    //             end: 0x21000,
    //         },
    //         0x2000,
    //         true,
    //     );
    //     set.invalidate(
    //         MappableRange {
    //             start: 0,
    //             end: 0x3000,
    //         },
    //         &InvalidateOpts {
    //             invalidate_private: false,
    //         },
    //     );
    //     assert_eq!(
    //         ms.borrow().inv,
    //         vec![
    //             AddrRange {
    //                 start: 0x10000,
    //                 end: 0x11000
    //             },
    //             AddrRange {
    //                 start: 0x20000,
    //                 end: 0x21000,
    //             }
    //         ]
    //     );
    // }

    // #[test]
    // fn invalidate_overlapping_mappings() {
    //     let ops = MappingSetOperations;
    //     let mut set = MappingSet::new(Box::new(ops));
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x10000,
    //             end: 0x12000,
    //         },
    //         0,
    //         true,
    //     );
    //     set.add_mapping(
    //         AddrRange {
    //             start: 0x20000,
    //             end: 0x22000,
    //         },
    //         0x1000,
    //         true,
    //     );
    //     set.invalidate(
    //         MappableRange {
    //             start: 0x1000,
    //             end: 0x2000,
    //         },
    //         &InvalidateOpts {
    //             invalidate_private: false,
    //         },
    //     );
    //     assert_eq!(
    //         ms1.borrow().inv,
    //         vec![AddrRange {
    //             start: 0x11000,
    //             end: 0x12000
    //         }]
    //     );
    //     assert_eq!(
    //         ms2.borrow().inv,
    //         vec![AddrRange {
    //             start: 0x20000,
    //             end: 0x21000
    //         }]
    //     );
    // }

    #[test]
    fn mixed_writeable_mappings() {
        let ops = MappingSetOperations;
        let mut set = MappingSet::new(Box::new(ops));

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x10000,
                end: 0x12000,
            },
            0x1000,
            true,
        );
        assert_eq!(
            mapped,
            vec![MappableRange {
                start: 0x1000,
                end: 0x3000
            }]
        );

        let mapped = set.add_mapping(
            AddrRange {
                start: 0x20000,
                end: 0x22000,
            },
            0x2000,
            false,
        );
        assert_eq!(
            mapped,
            vec![MappableRange {
                start: 0x3000,
                end: 0x4000
            }]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x20000,
                end: 0x21000,
            },
            0x2000,
            true,
        );
        assert!(unmapped.is_empty());

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x20000,
                end: 0x21000,
            },
            0x2000,
            false,
        );
        assert!(unmapped.is_empty());

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x11000,
                end: 0x12000,
            },
            0x2000,
            true,
        );
        assert_eq!(
            unmapped,
            vec![MappableRange {
                start: 0x2000,
                end: 0x3000
            }]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x10000,
                end: 0x12000,
            },
            0x1000,
            false,
        );
        assert!(unmapped.is_empty());

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x10000,
                end: 0x12000,
            },
            0x1000,
            true,
        );
        assert_eq!(
            unmapped,
            vec![MappableRange {
                start: 0x1000,
                end: 0x2000
            }]
        );

        let unmapped = set.remove_mapping(
            AddrRange {
                start: 0x21000,
                end: 0x22000,
            },
            0x3000,
            false,
        );
        assert_eq!(
            unmapped,
            vec![MappableRange {
                start: 0x3000,
                end: 0x4000
            }]
        );
    }
}
