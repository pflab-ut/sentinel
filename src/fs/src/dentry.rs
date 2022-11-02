use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound,
};

use crate::{attr::InodeType, ReaddirError, ReaddirResult};

#[derive(Clone, Copy, Debug)]
pub struct DentAttr {
    pub typ: InodeType,
    pub inode_id: u64,
}

pub trait DentrySerializer {
    fn copy_out(&mut self, name: &str, attr: DentAttr) -> std::io::Result<()>;
    fn written_bytes(&self) -> usize;
}

pub struct DirIterCtx<'a> {
    pub serializer: &'a mut dyn DentrySerializer,
    pub attrs: HashMap<String, DentAttr>,
    pub dir_cursor: Option<&'a mut String>,
}

impl DirIterCtx<'_> {
    pub fn dir_emit(&mut self, name: String, attr: DentAttr) -> std::io::Result<()> {
        self.serializer.copy_out(&name, attr)?;
        self.attrs.insert(name, attr);
        Ok(())
    }
}

pub fn generic_readdir(
    dir_ctx: &mut DirIterCtx,
    map: &BTreeMap<String, DentAttr>,
) -> ReaddirResult<i32> {
    let from = match &dir_ctx.dir_cursor {
        Some(s) => (*s).clone(),
        None => String::new(),
    };
    let mut serialized = 0;
    for (name, dent_attr) in map.range((Bound::Excluded(from), Bound::Unbounded)) {
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        dir_ctx
            .dir_emit(name.to_string(), *dent_attr)
            .map_err(|e| ReaddirError::new(serialized, e.raw_os_error().unwrap_or(-1)))?;
        serialized += 1;
        if let Some(ref mut cursor) = dir_ctx.dir_cursor {
            **cursor = name.clone();
        }
    }
    Ok(serialized)
}
