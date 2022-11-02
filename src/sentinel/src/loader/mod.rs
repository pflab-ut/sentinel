use std::{collections::HashMap, path::Path, rc::Rc};

use arch::{ArchContext, Stack, StackVal};
use auth::Context;
use fs::{
    self,
    attr::{InodeType, PermMask},
    mount::MountNamespace,
    DirentWeakRef,
};
use goblin::{
    elf::{Elf, ProgramHeader},
    elf64::{
        header::ELFMAG,
        program_header::{PF_X, PT_INTERP, PT_LOAD},
    },
};
use mem::{io::Io, AccessType, Addr, IoOpts, IoSequence, PAGE_SIZE};
use memmap::mmap_opts::MmapOpts;
use rand::Rng;
use utils::{bail_libc, SysError, SysResult};

use crate::{context, kernel::Vdso, mm::MemoryManager};

const MAX_LOADER_ATTEMPS: u32 = 6;

static INTERPRETER_MAGIC: &[u8; 2] = b"#!";

#[derive(Debug)]
struct LoadedElf {
    entry: u64,
    start: Addr,
    end: Addr,
    interpreter: Option<String>,
    phdr_addr: Addr,
    phdr_size: u16,
    phdr_num: u16,
    auxv: HashMap<u64, Addr>,
}

pub struct Loader<'a> {
    root: DirentWeakRef,
    working_directory: DirentWeakRef,
    mm: &'a mut MemoryManager,
    argv: Vec<String>,
    envv: &'a HashMap<String, String>,
    mount: &'a MountNamespace,
}

impl<'a> Loader<'a> {
    pub fn new(
        mm: &'a mut MemoryManager,
        argv: Vec<String>,
        envv: &'a HashMap<String, String>,
        mount: &'a MountNamespace,
    ) -> Self {
        let root = Rc::downgrade(mount.root());
        // TODO: setting working directory to root for now.
        let working_directory = root.clone();
        Self {
            root,
            working_directory,
            mm,
            argv,
            envv,
            mount,
        }
    }

    pub fn load<P: AsRef<Path>>(
        &mut self,
        executable_path: P,
        extra_auxv: &HashMap<u64, Addr>,
    ) -> anyhow::Result<ArchContext> {
        // load the executable
        let (loaded, mut arch_context) = self.load_path(&executable_path, MAX_LOADER_ATTEMPS)?;
        let entry = loaded.entry;

        // load the vdso
        let vdso_addr = {
            let ctx = context::context();
            let kernel = ctx.kernel();
            let vdso = kernel.vdso();
            self.load_vdso(vdso)?
        };

        // setup the heap
        let e = loaded.end.round_up().ok_or_else(|| {
            SysError::new_with_msg(libc::ENOEXEC, format!("brk overflows: {:?}", loaded.end))
        })?;
        self.mm.brk_setup(e);

        // allocate our stack
        let mut stack = self.alloc_stack()?;
        stack.push(
            StackVal::Bytes(executable_path.as_ref().to_str().unwrap().as_bytes()),
            self.mm,
        )?;
        let execfn = stack.bottom();

        let mut rand_bytes = [0; 16];
        rand::thread_rng().fill(&mut rand_bytes);
        stack.push(StackVal::Bytes(&rand_bytes), self.mm).unwrap();
        let random = stack.bottom();

        let ctx = &*context::context();
        let creds = ctx.credentials();
        let un = &creds.user_namespace;

        let mut auxv = loaded.auxv;
        auxv.insert(
            libc::AT_UID,
            Addr(un.map_from_kuid(&creds.real_kuid).or_overflow().0 as u64),
        );
        auxv.insert(
            libc::AT_EUID,
            Addr(un.map_from_kuid(&creds.effective_kuid).or_overflow().0 as u64),
        );
        auxv.insert(
            libc::AT_GID,
            Addr(un.map_from_kgid(&creds.real_kgid).or_overflow().0 as u64),
        );
        auxv.insert(
            libc::AT_EGID,
            Addr(un.map_from_kgid(&creds.effective_kgid).or_overflow().0 as u64),
        );
        auxv.insert(libc::AT_SECURE, Addr(0));
        auxv.insert(libc::AT_CLKTCK, Addr(100));
        auxv.insert(libc::AT_EXECFN, Addr(execfn));
        auxv.insert(libc::AT_RANDOM, Addr(random));
        auxv.insert(libc::AT_PAGESZ, Addr(PAGE_SIZE as u64));
        auxv.insert(linux::AT_SYSINFO_EHDR, vdso_addr);
        auxv.extend(extra_auxv);

        let stack_layout = stack.load(&self.argv, self.envv, &auxv, self.mm)?;

        self.mm.set_argv_start(stack_layout.argv_start);
        self.mm.set_argv_end(stack_layout.argv_end);
        self.mm.set_envv_start(stack_layout.envv_start);
        self.mm.set_envv_end(stack_layout.envv_end);
        self.mm.set_auxv(auxv);

        logger::debug!("rip: 0x{:x?}", entry);
        arch_context.regs.rip = entry;
        logger::debug!("stack bottom: {:x?}", stack.bottom());
        arch_context.regs.rsp = stack.bottom();

        self.mm.print_vmas();

        Ok(arch_context)
    }

    fn load_path<P: AsRef<Path>>(
        &mut self,
        target_elf_path: P,
        remaning_attemps: u32,
    ) -> SysResult<(LoadedElf, ArchContext)> {
        if remaning_attemps == 0 {
            bail_libc!(libc::ELOOP);
        }
        let mut f = self.open_path(target_elf_path.as_ref())?;
        let mut hdr = [0; 4];
        let mut dst = IoSequence::bytes_sequence(&mut hdr);
        {
            let ctx = &*context::context();
            f.read_full(&mut dst, 0, ctx)?;
        }
        if &hdr == ELFMAG {
            self.load_elf(&mut f)
        } else if &hdr[..2] == INTERPRETER_MAGIC {
            let path = target_elf_path.as_ref().to_str().unwrap().to_string();
            let target = self.parse_interpreter_script(path, &f)?;
            self.load_path(target, remaning_attemps - 1)
        } else {
            panic!("Unknown header: {:?}", hdr);
        }
    }

    fn load_elf(&mut self, file: &mut fs::File) -> SysResult<(LoadedElf, ArchContext)> {
        let (mut bin, arch_context) = self.load_initial_elf(file)?;
        let mut auxv = HashMap::new();
        auxv.insert(libc::AT_PHDR, bin.phdr_addr);
        auxv.insert(libc::AT_PHENT, Addr(bin.phdr_size as u64));
        auxv.insert(libc::AT_PHNUM, Addr(bin.phdr_num as u64));
        auxv.insert(libc::AT_ENTRY, Addr(bin.entry));
        match bin.interpreter {
            Some(ref interpreter) => {
                let mut f = self.open_path(&interpreter)?;
                let i = self.load_interpreter_elf(&mut f)?;
                if i.interpreter.is_some() {
                    panic!("No recursive interpreter's");
                }
                bin.entry = i.entry;
                auxv.insert(libc::AT_BASE, i.start);
            }
            None => {
                auxv.insert(libc::AT_BASE, Addr(0));
            }
        }
        bin.auxv = auxv;
        logger::debug!("loaded elf: {:?}", bin);
        Ok((bin, arch_context))
    }

    fn load_interpreter_elf(&mut self, file: &mut fs::File) -> SysResult<LoadedElf> {
        self.load_parsed_elf(file, Addr(0))
    }

    fn load_initial_elf(&mut self, file: &mut fs::File) -> SysResult<(LoadedElf, ArchContext)> {
        let arch_context = ArchContext::new();
        let layout = self.mm.set_mmap_layout()?;
        let elf = self.load_parsed_elf(file, layout.pie_load_address())?;
        Ok((elf, arch_context))
    }

    fn load_parsed_elf(
        &mut self,
        file: &mut fs::File,
        shared_load_offset: Addr,
    ) -> SysResult<LoadedElf> {
        let size = file.get_file_size()?;
        let mut buf = vec![0; size];
        let mut dst = IoSequence::bytes_sequence(&mut buf);
        let ctx = &*context::context();
        file.read_full(&mut dst, 0, ctx)?;
        let elf = Elf::parse(&buf).expect("failed to parse elf");

        let mut start = None;
        let mut end = Addr(0);
        for prog_hdr in &elf.program_headers {
            let p_type = prog_hdr.p_type;
            if p_type == PT_LOAD {
                let vaddr = Addr(prog_hdr.p_vaddr);
                if start.is_none() {
                    start = Some(vaddr);
                }
                if vaddr < end {
                    panic!("PT_LOAD headers out-of-order");
                }
                end = vaddr
                    .add_length(prog_hdr.p_memsz)
                    .expect("PT_LOAD header size overflows");
            } else if p_type == PT_INTERP {
                if prog_hdr.p_filesz < 2 || prog_hdr.p_filesz > libc::PATH_MAX as u64 {
                    panic!("PT_INTERP invalid path size");
                }
                if elf.interpreter.is_none() {
                    panic!("PT_INTERP path is empty");
                }
            }
        }
        let mut start = start.unwrap();

        let (entry, offset) = if !elf.is_lib {
            (elf.entry, Addr(0))
        } else {
            let total_size = (end - start).round_up().ok_or_else(|| {
                logger::error!("ELF PT_LOAD segments too big");
                SysError::new(libc::ENOEXEC)
            })?;
            let offset = self.mm.mmap(MmapOpts {
                length: total_size.0 as u64,
                addr: shared_load_offset,
                private: true,
                ..MmapOpts::default()
            })?;
            self.mm
                .munmap(offset, total_size.0 as u64)
                .unwrap_or_else(|err| panic!("Failed to unmap base address: {:?}", err));
            start = start.add_length(offset.0).unwrap();
            end = end.add_length(offset.0).unwrap();
            let entry = Addr(elf.entry)
                .add_length(offset.0)
                .expect("entrypoint overflows");
            (entry.0, offset)
        };

        // map PT_LOAD segments
        for prog_hdr in &elf.program_headers {
            if prog_hdr.p_type == PT_LOAD {
                if prog_hdr.p_memsz == 0 {
                    continue;
                }
                self.map_segment(file, prog_hdr, offset)?;
            }
        }

        let phdr_addr = start.add_length(elf.header.e_phoff).unwrap_or_else(|| {
            logger::error!(
                "ELF start address {} + program header offset {} overflows",
                start,
                elf.header.e_phoff
            );
            Addr(0)
        });

        Ok(LoadedElf {
            entry,
            start,
            end,
            interpreter: elf.interpreter.map(|s| s.to_string()),
            phdr_addr,
            phdr_size: elf.header.e_phentsize,
            phdr_num: elf.header.e_phnum,
            auxv: HashMap::new(),
        })
    }

    fn load_vdso(&mut self, vdso: &Vdso) -> SysResult<Addr> {
        let vdso_len = vdso.vdso.borrow().len();
        let map_size = vdso_len + vdso.param_page.borrow().len();
        let addr = self.mm.mmap(MmapOpts {
            length: map_size,
            private: true,
            ..MmapOpts::default()
        })?;

        let mmap_opts = MmapOpts {
            length: vdso.param_page.borrow().len(),
            mappable: Some(vdso.param_page.clone()),
            addr,
            fixed: true,
            unmap: true,
            private: true,
            perms: AccessType::read(),
            max_perms: AccessType::read(),
            ..MmapOpts::default()
        };
        self.mm.mmap(mmap_opts)?;

        let vdso_addr = addr.add_length(vdso.param_page.borrow().len()).unwrap();
        let mmap_opts = MmapOpts {
            length: vdso_len,
            mappable: Some(vdso.vdso.clone()),
            addr: vdso_addr,
            fixed: true,
            unmap: true,
            private: true,
            perms: AccessType::read(),
            max_perms: AccessType::any_access(),
            ..MmapOpts::default()
        };
        self.mm.mmap(mmap_opts)?;

        let vdso_end = vdso_addr.add_length(vdso_len).unwrap();

        let mut first_vaddr = None;
        for prog_hdr in &vdso.phdrs {
            if prog_hdr.p_type != PT_LOAD {
                continue;
            }
            if first_vaddr.is_none() {
                first_vaddr = Some(prog_hdr.p_vaddr);
            }
            let memory_offset = prog_hdr.p_vaddr - first_vaddr.unwrap();
            let seg_addr = vdso_addr.add_length(memory_offset).unwrap();
            let seg_page = seg_addr.round_down();
            let seg_size = Addr(prog_hdr.p_memsz);
            let seg_size = seg_size.add_length(seg_addr.page_offset()).unwrap();
            let seg_size = seg_size.round_up().unwrap();
            let seg_end = seg_page.add_length(seg_size.0).unwrap();
            if seg_end > vdso_end {
                logger::error!("PT_LOAD segments ends beyond VDSO");
                bail_libc!(libc::ENOEXEC);
            }
            let perms = AccessType::from_elf_prog_flags(prog_hdr.p_flags);
            if perms != AccessType::read() {
                self.mm.mprotect(seg_page, seg_size.0, perms, false)?;
            }
        }

        Ok(vdso_addr)
    }

    fn open_path<P: AsRef<Path>>(&self, filename: P) -> SysResult<fs::File> {
        let mut max_symlink_traversals = linux::MAX_SYMLINK_TRAVERSALS;
        let ctx = &*context::context();
        let dirent = self.mount.find_inode(
            &self.root.upgrade().unwrap(),
            Some(self.working_directory.upgrade().unwrap()),
            &filename,
            &mut max_symlink_traversals,
            ctx,
        )?;
        let dirent_ref = dirent.borrow();
        let inode = dirent_ref.inode();
        let sattr = inode.stable_attr();
        if sattr.is_symlink() {
            panic!("trying to load a symlink (should call find_link() in the future)");
        }
        let perms = PermMask {
            read: true,
            write: false,
            execute: true,
        };
        inode.check_permission(perms, ctx)?;

        if filename.as_ref().is_dir() && sattr.typ != InodeType::Directory {
            bail_libc!(libc::ENOTDIR);
        }

        if !sattr.is_regular() {
            logger::error!("not a regular file: {:?}", sattr);
            bail_libc!(libc::EACCES);
        }

        inode.get_file(
            dirent.clone(),
            fs::FileFlags::from_linux_flags(libc::O_RDONLY),
        )
    }

    fn alloc_stack(&mut self) -> SysResult<Stack> {
        let ar = self.mm.map_stack()?;
        Ok(Stack::new(Addr(ar.end)))
    }

    fn map_segment(
        &mut self,
        file: &mut fs::File,
        prog_hdr: &ProgramHeader,
        offset: Addr,
    ) -> SysResult<()> {
        let adjust = Addr(prog_hdr.p_vaddr).page_offset();
        let addr = offset.add_length(prog_hdr.p_vaddr).ok_or_else(|| {
            logger::error!("segment load address overflows");
            SysError::new(libc::ENOEXEC)
        })?;
        let addr = addr - Addr(adjust);

        let file_size = prog_hdr.p_filesz + adjust;
        let ms = Addr(file_size).round_up().unwrap();
        let map_size = ms.0;

        if map_size > 0 {
            let file_offset = prog_hdr.p_offset - adjust;
            let perms = AccessType::from_elf_prog_flags(prog_hdr.p_flags);
            let mut mopts = MmapOpts {
                length: map_size,
                offset: file_offset,
                addr,
                fixed: true,
                unmap: true,
                private: true,
                perms,
                max_perms: AccessType::any_access(),
                ..MmapOpts::default()
            };
            file.configure_mmap(&mut mopts)?;
            self.mm.mmap(mopts)?;
            if map_size > file_size {
                let zero_addr = addr
                    .add_length(file_size)
                    .expect("successfully mapped address overflows?");
                if map_size < file_size {
                    panic!("zero file too big?");
                }
                let zero_size = map_size - file_size;
                self.mm.zero_out(
                    zero_addr,
                    zero_size as i64,
                    &IoOpts {
                        ignore_permissions: true,
                    },
                )?;
            }
        }

        let mem_size = prog_hdr
            .p_memsz
            .checked_add(adjust)
            .unwrap_or_else(|| panic!("computed segment mem size overflows"));

        if map_size < mem_size {
            let anon_addr = addr
                .add_length(map_size)
                .expect("anonymous memory doesn't fit in pre-sized range?");
            let anon_size = Addr(mem_size - map_size)
                .round_up()
                .expect("extra anon pages too large");
            let prot = if prog_hdr.p_flags & PF_X == PF_X {
                AccessType::any_access()
            } else {
                AccessType::read_write()
            };
            self.mm.mmap(MmapOpts {
                length: anon_size.0 as u64,
                addr: anon_addr,
                fixed: true,
                private: true,
                perms: prot,
                max_perms: AccessType::any_access(),
                ..MmapOpts::default()
            })?;
        }

        Ok(())
    }

    fn parse_interpreter_script(&mut self, filename: String, file: &fs::File) -> SysResult<String> {
        let mut first_line = vec![0; 127];
        let mut dst = IoSequence::bytes_sequence(&mut first_line);
        let ctx = &*context::context();
        let n = file.read_full(&mut dst, 0, ctx)?;
        let first_line = &first_line[..n];

        let (magic, mut first_line) = first_line.split_at(2);
        if magic != INTERPRETER_MAGIC {
            bail_libc!(libc::ENOEXEC);
        }

        if let Some(i) = first_line.iter().position(|&c| c == b'\n') {
            first_line = &first_line[..i];
        }
        let first_line = first_line
            .iter()
            .skip_while(|b| **b == b' ' || **b == b'\t')
            .copied()
            .collect::<Vec<_>>();

        let (interp, args) = match first_line.iter().position(|c| *c == b' ' || *c == b'\t') {
            Some(i) => first_line.split_at(i),
            None => (&first_line as &[u8], &[] as &[u8]),
        };

        if interp.is_empty() {
            bail_libc!(libc::ENOEXEC);
        }

        let interp = std::str::from_utf8(interp).map_err(|_| SysError::new(libc::EINVAL))?;
        let mut new_argv = vec![interp.to_string()];
        if !args.is_empty() {
            let args = std::str::from_utf8(args).map_err(|_| SysError::new(libc::EINVAL))?;
            new_argv.push(args.to_string());
        }
        if self.argv.is_empty() {
            self.argv.push(filename);
        } else {
            self.argv[0] = filename;
        }
        new_argv.extend_from_slice(&self.argv);
        self.argv = new_argv;
        Ok(interp.to_string())
    }
}
