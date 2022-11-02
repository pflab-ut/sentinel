use core::arch::x86_64::{CpuidResult, __cpuid_count};
use std::{collections::HashMap, convert::TryInto};

#[derive(Debug)]
pub struct FeatureSet {
    set: HashMap<i32, bool>,
    vendor_id: String,
    extended_family: u8,
    extended_model: u8,
    processor_type: u8,
    family: u8,
    model: u8,
    stepping_id: u8,
    caches: Vec<Cache>,
    cache_line: u32,
}

fn feature_id(b: i32, bit: i32) -> i32 {
    32 * b + bit
}

fn set_from_block_masks(blocks: &[u32]) -> HashMap<i32, bool> {
    let mut s = HashMap::new();
    for (b, bm) in blocks.iter().enumerate() {
        let mut block_mask = *bm;
        for i in 0..32 {
            if block_mask & 1 != 0 {
                s.insert(feature_id(b as i32, i), true);
            }
            block_mask >>= 1;
        }
    }
    s
}

impl FeatureSet {
    pub fn new() -> Self {
        let CpuidResult { ebx, ecx, edx, .. } = unsafe { __cpuid_count(0, 0) };
        let vendor_id = format!(
            "{}{}{}",
            std::str::from_utf8(&ebx.to_le_bytes()).unwrap(),
            std::str::from_utf8(&edx.to_le_bytes()).unwrap(),
            std::str::from_utf8(&ecx.to_le_bytes()).unwrap()
        );
        assert_eq!(&vendor_id, "GenuineIntel");

        let CpuidResult { eax, ebx, ecx, edx } = unsafe { __cpuid_count(1, 0) };
        let feature_block0 = ecx;
        let feature_block1 = edx;
        let (extended_family, extended_model, processor_type, family, model, stepping_id) =
            signature_split(eax);
        let cache_line = (8 * (ebx >> 8)) & 0xff;
        let mut caches = Vec::new();
        for i in 0.. {
            let CpuidResult { eax, ebx, ecx, edx } = unsafe { __cpuid_count(4, i) };
            let typ = CacheType::from_u32(eax & 0xf);
            if typ == CacheType::Null {
                break;
            }
            let line_size = (ebx & 0xfff) + 1;
            if line_size != cache_line {
                panic!(
                    "mismatched cache line size: {} vs {}",
                    line_size, cache_line
                );
            }
            caches.push(Cache {
                level: (eax >> 5) & 0x7,
                typ,
                fully_associative: ((eax >> 9) & 1) == 1,
                partitions: ((ebx >> 12) & 0x3ff) + 1,
                ways: ((ebx >> 22) & 0x3ff) + 1,
                sets: ecx + 1,
                invalidate_hierarchical: (edx & 1) == 0,
                inclusive: ((edx >> 1) & 1) == 1,
                direct_mapped: ((edx >> 2) & 1) == 0,
            });
        }

        let CpuidResult { ebx, ecx, .. } = unsafe { __cpuid_count(7, 0) };
        let feature_block2 = ebx;
        let feature_block3 = ecx;
        let feature_block4 = if feature_block0 & (1 << 26) != 0 {
            unsafe { __cpuid_count(CpuIdFunction::XSaveInfo as u32, 1).eax }
        } else {
            0
        };
        let (feature_block5, feature_block6) =
            if unsafe { __cpuid_count(CpuIdFunction::ExtendedFeatureInfo as u32, 0).eax }
                >= CpuIdFunction::ExtendedFeatureInfo as u32
            {
                let CpuidResult { ecx, edx, .. } =
                    unsafe { __cpuid_count(CpuIdFunction::ExtendedFeatureInfo as u32, 0) };
                (ecx, edx & !BLOCK6_DUP_MASK)
            } else {
                (0, 0)
            };
        let set = set_from_block_masks(&[
            feature_block0,
            feature_block1,
            feature_block2,
            feature_block3,
            feature_block4,
            feature_block5,
            feature_block6,
        ]);
        FeatureSet {
            set,
            vendor_id,
            extended_family,
            extended_model,
            processor_type,
            family,
            model,
            stepping_id,
            cache_line,
            caches,
        }
    }

    pub fn emulate_id(&self, orig_ax: u32, orig_cx: u32) -> (u32, u32, u32, u32) {
        match CpuIdFunction::from_u32(orig_ax) {
            CpuIdFunction::VendorId => {
                let ax = CpuIdFunction::XSaveInfo as u32;
                let (bx, dx, cx) = self.vendor_id_regs();
                (ax, bx, cx, dx)
            }
            CpuIdFunction::FeatureInfo => {
                let bx = (self.cache_line / 8) << 8;
                let cx = self.block_mask(0);
                let dx = self.block_mask(1);
                let ax = self.signature();
                (ax, bx, cx, dx)
            }
            CpuIdFunction::IntelCacheDescriptors => {
                let ax = 1 | ((IntelCacheDescriptor::NoCache as u32) << 8);
                (ax, 0, 0, 0)
            }
            CpuIdFunction::IntelDeterministicCacheParams => {
                if (orig_cx as usize) >= self.caches.len() {
                    return (CacheType::Null as u32, 0, 0, 0);
                }
                let c = match self.caches.get(orig_cx as usize) {
                    Some(c) => *c,
                    None => return (CacheType::Null as u32, 0, 0, 0),
                };
                let ax = c.typ as u32;
                let ax = ax | c.level << 5;
                let mut ax = ax | (1 << 8);
                if c.fully_associative {
                    ax |= 1 << 9;
                }
                let bx = (self.cache_line - 1) | ((c.partitions - 1) << 12) | ((c.ways - 1) << 22);
                let cx = c.sets - 1;
                let mut dx = 0;
                if !c.invalidate_hierarchical {
                    dx |= 1;
                }
                if c.inclusive {
                    dx |= 1 << 1;
                }
                if !c.direct_mapped {
                    dx |= 1 << 2;
                }
                (ax, bx, cx, dx)
            }
            CpuIdFunction::XSaveInfo => {
                if !self.use_xsave() {
                    (0, 0, 0, 0)
                } else {
                    let res = unsafe { __cpuid_count(CpuIdFunction::XSaveInfo as u32, orig_cx) };
                    (res.eax, res.ebx, res.ecx, res.edx)
                }
            }
            CpuIdFunction::ExtendedFeatureInfo => {
                if orig_cx != 0 {
                    (0, 0, 0, 0)
                } else {
                    let bx = self.block_mask(2);
                    let cx = self.block_mask(3);
                    (0, bx, cx, 0)
                }
            }
            CpuIdFunction::ExtendedFunctionInfo => {
                let ax = CpuIdFunction::ExtendedFeatures as u32;
                (ax, 0, 0, 0)
            }
            CpuIdFunction::ExtendedFeatures => {
                let cx = self.block_mask(5);
                let dx = self.block_mask(6);
                (0, 0, cx, dx)
            }
            _ => (0, 0, 0, 0),
        }
    }

    fn has_feature(&self, f: i32) -> bool {
        *self.set.get(&f).unwrap()
    }

    fn use_xsave(&self) -> bool {
        self.has_feature(Feature::X86FeatureXsave as i32)
            && self.has_feature(Feature::X86FeatureOsxsave as i32)
    }

    fn vendor_id_regs(&self) -> (u32, u32, u32) {
        assert_eq!(self.vendor_id.as_bytes().len(), 12);
        let bx = u32::from_le_bytes(self.vendor_id.as_bytes()[..4].try_into().unwrap());
        let dx = u32::from_le_bytes(self.vendor_id.as_bytes()[4..8].try_into().unwrap());
        let cx = u32::from_le_bytes(self.vendor_id.as_bytes()[8..].try_into().unwrap());
        (bx, dx, cx)
    }

    fn block_mask(&self, b: i32) -> u32 {
        let mut mask = 0;
        for i in 0..32 {
            if *self.set.get(&feature_id(b, i)).unwrap() {
                mask |= 1 << (i as u32);
            }
        }
        mask
    }

    fn signature(&self) -> u32 {
        let mut s = 0;
        s |= (self.stepping_id & 0xf) as u32;
        s |= ((self.model & 0xf) as u32) << 4;
        s |= ((self.family & 0xf) as u32) << 8;
        s |= ((self.processor_type & 0x3) as u32) << 12;
        s |= ((self.extended_model & 0xf) as u32) << 16;
        s |= (self.extended_family as u32) << 20;
        s
    }
}

static BLOCK6_DUP_MASK: u32 = 0x183f3ff;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum CacheType {
    Null,
    Data,
    Instruction,
    Unified,
}

impl CacheType {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Null,
            1 => Self::Data,
            2 => Self::Instruction,
            3 => Self::Unified,
            _ => panic!("invalid u32 for CacheType"),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Cache {
    level: u32,
    typ: CacheType,
    fully_associative: bool,
    partitions: u32,
    ways: u32,
    sets: u32,
    invalidate_hierarchical: bool,
    inclusive: bool,
    direct_mapped: bool,
}

#[allow(dead_code)]
enum CpuIdFunction {
    VendorId,
    FeatureInfo,
    IntelCacheDescriptors,
    IntelSerialNumber,
    IntelDeterministicCacheParams,
    MonitorMwaitParams,
    PowerParams,
    ExtendedFeatureInfo,
    _Reserved0x8,
    IntelDcaParams,
    IntelPmcInfo,
    IntelX2ApicInfo,
    _Reserved0xc,
    XSaveInfo,
    ExtendedFunctionInfo = 0x80000000,
    ExtendedFeatures,
}

impl CpuIdFunction {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::VendorId,
            1 => Self::FeatureInfo,
            2 => Self::IntelCacheDescriptors,
            3 => Self::IntelSerialNumber,
            4 => Self::IntelDeterministicCacheParams,
            5 => Self::MonitorMwaitParams,
            6 => Self::PowerParams,
            7 => Self::ExtendedFeatureInfo,
            8 => panic!("function 0x8 is reserved"),
            9 => Self::IntelDcaParams,
            10 => Self::IntelPmcInfo,
            11 => Self::IntelX2ApicInfo,
            12 => panic!("function 0x8 is reserved"),
            13 => Self::XSaveInfo,
            _ => panic!("invalid u32 for CpuIdFunction"),
        }
    }
}

#[allow(dead_code)]
enum IntelCacheDescriptor {
    Null = 0,
    NoTlb = 0xfe,
    NoCache = 0xff,
}

#[allow(dead_code)]
enum Feature {
    X86FeatureSse3,
    X86FeaturePclmuldq,
    X86FeatureDtes64,
    X86FeatureMonitor,
    X86FeatureDscpl,
    X86FeatureVmx,
    X86FeatureSmx,
    X86FeatureEst,
    X86FeatureTm2,
    X86FeatureSsse3,
    X86FeatureCnxtid,
    X86FeatureSdbg,
    X86FeatureFma,
    X86FeatureCx16,
    X86FeatureXtpr,
    X86FeaturePdcm,
    _Reserved, // ecx bit 16 is reserved.
    X86FeaturePcid,
    X86FeatureDca,
    X86FeatureSse4_1,
    X86FeatureSse4_2,
    X86FeatureX2apic,
    X86FeatureMovbe,
    X86FeaturePopcnt,
    X86FeatureTscd,
    X86FeatureAes,
    X86FeatureXsave,
    X86FeatureOsxsave,
    X86FeatureAvx,
    X86FeatureF16c,
    X86FeatureRdrand,
    X86FeatureHypervisor,
}

fn signature_split(v: u32) -> (u8, u8, u8, u8, u8, u8) {
    (
        (v & 0xf) as u8,
        ((v >> 4) & 0xf) as u8,
        ((v >> 8) & 0xf) as u8,
        ((v >> 12) & 0xf) as u8,
        ((v >> 16) & 0xf) as u8,
        ((v >> 20) & 0xf) as u8,
    )
}
