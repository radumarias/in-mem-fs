use std::cmp::min;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::raw::c_int;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytebuffer::ByteBuffer;
use fuser::{FileAttr, Filesystem, FileType, KernelConfig, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow};
use fuser::consts::FOPEN_DIRECT_IO;
use fuser::TimeOrNow::Now;
use libc::{ENOENT, ENOSYS};
use log::{debug, warn};

use crate::tree_fs::{Item, TreeFs};

const BLOCK_SIZE: u64 = 512;

const FMODE_EXEC: i32 = 0x20;

pub struct MemFs {
    tree_fs: TreeFs<FileAttr>,
    direct_io: bool,
    suid_support: bool,
    current_inode: u64,
    current_file_handle: u64,
}

impl MemFs {
    // pub fn new_sample(direct_io: bool, suid_support: bool) -> Self {
    //     MemFs {
    //         tree_fs: generate_sample_tree(),
    //         direct_io,
    //         suid_support,
    //     }
    // }

    pub fn new(direct_io: bool, _suid_support: bool) -> Self {
        #[cfg(feature = "abi-7-26")]
        {
            MemFs {
                tree_fs: TreeFs::new(),
                direct_io,
                suid_support: _suid_support,
                current_inode: 1,
                current_file_handle: 0,
            }
        }
        #[cfg(not(feature = "abi-7-26"))] {
            MemFs {
                tree_fs: TreeFs::new(),
                direct_io,
                suid_support: false,
                current_inode: 1,
                current_file_handle: 0,
            }
        }
    }

    fn creation_mode(&self, mode: u32) -> u16 {
        if !self.suid_support {
            (mode & !(libc::S_ISUID | libc::S_ISGID) as u32) as u16
        } else {
            mode as u16
        }
    }

    fn allocate_next_inode(&mut self) -> u64 {
        self.current_inode += 1;

        self.current_inode
    }

    fn create_nod(&mut self, parent: u64, mut mode: u32, req: &Request, name: &OsStr) -> Result<FileAttr, c_int> {
        match self.tree_fs.get_item_mut(parent) {
            Some(parent) => {
                if !parent.is_dir {
                    return Err(ENOENT);
                }

                if parent.find_child_mut(name.to_str().unwrap()).is_some() {
                    return Err(libc::EEXIST);
                }

                let parent_attr = parent.extra.as_mut().unwrap();

                if !check_access(
                    parent_attr.uid,
                    parent_attr.gid,
                    parent_attr.perm,
                    req.uid(),
                    req.gid(),
                    libc::W_OK,
                ) {
                    return Err(libc::EACCES);
                }

                parent_attr.mtime = SystemTime::now();
                parent_attr.ctime = SystemTime::now();

                if req.uid() != 0 {
                    mode &= !(libc::S_ISUID | libc::S_ISGID) as u32;
                }

                let kind = as_file_kind(mode);
                let ino = self.allocate_next_inode();
                let mut attr = if kind == FileType::Directory {
                    dir_attr(ino)
                } else {
                    file_attr(ino, 0)
                };
                attr.perm = self.creation_mode(mode);
                attr.uid = req.uid();
                attr.gid = creation_gid(&parent_attr, req.gid());

                self.tree_fs.push(&parent, Item::new(ino, name.to_str().unwrap().to_string(), kind == FileType::Directory, Some(attr)));

                Ok(attr)
            }
            None => Err(ENOENT),
        }
    }

    fn allocate_next_file_handle(&mut self) -> u64 {
        self.current_file_handle += 1;

        self.current_file_handle
    }
}

impl Filesystem for MemFs {
    fn init(
        &mut self,
        _req: &Request,
        #[allow(unused_variables)] config: &mut KernelConfig,
    ) -> Result<(), c_int> {
        #[cfg(feature = "abi-7-26")]
        config.add_capabilities(FUSE_HANDLE_KILLPRIV).unwrap();

        if self.tree_fs.get_root().is_none() {
            let root = Item::new(1, String::from("root"), true, Some(dir_attr(1)));
            self.tree_fs.set_root(root);
        }
        Ok(())
    }

    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup {}, {}", parent, name.to_str().unwrap());

        match self.tree_fs.get_item_mut(parent) {
            Some(parent_item) => {
                let parent_attr = parent_item.extra.as_ref().unwrap();
                if !check_access(
                    parent_attr.uid,
                    parent_attr.gid,
                    parent_attr.perm,
                    req.uid(),
                    req.gid(),
                    libc::X_OK,
                ) {
                    reply.error(libc::EACCES);
                    return;
                }

                match parent_item.find_child_mut(name.to_str().unwrap()) {
                    Some(child) => {
                        if child.is_dir {
                            debug!("  dir {}", child.ino);
                            reply.entry(&Duration::new(0, 0), &&child.extra.as_ref().unwrap(), 0);
                        } else {
                            debug!("  file {}", child.ino);
                            reply.entry(&Duration::new(0, 0), &&child.extra.as_ref().unwrap(), 0);
                        }
                    }
                    None => {
                        debug!("  not found");
                        reply.error(ENOENT);
                    }
                }
            }
            None => {
                debug!("  not found");
                reply.error(ENOENT)
            }
        }
    }

    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {
        debug!("forget() called with {:?} {:?}", _ino, _nlookup);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        debug!("getattr {}", ino);

        match self.tree_fs.get_item_mut(ino) {
            Some(item) => {
                if item.is_dir {
                    debug!("  dir {}", ino);
                    reply.attr(&Duration::new(0, 0), &item.extra.as_ref().unwrap());
                } else {
                    debug!("  file {}", ino);
                    reply.attr(&Duration::new(0, 0), &item.extra.as_ref().unwrap());
                }
            }
            None => {
                debug!("  not found");
                reply.error(ENOENT)
            }
        }
    }

    fn setattr(
        &mut self,
        req: &Request,
        inode: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        debug!("setattr() called with {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}", inode, mode, uid, gid, size, atime, mtime, fh);

        let item = match self.tree_fs.get_item_mut(inode) {
            Some(item) => item,
            None => {
                reply.error(ENOENT);
                return;
            }
        };
        let mut attr = item.extra.as_mut().unwrap();

        if let Some(mode) = mode {
            debug!("chmod() called with {:?}, {:o}", inode, mode);

            if req.uid() != 0 && req.uid() != attr.uid {
                reply.error(libc::EPERM);
                return;
            }
            if req.uid() != 0
                && req.gid() != attr.gid
                && !get_groups(req.pid()).contains(&attr.gid)
            {
                // If SGID is set and the file belongs to a group that the caller is not part of
                // then the SGID bit is suppose to be cleared during chmod
                attr.perm = (mode & !libc::S_ISGID as u32) as u16;
            } else {
                attr.perm = mode as u16;
            }
            attr.ctime = SystemTime::now();
            reply.attr(&Duration::new(0, 0), &attr);
            return;
        }

        if uid.is_some() || gid.is_some() {
            debug!("chown() called with {:?} {:?} {:?}", inode, uid, gid);

            if let Some(gid) = gid {
                // Non-root users can only change gid to a group they're in
                if req.uid() != 0 && !get_groups(req.pid()).contains(&gid) {
                    reply.error(libc::EPERM);
                    return;
                }
            }
            if let Some(uid) = uid {
                if req.uid() != 0
                    // but no-op changes by the owner are not an error
                    && !(uid == attr.uid && req.uid() == attr.uid)
                {
                    reply.error(libc::EPERM);
                    return;
                }
            }
            // Only owner may change the group
            if gid.is_some() && req.uid() != 0 && req.uid() != attr.uid {
                reply.error(libc::EPERM);
                return;
            }

            if attr.perm & (libc::S_IXUSR | libc::S_IXGRP | libc::S_IXOTH) as u16 != 0 {
                // SUID & SGID are suppose to be cleared when chown'ing an executable file
                clear_suid_sgid(&mut attr);
            }

            if let Some(uid) = uid {
                attr.uid = uid;
                // Clear SETUID on owner change
                attr.perm &= !libc::S_ISUID as u16;
            }
            if let Some(gid) = gid {
                attr.gid = gid;
                // Clear SETGID unless user is root
                if req.uid() != 0 {
                    attr.perm &= !libc::S_ISGID as u16;
                }
            }
            attr.ctime = SystemTime::now();
            reply.attr(&Duration::new(0, 0), &attr);
            return;
        }

        if let Some(size) = size {
            debug!("truncate() called with {:?} {:?}", inode, size);

            if size == 0 {
                item.data.as_mut().unwrap().clear();
            } else {
                let old_data = item.data.take().unwrap();
                let old_data_vec = old_data.into_vec();

                let mut new_data = ByteBuffer::new();
                let _ = new_data.write(&old_data_vec[..(size as usize)]);
                item.data = Some(new_data);

                attr.size = size;
                attr.ctime = SystemTime::now();
                attr.mtime = SystemTime::now();

                // Clear SETUID & SETGID on truncate
                clear_suid_sgid(&mut attr);
            }
        }

        if let Some(atime) = atime {
            debug!("utimens() called with {:?}, atime={:?}", inode, atime);

            if attr.uid != req.uid() && req.uid() != 0 && atime != Now {
                reply.error(libc::EPERM);
                return;
            }

            if attr.uid != req.uid()
                && !check_access(
                attr.uid,
                attr.gid,
                attr.perm,
                req.uid(),
                req.gid(),
                libc::W_OK,
            ) {
                reply.error(libc::EACCES);
                return;
            }

            attr.atime = match atime {
                TimeOrNow::SpecificTime(time) => time,
                Now => SystemTime::now(),
            };
            attr.ctime = SystemTime::now();
        }
        if let Some(mtime) = mtime {
            debug!("utimens() called with {:?}, mtime={:?}", inode, mtime);

            if attr.uid != req.uid() && req.uid() != 0 && mtime != Now {
                reply.error(libc::EPERM);
                return;
            }

            if attr.uid != req.uid()
                && !check_access(
                attr.uid,
                attr.gid,
                attr.perm,
                req.uid(),
                req.gid(),
                libc::W_OK,
            ) {
                reply.error(libc::EACCES);
                return;
            }

            attr.mtime = match mtime {
                TimeOrNow::SpecificTime(time) => time,
                Now => SystemTime::now(),
            };
            attr.ctime = SystemTime::now();
        }

        reply.attr(&Duration::new(0, 0), &attr);
        return;
    }

    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        debug!("mknod() called with {:?} {:?} {:o}", parent, name, mode);

        let file_type = mode & libc::S_IFMT as u32;

        if file_type != libc::S_IFREG as u32
            // && file_type != libc::S_IFLNK as u32
            && file_type != libc::S_IFDIR as u32
        {
            // TODO
            warn!("mknod() implementation is incomplete. Only supports regular files and directories. Got {:o}", mode);
            reply.error(libc::ENOSYS);
            return;
        }

        match self.create_nod(parent, mode, req, name) {
            Ok(attr) => {
                // TODO: implement flags
                reply.entry(&Duration::new(0, 0), &attr, 0);
            }
            Err(err) => reply.error(err)
        }
    }
    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mut mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        debug!("mkdir() called with {:?} {:?} {:o}", parent, name, mode);

        let parent_o = self.tree_fs.get_item_mut(parent);
        if parent_o
            .map_or(None,
                    |parent_item| parent_item.find_child_mut(name.to_str().unwrap()))
            .is_some() {
            reply.error(libc::EEXIST);
            return;
        }

        let parent = self.tree_fs.get_item_mut(parent).unwrap();
        let parent_attr = parent.extra.as_ref().unwrap();
        if !check_access(
            parent_attr.uid,
            parent_attr.gid,
            parent_attr.perm,
            req.uid(),
            req.gid(),
            libc::W_OK,
        ) {
            reply.error(libc::EACCES);
            return;
        }

        let ino = self.allocate_next_inode();
        let mut attr = dir_attr(ino);
        self.tree_fs.push(&parent, Item::new(ino, name.to_str().unwrap().to_string(), true, Some(attr)));
        let parent_attr = parent.extra.as_mut().unwrap();

        parent_attr.mtime = SystemTime::now();
        parent_attr.ctime = SystemTime::now();

        attr.size = BLOCK_SIZE;
        attr.atime = SystemTime::now();
        attr.mtime = SystemTime::now();
        attr.ctime = SystemTime::now();

        if req.uid() != 0 {
            mode &= !(libc::S_ISUID | libc::S_ISGID) as u32;
        }
        if parent_attr.perm & libc::S_ISGID as u16 != 0 {
            mode |= libc::S_ISGID as u32;
        }
        attr.perm = self.creation_mode(mode);

        attr.uid = req.uid();
        attr.gid = creation_gid(&parent_attr, req.gid());

        reply.entry(&Duration::new(0, 0), &attr, 0);
    }

    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        debug!("rename() called with {:?} {:?} {:?} {:?}", parent, name, new_parent, new_name);

        if parent != new_parent {
            reply.error(ENOSYS);
            return;
        }

        let parent = match self.tree_fs.get_item_mut(parent) {
            Some(parent) => parent,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        if parent.find_child_mut(new_name.to_str().unwrap()).is_some() {
            reply.error(libc::EEXIST);
            return;
        }

        let child = match parent.find_child_mut(name.to_str().unwrap()) {
            Some(child) => child,
            None => {
                reply.error(ENOENT);
                return;
            }
        };

        child.name = new_name.to_str().unwrap().to_string();

        let parent_attr = parent.extra.as_mut().unwrap();
        parent_attr.ctime = SystemTime::now();
        parent_attr.mtime = SystemTime::now();

        let attr = child.extra.as_mut().unwrap();
        attr.ctime = SystemTime::now();
        attr.mtime = SystemTime::now();

        reply.ok();
    }

    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        debug!("unlink() called with {:?} {:?}", parent, name);

        match self.tree_fs.get_item_mut(parent) {
            Some(parent) => {
                if !parent.is_dir {
                    reply.error(ENOENT);
                    return;
                }

                let child = parent.find_child_mut(name.to_str().unwrap());
                match child {
                    Some(child) => {
                        let parent_attr = parent.extra.as_mut().unwrap();
                        let attr = child.extra.as_mut().unwrap();

                        let uid = req.uid();
                        // "Sticky bit" handling
                        if parent_attr.perm & libc::S_ISVTX as u16 != 0
                            && uid != 0
                            && uid != parent_attr.uid
                            && uid != attr.uid
                        {
                            reply.error(libc::EACCES);
                            return;
                        }

                        parent_attr.ctime = SystemTime::now();
                        parent_attr.mtime = SystemTime::now();

                        self.tree_fs.remove_child(parent, child);

                        reply.ok();
                    }
                    None => reply.error(ENOENT)
                }
            }
            _ => reply.error(ENOENT)
        }
    }

    fn rmdir(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        debug!("rmdir() called with {:?} {:?}", parent, name);

        match self.tree_fs.get_item_mut(parent) {
            Some(parent) => {
                let parent_attr = parent.extra.as_ref().unwrap();
                if !check_access(
                    parent_attr.uid,
                    parent_attr.gid,
                    parent_attr.perm,
                    req.uid(),
                    req.gid(),
                    libc::W_OK,
                ) {
                    reply.error(libc::EACCES);
                    return;
                }

                match parent.find_child_mut(name.to_str().unwrap()) {
                    Some(child) => {
                        if !child.is_dir {
                            reply.error(libc::EACCES);
                            return;
                        }
                        if child.children().len() != 0 {
                            reply.error(libc::ENOTEMPTY);
                            return;
                        }

                        let parent_attr = parent.extra.as_mut().unwrap();
                        let attrs = child.extra.as_mut().unwrap();

                        // "Sticky bit" handling
                        if parent_attr.perm & libc::S_ISVTX as u16 != 0
                            && req.uid() != 0
                            && req.uid() != parent_attr.uid
                            && req.uid() != attrs.uid
                        {
                            reply.error(libc::EACCES);
                            return;
                        }

                        parent_attr.ctime = SystemTime::now();
                        parent_attr.mtime = SystemTime::now();

                        self.tree_fs.remove_child(parent, child);

                        reply.ok();
                    }
                    None => reply.error(ENOENT)
                }
            }
            None => reply.error(ENOENT)
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        debug!("read {} {} {}", ino, offset, size);

        match self.tree_fs.get_item_mut(ino) {
            Some(item) => {
                if item.is_dir {
                    reply.error(ENOENT);
                    return;
                }

                let read_size = min(size, item.data.as_ref().unwrap().len() as u32);
                debug!("  read_size={}", read_size);
                let mut buffer = vec![0; read_size as usize];
                item.data.as_mut().unwrap().set_rpos(offset as usize);
                let read_len = item.data.as_mut().unwrap().read(&mut buffer).unwrap();
                debug!("  read_len={}", read_len);

                reply.data(&buffer);
            }
            None => reply.error(ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request,
        inode: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        #[allow(unused_variables)] flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        debug!("write() called with {:?} size={:?}", inode, data.len());

        assert!(offset >= 0);

        match self.tree_fs.get_item_mut(inode) {
            Some(item) => {
                if item.is_dir {
                    reply.error(ENOENT);
                    return;
                }

                item.data.as_mut().unwrap().set_wpos(offset as usize);
                let _ = item.data.as_mut().unwrap().write(data);

                item.extra.as_mut().unwrap().size = item.data.as_mut().unwrap().len() as u64;

                reply.written(data.len() as u32);
            }
            _ => reply.error(ENOENT)
        }
    }

    fn flush(&mut self, _req: &Request<'_>, ino: u64, fh: u64, lock_owner: u64, reply: ReplyEmpty) {
        debug!("flush() called with {:?} {:?} {:?}", ino, fh, lock_owner);

        reply.ok();
    }

    fn release(&mut self, _req: &Request<'_>, _ino: u64, _fh: u64, _flags: i32, _lock_owner: Option<u64>, _flush: bool, reply: ReplyEmpty) {
        debug!("release() called with {:?} {:?} {:?}", _ino, _fh, _lock_owner);

        reply.ok();
    }

    fn opendir(&mut self, req: &Request, inode: u64, flags: i32, reply: ReplyOpen) {
        debug!("opendir() called on {:?}", inode);

        let (access_mask, _read, _write) = match flags & libc::O_ACCMODE {
            libc::O_RDONLY => {
                // Behavior is undefined, but most filesystems return EACCES
                if flags & libc::O_TRUNC != 0 {
                    reply.error(libc::EACCES);
                    return;
                }
                (libc::R_OK, true, false)
            }
            libc::O_WRONLY => (libc::W_OK, false, true),
            libc::O_RDWR => (libc::R_OK | libc::W_OK, true, true),
            // Exactly one access mode flag must be specified
            _ => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        match self.tree_fs.get_item_mut(inode) {
            Some(item) => {
                let attr = item.extra.as_ref().unwrap();
                if check_access(
                    attr.uid,
                    attr.gid,
                    attr.perm,
                    req.uid(),
                    req.gid(),
                    access_mask,
                ) {
                    let open_flags = if self.direct_io { FOPEN_DIRECT_IO } else { 0 };
                    reply.opened(self.allocate_next_file_handle(), open_flags);
                }
            }
            None => reply.error(ENOENT)
        }
    }


    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir {} {} {}", ino, _fh, offset);

        match self.tree_fs.get_item_mut(ino) {
            Some(item) => {
                if !item.is_dir {
                    reply.error(ENOENT);
                    return;
                }
                let mut entries = vec![
                    (item.ino, FileType::Directory, "."),
                ];
                // root doesn't have parent
                if item.get_parent().is_some() {
                    entries.push((item.get_parent().unwrap().ino, FileType::Directory, ".."));
                }
                for item in item.children() {
                    entries.push((item.ino, if item.is_dir { FileType::Directory } else { FileType::RegularFile }, &item.name));
                }

                for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
                    if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                        break;
                    }
                }

                reply.ok();
            }
            None => reply.error(ENOENT),
        }
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        inode: u64,
        _fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        debug!("releasedir() called with {:?} {:?}", inode, _fh);

        match self.tree_fs.get_item_mut(inode) {
            Some(_) => reply.ok(),
            None => reply.error(ENOENT)
        }
    }

    fn access(&mut self, req: &Request, inode: u64, mask: i32, reply: ReplyEmpty) {
        debug!("access() called with {:?} {:?}", inode, mask);

        match self.tree_fs.get_item_mut(inode) {
            Some(item) => {
                let attr = item.extra.as_ref().unwrap();
                if check_access(attr.uid, attr.gid, attr.perm, req.uid(), req.gid(), mask) {
                    reply.ok();
                } else {
                    reply.error(libc::EACCES);
                }
            }
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, req: &Request, inode: u64, flags: i32, reply: ReplyOpen) {
        debug!("open() called for {:?}", inode);

        let (access_mask, _read, _write) = match flags & libc::O_ACCMODE {
            libc::O_RDONLY => {
                // Behavior is undefined, but most filesystems return EACCES
                if flags & libc::O_TRUNC != 0 {
                    reply.error(libc::EACCES);
                    return;
                }
                if flags & FMODE_EXEC != 0 {
                    // Open is from internal exec syscall
                    (libc::X_OK, true, false)
                } else {
                    (libc::R_OK, true, false)
                }
            }
            libc::O_WRONLY => (libc::W_OK, false, true),
            libc::O_RDWR => (libc::R_OK | libc::W_OK, true, true),
            // Exactly one access mode flag must be specified
            _ => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        match self.tree_fs.get_item_mut(inode) {
            Some(item) => {
                let attr = item.extra.as_ref().unwrap();
                if check_access(attr.uid, attr.gid, attr.perm, req.uid(), req.gid(), access_mask) {
                    let open_flags = if self.direct_io { FOPEN_DIRECT_IO } else { 0 };
                    reply.opened(self.allocate_next_file_handle(), open_flags);
                } else {
                    reply.error(libc::EACCES);
                }
            }
            None => reply.error(ENOENT)
        }
    }

    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        debug!("create() called with {:?} {:?}", parent, name);

        let (_read, _write) = match flags & libc::O_ACCMODE {
            libc::O_RDONLY => (true, false),
            libc::O_WRONLY => (false, true),
            libc::O_RDWR => (true, true),
            // Exactly one access mode flag must be specified
            _ => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        match self.create_nod(parent, mode, req, name) {
            Ok(attr) => {
                // TODO: implement flags
                reply.created(
                    &Duration::new(0, 0),
                    &attr,
                    0,
                    self.allocate_next_file_handle(),
                    0,
                );
            }
            Err(err) => reply.error(err)
        }
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        src_inode: u64,
        src_fh: u64,
        src_offset: i64,
        dest_inode: u64,
        dest_fh: u64,
        dest_offset: i64,
        size: u64,
        _flags: u32,
        reply: ReplyWrite,
    ) {
        debug!(
            "copy_file_range() called with src ({}, {}, {}) dest ({}, {}, {}) size={}",
            src_fh, src_inode, src_offset, dest_fh, dest_inode, dest_offset, size
        );

        match self.tree_fs.get_item_mut(src_inode) {
            Some(src) => {
                match self.tree_fs.get_item_mut(dest_inode) {
                    Some(dest) => {
                        let file_size = src.extra.as_ref().unwrap().size;
                        // Could underflow if file length is less than local_start
                        let read_size = min(size, file_size.saturating_sub(src_offset as u64));

                        let mut data = vec![0; read_size as usize];
                        src.data.as_mut().unwrap().set_rpos(src_offset as usize);
                        src.data.as_mut().unwrap().read(&mut data).unwrap();

                        dest.data.as_mut().unwrap().set_wpos(dest_offset as usize);
                        dest.data.as_mut().unwrap().write(&data).unwrap();

                        let attr = dest.extra.as_mut().unwrap();
                        attr.ctime = SystemTime::now();
                        attr.mtime = SystemTime::now();

                        reply.written(data.len() as u32);
                    }
                    None => reply.error(ENOENT)
                }
            }
            None => reply.error(ENOENT)
        }
    }
}

fn dir_attr(ino: u64) -> FileAttr {
    let mut f = FileAttr {
        ino,
        size: BLOCK_SIZE,
        blocks: 0,
        atime: SystemTime::now(),
        mtime: SystemTime::now(),
        ctime: SystemTime::now(),
        crtime: UNIX_EPOCH,
        kind: FileType::Directory,
        perm: 0o777,
        nlink: 2,
        uid: 0,
        gid: 0,
        rdev: 0,
        flags: 0,
        blksize: BLOCK_SIZE as u32,
    };
    f.blocks = (f.size + BLOCK_SIZE - 1) / BLOCK_SIZE;

    f
}

fn file_attr(ino: u64, size: u64) -> FileAttr {
    let mut f = FileAttr {
        ino,
        size,
        blocks: (size + BLOCK_SIZE - 1) / BLOCK_SIZE,
        atime: SystemTime::now(),
        mtime: SystemTime::now(),
        ctime: SystemTime::now(),
        crtime: UNIX_EPOCH,
        kind: FileType::RegularFile,
        perm: 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        flags: 0,
        blksize: 512,
    };
    f.blocks = (f.size + BLOCK_SIZE - 1) / BLOCK_SIZE;

    f
}

fn creation_gid(parent: &FileAttr, gid: u32) -> u32 {
    if parent.perm & libc::S_ISGID as u16 != 0 {
        return parent.gid;
    }

    gid
}

pub fn check_access(
    file_uid: u32,
    file_gid: u32,
    file_mode: u16,
    uid: u32,
    gid: u32,
    mut access_mask: i32,
) -> bool {
    // F_OK tests for existence of file
    if access_mask == libc::F_OK {
        return true;
    }
    let file_mode = i32::from(file_mode);

    // root is allowed to read & write anything
    if uid == 0 {
        // root only allowed to exec if one of the X bits is set
        access_mask &= libc::X_OK;
        access_mask -= access_mask & (file_mode >> 6);
        access_mask -= access_mask & (file_mode >> 3);
        access_mask -= access_mask & file_mode;
        return access_mask == 0;
    }

    if uid == file_uid {
        access_mask -= access_mask & (file_mode >> 6);
    } else if gid == file_gid {
        access_mask -= access_mask & (file_mode >> 3);
    } else {
        access_mask -= access_mask & file_mode;
    }

    return access_mask == 0;
}

fn get_groups(pid: u32) -> Vec<u32> {
    #[cfg(not(target_os = "macos"))]
    {
        let path = format!("/proc/{pid}/task/{pid}/status");
        let file = File::open(path).unwrap();
        for line in BufReader::new(file).lines() {
            let line = line.unwrap();
            if line.starts_with("Groups:") {
                return line["Groups: ".len()..]
                    .split(' ')
                    .filter(|x| !x.trim().is_empty())
                    .map(|x| x.parse::<u32>().unwrap())
                    .collect();
            }
        }
    }

    vec![]
}

fn as_file_kind(mut mode: u32) -> FileType {
    mode &= libc::S_IFMT as u32;

    if mode == libc::S_IFREG as u32 {
        return FileType::RegularFile;
    } else if mode == libc::S_IFLNK as u32 {
        return FileType::Symlink;
    } else if mode == libc::S_IFDIR as u32 {
        return FileType::Directory;
    } else {
        unimplemented!("{}", mode);
    }
}

fn clear_suid_sgid(attr: &mut FileAttr) {
    attr.perm &= !libc::S_ISUID as u16;
    // SGID is only suppose to be cleared if XGRP is set
    if attr.perm & libc::S_IXGRP as u16 != 0 {
        attr.perm &= !libc::S_ISGID as u16;
    }
}

// fn generate_sample_tree<'a>() -> TreeFs<FileAttr> {
//     let mut fs = TreeFs::new();
//
//     let root = Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("root"), true, Some(dir_attr(INO_GEN.load(Ordering::SeqCst))));
//     let root = fs.set_root(root);
//
//     let dir1 = fs.push(root, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("1"), true, Some(dir_attr(INO_GEN.load(Ordering::SeqCst)))));
//     fs.push(dir1, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("1.1"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//     fs.push(dir1, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("1.2"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//     fs.push(dir1, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("1.3"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//
//     let dir2 = fs.push(root, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("2"), true, Some(dir_attr(6))));
//     fs.push(dir2, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("2.1"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//     fs.push(dir2, Item::new(INO_GEN.fetch_add(1, Ordering::SeqCst), String::from("2.2"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//
//     fs.push(root, Item::new(9, String::from("3"), false, Some(file_attr(INO_GEN.load(Ordering::SeqCst), 4))));
//
//     fs
// }
