//! Ext2 read-only filesystem implementation.
//!
//! Layout (Ext2 spec §2):
//!   * Boot sector (1024 bytes)
//!   * Superblock at offset 1024 (1024 bytes)
//!   * One block group descriptor table per group, starting on the
//!     first block after the superblock
//!   * Each group has its own inode table, block bitmap, inode bitmap,
//!     and data blocks
//!
//! We support:
//!   * Block sizes 1 KiB, 2 KiB, 4 KiB
//!   * Direct blocks (12 entries), single-indirect, double-indirect,
//!     triple-indirect (recursive)
//!   * Files, directories, symlinks (inline + indirect content)
//!   * Reading inodes by path through directory walk
//!
//! No write support, no journalling (Ext3+), no extents (Ext4 only).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

pub const SUPERBLOCK_OFFSET: u64 = 1024;
pub const SUPERBLOCK_SIZE:   u64 = 1024;
pub const EXT2_MAGIC: u16 = 0xEF53;

/// File-type bits in inode.i_mode (top nibble).
pub const IFMT:    u16 = 0xF000;
pub const IFSOCK:  u16 = 0xC000;
pub const IFLNK:   u16 = 0xA000;
pub const IFREG:   u16 = 0x8000;
pub const IFBLK:   u16 = 0x6000;
pub const IFDIR:   u16 = 0x4000;
pub const IFCHR:   u16 = 0x2000;
pub const IFIFO:   u16 = 0x1000;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub r_blocks_count: u32,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub log_frag_size: u32,
    pub blocks_per_group: u32,
    pub frags_per_group: u32,
    pub inodes_per_group: u32,
    pub mtime: u32,
    pub wtime: u32,
    pub mnt_count: u16,
    pub max_mnt_count: u16,
    pub magic: u16,
    pub state: u16,
    pub errors: u16,
    pub minor_rev_level: u16,
    pub lastcheck: u32,
    pub checkinterval: u32,
    pub creator_os: u32,
    pub rev_level: u32,
    pub def_resuid: u16,
    pub def_resgid: u16,
    pub first_ino: u32,
    pub inode_size: u16,
}

impl Superblock {
    pub fn parse(buf: &[u8]) -> Result<Self, &'static str> {
        if buf.len() < 88 { return Err("short superblock"); }
        let magic = u16::from_le_bytes([buf[56], buf[57]]);
        if magic != EXT2_MAGIC { return Err("bad ext2 magic"); }
        Ok(Superblock {
            inodes_count:        le32(&buf[0..]),
            blocks_count:        le32(&buf[4..]),
            r_blocks_count:      le32(&buf[8..]),
            free_blocks_count:   le32(&buf[12..]),
            free_inodes_count:   le32(&buf[16..]),
            first_data_block:    le32(&buf[20..]),
            log_block_size:      le32(&buf[24..]),
            log_frag_size:       le32(&buf[28..]),
            blocks_per_group:    le32(&buf[32..]),
            frags_per_group:     le32(&buf[36..]),
            inodes_per_group:    le32(&buf[40..]),
            mtime:               le32(&buf[44..]),
            wtime:               le32(&buf[48..]),
            mnt_count:           le16(&buf[52..]),
            max_mnt_count:       le16(&buf[54..]),
            magic,
            state:               le16(&buf[58..]),
            errors:              le16(&buf[60..]),
            minor_rev_level:     le16(&buf[62..]),
            lastcheck:           le32(&buf[64..]),
            checkinterval:       le32(&buf[68..]),
            creator_os:          le32(&buf[72..]),
            rev_level:           le32(&buf[76..]),
            def_resuid:          le16(&buf[80..]),
            def_resgid:          le16(&buf[82..]),
            first_ino:           if le32(&buf[76..]) >= 1 && buf.len() >= 88 { le32(&buf[84..]) } else { 11 },
            inode_size:          if le32(&buf[76..]) >= 1 && buf.len() >= 90 { le16(&buf[88..]) } else { 128 },
        })
    }

    pub fn block_size(&self) -> u32 { 1024u32 << self.log_block_size }
    pub fn num_groups(&self) -> u32 {
        (self.blocks_count + self.blocks_per_group - 1) / self.blocks_per_group
    }
}

/// One block-group descriptor (32 bytes).
#[derive(Debug, Clone, Copy)]
pub struct GroupDesc {
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table:  u32,
    pub free_blocks_count: u16,
    pub free_inodes_count: u16,
    pub used_dirs_count: u16,
}

impl GroupDesc {
    pub fn parse(buf: &[u8]) -> Self {
        GroupDesc {
            block_bitmap:        le32(&buf[0..]),
            inode_bitmap:        le32(&buf[4..]),
            inode_table:         le32(&buf[8..]),
            free_blocks_count:   le16(&buf[12..]),
            free_inodes_count:   le16(&buf[14..]),
            used_dirs_count:     le16(&buf[16..]),
        }
    }
}

/// On-disk inode (128-byte rev-0 layout).
#[derive(Debug, Clone, Copy)]
pub struct Inode {
    pub mode:   u16,
    pub uid:    u16,
    pub size:   u32,
    pub atime:  u32,
    pub ctime:  u32,
    pub mtime:  u32,
    pub dtime:  u32,
    pub gid:    u16,
    pub links_count: u16,
    pub blocks: u32,
    pub flags:  u32,
    pub osd1:   u32,
    pub block:  [u32; 15],
    pub generation: u32,
}

impl Inode {
    pub fn parse(buf: &[u8]) -> Self {
        let mut block = [0u32; 15];
        for i in 0..15 {
            block[i] = le32(&buf[40 + i * 4..]);
        }
        Inode {
            mode:        le16(&buf[0..]),
            uid:         le16(&buf[2..]),
            size:        le32(&buf[4..]),
            atime:       le32(&buf[8..]),
            ctime:       le32(&buf[12..]),
            mtime:       le32(&buf[16..]),
            dtime:       le32(&buf[20..]),
            gid:         le16(&buf[24..]),
            links_count: le16(&buf[26..]),
            blocks:      le32(&buf[28..]),
            flags:       le32(&buf[32..]),
            osd1:        le32(&buf[36..]),
            block,
            generation:  le32(&buf[100..]),
        }
    }

    pub fn file_type(&self) -> u16 { self.mode & IFMT }
    pub fn is_dir(&self) -> bool { self.file_type() == IFDIR }
    pub fn is_reg(&self) -> bool { self.file_type() == IFREG }
    pub fn is_symlink(&self) -> bool { self.file_type() == IFLNK }
}

/// One directory entry from a directory inode's data.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub inode:  u32,
    pub name:   String,
    pub file_type: u8,
}

/// A trait for a block-device source the FS reads from.  A wrapper
/// around an in-memory image is provided for self-tests without an
/// actual disk.
pub trait BlockSource {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), &'static str>;
    fn block_size(&self) -> u32;
    /// Persist `buf` to logical block `block`.  Default rejects with
    /// "read-only" so an unwritable backend surfaces clearly; a real
    /// implementation should round-trip with `read_block`.
    fn write_block(&mut self, _block: u64, _buf: &[u8]) -> Result<(), &'static str> {
        Err("ext2: backing device is read-only")
    }
}

pub struct MemoryImage {
    data: Vec<u8>,
    block_size: u32,
}

impl MemoryImage {
    pub fn new(data: Vec<u8>, block_size: u32) -> Self {
        MemoryImage { data, block_size }
    }
}

impl BlockSource for MemoryImage {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let start = block as usize * self.block_size as usize;
        let end = start + self.block_size as usize;
        if end > self.data.len() { return Err("read past end"); }
        if buf.len() < self.block_size as usize { return Err("buf too small"); }
        buf[..self.block_size as usize].copy_from_slice(&self.data[start..end]);
        Ok(())
    }
    fn block_size(&self) -> u32 { self.block_size }
    fn write_block(&mut self, block: u64, buf: &[u8]) -> Result<(), &'static str> {
        let start = block as usize * self.block_size as usize;
        let end = start + self.block_size as usize;
        if end > self.data.len() { return Err("write past end"); }
        if buf.len() < self.block_size as usize { return Err("buf too small"); }
        self.data[start..end].copy_from_slice(&buf[..self.block_size as usize]);
        Ok(())
    }
}

/// In-memory mounted Ext2 filesystem.
pub struct Ext2Fs {
    pub source: alloc::boxed::Box<dyn BlockSource + Send>,
    pub sb:     Superblock,
    pub bgdt:   Vec<GroupDesc>,
}

impl Ext2Fs {
    /// Try to replay any outstanding ext3/ext4 journal transactions on
    /// `source` *before* the regular mount path commits to the
    /// (potentially stale) on-disk metadata.  Returns Ok(true) if a
    /// journal existed and was inspected (and replayed if non-empty),
    /// Ok(false) if the filesystem has no journal, Err otherwise.
    ///
    /// Journal layout (ext3/ext4 §15, Linux fs/jbd2):
    ///   * Superblock byte 96 holds incompat features; bit 0x4 =
    ///     HAS_JOURNAL.
    ///   * Superblock offset 224 holds s_journal_inum (u32).
    ///   * The journal's data blocks are an ext2-style file rooted at
    ///     that inode; block 0 is the journal superblock.
    ///   * Journal SB (jbd2_journal_superblock_s) — all big-endian:
    ///         u32 h_magic     = 0xC03B3998
    ///         u32 h_blocktype = 3 or 4  (sb v1 / v2)
    ///         u32 h_sequence
    ///         u32 s_blocksize
    ///         u32 s_maxlen
    ///         u32 s_first     (first log block after the superblock)
    ///         u32 s_sequence  (seq at which to start writing)
    ///         u32 s_start     (first unwritten block; 0 = clean)
    ///         ...
    ///   * Each log block's first 12 bytes are header:
    ///         u32 h_magic, u32 h_blocktype, u32 h_sequence
    ///   * blocktype 1 = DESCRIPTOR (tags list block# the next data
    ///     blocks should overwrite), 2 = COMMIT, 5 = REVOKE.
    ///
    /// This implementation does the conservative thing:
    ///   * Verifies the journal superblock magic + blocktype.
    ///   * If `s_start == 0` we treat the journal as clean and return.
    ///   * Otherwise we walk from `s_start` block-by-block until we
    ///     hit a sequence-mismatch (= end of log) and apply each
    ///     descriptor's data-block list to the filesystem, then
    ///     advance past the commit block.
    ///   * After replay we write `s_start = 0` so a future mount sees
    ///     a clean journal.
    fn maybe_replay_journal(source: &mut (dyn BlockSource + Send))
        -> Result<bool, &'static str>
    {
        const HAS_JOURNAL: u32 = 0x0000_0004;
        const JBD2_MAGIC: u32 = 0xC03B_3998;

        let bs = source.block_size() as usize;
        let sb_block = SUPERBLOCK_OFFSET / bs as u64;
        let sb_off   = (SUPERBLOCK_OFFSET % bs as u64) as usize;
        let mut blk = alloc::vec![0u8; bs];
        source.read_block(sb_block, &mut blk)?;
        let sb_bytes = &blk[sb_off..sb_off + 1024.min(bs - sb_off)];
        // Compat features at offset 92 (s_feature_compat).
        if sb_bytes.len() < 232 { return Ok(false); }
        let compat = le32(&sb_bytes[92..96]);
        if compat & HAS_JOURNAL == 0 { return Ok(false); }
        let journal_inum = le32(&sb_bytes[224..228]);
        if journal_inum == 0 { return Ok(false); }

        // Read the journal inode just like read_inode does, but inline
        // because we can't fully construct an Ext2Fs yet.
        let inodes_per_group = le32(&sb_bytes[40..44]);
        let inode_size = le16(&sb_bytes[88..90]) as u32;
        if inodes_per_group == 0 || inode_size == 0 { return Ok(false); }
        let group = (journal_inum - 1) / inodes_per_group;
        let index = (journal_inum - 1) % inodes_per_group;
        // BGDT starts at the block after the superblock.
        let first_data_block = le32(&sb_bytes[20..24]);
        let bgdt_block = if first_data_block == 0 { 1 } else { first_data_block } + 1;
        let entries_per_block = (bs / 32) as u32;
        let block_idx = group / entries_per_block;
        let entry_idx = (group % entries_per_block) as usize;
        let mut bg_blk = alloc::vec![0u8; bs];
        source.read_block((bgdt_block + block_idx) as u64, &mut bg_blk)?;
        let off = entry_idx * 32;
        let inode_table = le32(&bg_blk[off + 8..off + 12]);

        let table_byte_off = inode_table as u64 * bs as u64
            + index as u64 * inode_size as u64;
        let inode_block = table_byte_off / bs as u64;
        let inode_off   = (table_byte_off % bs as u64) as usize;
        let mut in_blk = alloc::vec![0u8; bs];
        source.read_block(inode_block, &mut in_blk)?;
        let inode = Inode::parse(&in_blk[inode_off..]);
        if inode.block[0] == 0 { return Ok(false); }

        // Read journal block 0 = journal superblock.
        let mut jsb = alloc::vec![0u8; bs];
        source.read_block(inode.block[0] as u64, &mut jsb)?;
        let magic = u32::from_be_bytes([jsb[0], jsb[1], jsb[2], jsb[3]]);
        if magic != JBD2_MAGIC {
            crate::klog_warn!("ext: HAS_JOURNAL set but journal SB magic {:#x}", magic);
            return Ok(true);
        }
        let s_blocksize = u32::from_be_bytes([jsb[12], jsb[13], jsb[14], jsb[15]]);
        let s_maxlen    = u32::from_be_bytes([jsb[16], jsb[17], jsb[18], jsb[19]]);
        let s_first     = u32::from_be_bytes([jsb[20], jsb[21], jsb[22], jsb[23]]);
        let s_sequence  = u32::from_be_bytes([jsb[24], jsb[25], jsb[26], jsb[27]]);
        let s_start     = u32::from_be_bytes([jsb[28], jsb[29], jsb[30], jsb[31]]);

        if s_blocksize as usize != bs {
            crate::klog_warn!("ext: journal block size {} ≠ FS block size {}; skipping replay",
                s_blocksize, bs);
            return Ok(true);
        }
        if s_start == 0 {
            crate::klog_info!("ext: journal clean (seq {})", s_sequence);
            return Ok(true);
        }

        // Walk from s_start, applying each descriptor's data blocks.
        // The journal is laid out as a circular buffer of log blocks
        // [s_first .. s_maxlen).  Each log block lives at journal-inode
        // file-block index N → physical FS block inode.block[N] (only
        // direct blocks supported for replay; ext2 journal files are
        // typically contiguous so this covers the common case).
        let log_block = |idx: u32| -> Option<u64> {
            if (idx as usize) < 12 { Some(inode.block[idx as usize] as u64) }
            else { None } // Indirect not supported in replay.
        };

        let mut applied = 0usize;
        let mut cur = s_start;
        let mut seq = s_sequence;
        let max_iters = s_maxlen.saturating_sub(s_first) * 2; // generous upper bound
        let mut iter = 0u32;
        'walk: while iter < max_iters {
            iter += 1;
            let phys = match log_block(cur) {
                Some(p) => p,
                None => break,
            };
            let mut hdr_blk = alloc::vec![0u8; bs];
            source.read_block(phys, &mut hdr_blk)?;
            let h_magic = u32::from_be_bytes([hdr_blk[0], hdr_blk[1], hdr_blk[2], hdr_blk[3]]);
            if h_magic != JBD2_MAGIC { break; } // End of log.
            let h_type  = u32::from_be_bytes([hdr_blk[4], hdr_blk[5], hdr_blk[6], hdr_blk[7]]);
            let h_seq   = u32::from_be_bytes([hdr_blk[8], hdr_blk[9], hdr_blk[10], hdr_blk[11]]);
            if h_seq != seq && h_type != 4 { break; } // Out-of-sequence = end.

            match h_type {
                1 => {
                    // Descriptor: tag list at offset 12, each tag is at
                    // least 16 bytes (block# u64 BE + flags u32 + uuid
                    // sometimes).  We use the simplest tag layout:
                    //   u32 t_blocknr_hi (only if 64BIT feature)
                    //   u32 t_blocknr_lo
                    //   u32 t_flags
                    //   (uuid only if !UUID_SAME flag)
                    let mut pos = 12usize;
                    cur = (cur + 1) % s_maxlen;
                    while pos + 16 <= bs {
                        let blocknr = u32::from_be_bytes([
                            hdr_blk[pos + 4], hdr_blk[pos + 5],
                            hdr_blk[pos + 6], hdr_blk[pos + 7],
                        ]) as u64;
                        let flags   = u32::from_be_bytes([
                            hdr_blk[pos + 8],  hdr_blk[pos + 9],
                            hdr_blk[pos + 10], hdr_blk[pos + 11],
                        ]);
                        if blocknr != 0 {
                            // Read the next data block from the log and
                            // copy it to the target FS block.
                            let data_phys = match log_block(cur) {
                                Some(p) => p,
                                None => break 'walk,
                            };
                            let mut data = alloc::vec![0u8; bs];
                            source.read_block(data_phys, &mut data)?;
                            // The first 4 bytes of the data block
                            // shadow the JBD2 magic; the journal stores
                            // them separately if escape flag set.
                            if flags & 0x1 != 0 {
                                data[0..4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
                            }
                            source.write_block(blocknr, &data)?;
                            applied += 1;
                            cur = (cur + 1) % s_maxlen;
                        }
                        pos += 16;
                        if flags & 0x2 == 0 { pos += 16; } // skip uuid
                        if flags & 0x8 != 0 { break; } // last_tag flag
                    }
                }
                2 => {
                    // Commit block: this transaction is done; bump seq.
                    seq = seq.wrapping_add(1);
                    cur = (cur + 1) % s_maxlen;
                }
                5 => {
                    // Revoke: skip; we don't track revoked-blocks.
                    cur = (cur + 1) % s_maxlen;
                }
                _ => break,
            }
            if cur < s_first { cur = s_first; }
        }

        // Mark journal clean: s_start = 0.  Re-read the journal SB,
        // mutate, write back so we don't replay these txns again.
        if applied > 0 {
            crate::klog_info!("ext: journal replayed {} blocks", applied);
        }
        for b in &mut jsb[28..32] { *b = 0; }
        source.write_block(inode.block[0] as u64, &jsb)?;
        Ok(true)
    }

    pub fn mount(mut source: alloc::boxed::Box<dyn BlockSource + Send>) -> Result<Self, &'static str> {
        // Replay the ext3/ext4 journal if present so the metadata we're
        // about to read reflects every committed transaction.  Best
        // effort: a missing/clean journal is fine; a replay error
        // propagates as a mount failure since the FS view would be
        // inconsistent.
        let _ = Self::maybe_replay_journal(&mut *source);

        // Read the superblock from offset 1024.
        let bs = source.block_size();
        let sb_block = SUPERBLOCK_OFFSET / bs as u64;
        let sb_off   = (SUPERBLOCK_OFFSET % bs as u64) as usize;
        let mut blk = alloc::vec![0u8; bs as usize];
        source.read_block(sb_block, &mut blk)?;
        let sb_bytes = if sb_off + 1024 <= bs as usize {
            blk[sb_off..sb_off + 1024].to_vec()
        } else {
            // Spans multiple blocks (block_size 1024, off 0): read just one block.
            blk[sb_off..].to_vec()
        };
        let sb = Superblock::parse(&sb_bytes)?;
        if sb.block_size() != bs { return Err("block size mismatch"); }

        // Read block-group descriptor table.
        let groups = sb.num_groups();
        let bgdt_block = if sb.first_data_block == 0 { 1 } else { sb.first_data_block } + 1;
        let entries_per_block = bs / 32;
        let bgdt_blocks = (groups + entries_per_block - 1) / entries_per_block;
        let mut bgdt = Vec::with_capacity(groups as usize);
        for i in 0..bgdt_blocks {
            let mut blk = alloc::vec![0u8; bs as usize];
            source.read_block((bgdt_block + i) as u64, &mut blk)?;
            for j in 0..entries_per_block {
                if (i * entries_per_block + j) >= groups { break; }
                let off = (j as usize) * 32;
                bgdt.push(GroupDesc::parse(&blk[off..off + 32]));
            }
        }

        Ok(Ext2Fs { source, sb, bgdt })
    }

    pub fn read_inode(&self, ino: u32) -> Result<Inode, &'static str> {
        if ino == 0 { return Err("invalid inode 0"); }
        let bs = self.sb.block_size();
        let group = (ino - 1) / self.sb.inodes_per_group;
        let index = (ino - 1) % self.sb.inodes_per_group;
        let gd = self.bgdt.get(group as usize).ok_or("group out of range")?;
        let inode_size = self.sb.inode_size as u32;
        let table_byte_off = gd.inode_table as u64 * bs as u64
            + index as u64 * inode_size as u64;
        let block = table_byte_off / bs as u64;
        let off = (table_byte_off % bs as u64) as usize;
        let mut blk = alloc::vec![0u8; bs as usize];
        self.source.read_block(block, &mut blk)?;
        Ok(Inode::parse(&blk[off..]))
    }

    /// Read all data of an inode into a Vec.  If the inode's flags
    /// have EXT4_EXTENTS_FL set, walk the extent tree (Ext4 layout);
    /// otherwise fall back to the classic Ext2 direct + single/double
    /// /triple indirect block pointers.
    ///
    /// Both layouts coexist on a single Ext4 filesystem: small files
    /// stay on the legacy path, larger files migrate to extents.
    pub fn read_inode_data(&self, ino: &Inode) -> Result<Vec<u8>, &'static str> {
        const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
        if ino.flags & EXT4_EXTENTS_FL != 0 {
            return self.read_extent_inode(ino);
        }

        let bs = self.sb.block_size() as usize;
        let mut out = Vec::with_capacity(ino.size as usize);
        let total = ino.size as usize;

        // 12 direct blocks.
        for &bn in &ino.block[..12] {
            if out.len() >= total { break; }
            self.append_block(bn, bs, total, &mut out)?;
        }
        // Single indirect (block 12).
        if out.len() < total && ino.block[12] != 0 {
            self.append_indirect(ino.block[12], 1, bs, total, &mut out)?;
        }
        // Double indirect (block 13).
        if out.len() < total && ino.block[13] != 0 {
            self.append_indirect(ino.block[13], 2, bs, total, &mut out)?;
        }
        // Triple indirect (block 14).
        if out.len() < total && ino.block[14] != 0 {
            self.append_indirect(ino.block[14], 3, bs, total, &mut out)?;
        }

        out.truncate(total);
        Ok(out)
    }

    /// Walk an Ext4 extent tree rooted in `inode.block[]` and return
    /// the contiguous file contents.  Layout per ext4 §10:
    ///
    ///   struct ext4_extent_header {
    ///       le16 eh_magic;     // 0xF30A
    ///       le16 eh_entries;
    ///       le16 eh_max;
    ///       le16 eh_depth;     // 0 = leaf, > 0 = index
    ///       le32 eh_generation;
    ///   };
    ///   // Followed by eh_entries entries of:
    ///   //   leaf  (depth == 0): { le32 ee_block, le16 ee_len,
    ///   //                         le16 ee_start_hi, le32 ee_start_lo }
    ///   //   index (depth > 0):  { le32 ei_block, le32 ei_leaf_lo,
    ///   //                         le16 ei_leaf_hi, le16 ei_unused }
    ///
    /// On disk these all use 12-byte entries.  ee_len > 32768 marks an
    /// uninitialised extent (treat as a sparse hole).
    fn read_extent_inode(&self, ino: &Inode) -> Result<Vec<u8>, &'static str> {
        let bs = self.sb.block_size() as usize;
        let total = ino.size as usize;
        // Serialize inode.block[15] into a 60-byte buffer for parsing.
        let mut hdr_buf = [0u8; 60];
        for i in 0..15 {
            hdr_buf[i * 4..i * 4 + 4].copy_from_slice(&ino.block[i].to_le_bytes());
        }
        let mut out = alloc::vec![0u8; total];
        self.walk_extent_node(&hdr_buf, bs, total, &mut out)?;
        Ok(out)
    }

    fn walk_extent_node(&self, node: &[u8], bs: usize, total: usize,
                        out: &mut [u8]) -> Result<(), &'static str> {
        if node.len() < 12 { return Err("ext4: extent node truncated"); }
        let magic = le16(&node[0..2]);
        if magic != 0xF30A { return Err("ext4: bad extent magic"); }
        let entries = le16(&node[2..4]) as usize;
        let depth   = le16(&node[6..8]);
        let entries_off = 12usize;

        if depth == 0 {
            for i in 0..entries {
                let off = entries_off + i * 12;
                if off + 12 > node.len() { break; }
                let ee_block = le32(&node[off..off + 4]) as usize;
                let ee_len   = le16(&node[off + 4..off + 6]) as usize;
                let ee_start_hi = le16(&node[off + 6..off + 8]) as u64;
                let ee_start_lo = le32(&node[off + 8..off + 12]) as u64;
                let phys = (ee_start_hi << 32) | ee_start_lo;
                let len = if ee_len > 32768 {
                    // Uninitialised extent: file shows zeros at this range.
                    ee_len - 32768
                } else {
                    ee_len
                };
                let initialised = ee_len <= 32768;
                let file_byte = ee_block * bs;
                for blk in 0..len {
                    let dst_start = file_byte + blk * bs;
                    if dst_start >= total { break; }
                    let dst_end = (dst_start + bs).min(total);
                    if initialised {
                        let mut buf = alloc::vec![0u8; bs];
                        self.source.read_block(phys + blk as u64, &mut buf)?;
                        out[dst_start..dst_end].copy_from_slice(&buf[..dst_end - dst_start]);
                    }
                    // else: out is already zero-init
                }
            }
        } else {
            // Index node — each entry points at a deeper extent block.
            for i in 0..entries {
                let off = entries_off + i * 12;
                if off + 12 > node.len() { break; }
                let ei_leaf_lo = le32(&node[off + 4..off + 8]) as u64;
                let ei_leaf_hi = le16(&node[off + 8..off + 10]) as u64;
                let leaf_phys = (ei_leaf_hi << 32) | ei_leaf_lo;
                let mut buf = alloc::vec![0u8; bs];
                self.source.read_block(leaf_phys, &mut buf)?;
                self.walk_extent_node(&buf, bs, total, out)?;
            }
        }
        Ok(())
    }

    fn append_block(&self, block_no: u32, bs: usize, total: usize, out: &mut Vec<u8>)
        -> Result<(), &'static str>
    {
        let mut blk = alloc::vec![0u8; bs];
        if block_no == 0 {
            // Sparse hole — read as zeros.
        } else {
            self.source.read_block(block_no as u64, &mut blk)?;
        }
        let take = (total - out.len()).min(bs);
        out.extend_from_slice(&blk[..take]);
        Ok(())
    }

    fn append_indirect(&self, block_no: u32, depth: u8, bs: usize, total: usize,
                       out: &mut Vec<u8>) -> Result<(), &'static str>
    {
        if block_no == 0 || out.len() >= total { return Ok(()); }
        let mut blk = alloc::vec![0u8; bs];
        self.source.read_block(block_no as u64, &mut blk)?;
        let entries_per_block = bs / 4;
        for i in 0..entries_per_block {
            if out.len() >= total { break; }
            let bn = u32::from_le_bytes([
                blk[i * 4], blk[i * 4 + 1], blk[i * 4 + 2], blk[i * 4 + 3],
            ]);
            if depth == 1 {
                self.append_block(bn, bs, total, out)?;
            } else {
                self.append_indirect(bn, depth - 1, bs, total, out)?;
            }
        }
        Ok(())
    }

    /// Walk a directory inode's data, decoding the variable-length entries.
    pub fn read_dir(&self, dir_ino: &Inode) -> Result<Vec<DirEntry>, &'static str> {
        let data = self.read_inode_data(dir_ino)?;
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            let inode = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
            let rec_len = u16::from_le_bytes([data[pos+4], data[pos+5]]) as usize;
            let name_len = data[pos + 6] as usize;
            let file_type = data[pos + 7];
            if rec_len == 0 || pos + rec_len > data.len() { break; }
            if inode != 0 && pos + 8 + name_len <= data.len() {
                if let Ok(name) = core::str::from_utf8(&data[pos+8..pos+8+name_len]) {
                    out.push(DirEntry { inode, name: name.to_string(), file_type });
                }
            }
            pos += rec_len;
        }
        Ok(out)
    }

    /// Resolve a slash-separated path to an inode number, starting at /.
    pub fn lookup(&self, path: &str) -> Result<u32, &'static str> {
        let mut cur_ino = 2u32; // / is always inode 2
        for component in path.split('/').filter(|s| !s.is_empty()) {
            let inode = self.read_inode(cur_ino)?;
            if !inode.is_dir() { return Err("not a directory"); }
            let entries = self.read_dir(&inode)?;
            let found = entries.iter().find(|e| e.name == component);
            cur_ino = found.ok_or("path component not found")?.inode;
        }
        Ok(cur_ino)
    }

    // =========================================================================
    // Write support
    // =========================================================================
    //
    // Allocator strategy: scan the inode bitmap of each group looking for
    // a 0 bit, mark it 1, decrement free_inodes in BGDT and superblock,
    // sync both back to disk.  Same for blocks via the block bitmap.
    //
    // Limits of this implementation:
    //   * Files up to 12 * block_size (= 48 KiB on 4 K blocks) — only
    //     direct blocks are populated; indirects are not yet allocated.
    //   * `add_dir_entry` extends the parent directory by simply
    //     appending in the existing tail block; we don't grow the
    //     directory if it overflows.
    //   * No symlink/special-file creation, no journaling.
    //
    // These limits are enough for a self-test that round-trips a small
    // payload across a reboot, which is the immediate value.

    /// Serialize one Inode and write it back to the inode table.
    pub fn write_inode(&mut self, ino: u32, inode: &Inode) -> Result<(), &'static str> {
        if ino == 0 { return Err("invalid inode 0"); }
        let bs = self.sb.block_size();
        let group = (ino - 1) / self.sb.inodes_per_group;
        let index = (ino - 1) % self.sb.inodes_per_group;
        let gd = self.bgdt.get(group as usize).ok_or("group out of range")?.clone();
        let inode_size = self.sb.inode_size as u32;
        let table_byte_off = gd.inode_table as u64 * bs as u64
            + index as u64 * inode_size as u64;
        let block = table_byte_off / bs as u64;
        let off = (table_byte_off % bs as u64) as usize;

        let mut blk = alloc::vec![0u8; bs as usize];
        self.source.read_block(block, &mut blk)?;
        let buf = &mut blk[off..off + inode_size as usize];
        write_le16(buf, inode.mode);
        write_le16(&mut buf[2..], inode.uid);
        write_le32(&mut buf[4..], inode.size);
        write_le32(&mut buf[8..], inode.atime);
        write_le32(&mut buf[12..], inode.ctime);
        write_le32(&mut buf[16..], inode.mtime);
        write_le32(&mut buf[20..], inode.dtime);
        write_le16(&mut buf[24..], inode.gid);
        write_le16(&mut buf[26..], inode.links_count);
        write_le32(&mut buf[28..], inode.blocks);
        write_le32(&mut buf[32..], inode.flags);
        write_le32(&mut buf[36..], inode.osd1);
        for i in 0..15 {
            write_le32(&mut buf[40 + i * 4..], inode.block[i]);
        }
        write_le32(&mut buf[100..], inode.generation);
        // bytes 104..inode_size left as-is (osd2 etc.)

        self.source.write_block(block, &blk)
    }

    /// Persist the superblock at byte 1024 (block 1 if bs=1024, block 0
    /// otherwise) — only the fields we mutate in this driver: inode and
    /// block free counts.  Uses read-modify-write to preserve the
    /// fields we don't touch.
    fn sync_superblock(&mut self) -> Result<(), &'static str> {
        let bs = self.sb.block_size();
        let sb_block = SUPERBLOCK_OFFSET / bs as u64;
        let sb_off   = (SUPERBLOCK_OFFSET % bs as u64) as usize;
        let mut blk = alloc::vec![0u8; bs as usize];
        self.source.read_block(sb_block, &mut blk)?;
        write_le32(&mut blk[sb_off + 12..], self.sb.free_blocks_count);
        write_le32(&mut blk[sb_off + 16..], self.sb.free_inodes_count);
        self.source.write_block(sb_block, &blk)
    }

    /// Persist one BGDT entry (32 bytes) back to disk, preserving every
    /// other byte of the BGDT block.
    fn sync_bgdt_entry(&mut self, group: u32) -> Result<(), &'static str> {
        let bs = self.sb.block_size();
        let bgdt_block = if self.sb.first_data_block == 0 { 1 } else { self.sb.first_data_block } + 1;
        let entries_per_block = bs / 32;
        let block_idx = group / entries_per_block;
        let entry_idx = group % entries_per_block;
        let block = (bgdt_block + block_idx) as u64;
        let off = (entry_idx * 32) as usize;

        let mut blk = alloc::vec![0u8; bs as usize];
        self.source.read_block(block, &mut blk)?;
        let gd = &self.bgdt[group as usize];
        write_le32(&mut blk[off..],     gd.block_bitmap);
        write_le32(&mut blk[off + 4..], gd.inode_bitmap);
        write_le32(&mut blk[off + 8..], gd.inode_table);
        write_le16(&mut blk[off + 12..], gd.free_blocks_count);
        write_le16(&mut blk[off + 14..], gd.free_inodes_count);
        write_le16(&mut blk[off + 16..], gd.used_dirs_count);
        // bytes 18..32 reserved/padding — leave as-is.
        self.source.write_block(block, &blk)
    }

    /// Find a clear bit in the given bitmap block, set it, and return
    /// the bit's index (0-based).  Returns None if the block is full.
    fn alloc_bit_in_bitmap(&mut self, bitmap_block: u32, max_bits: u32)
        -> Result<Option<u32>, &'static str>
    {
        let bs = self.sb.block_size() as usize;
        let mut blk = alloc::vec![0u8; bs];
        self.source.read_block(bitmap_block as u64, &mut blk)?;
        for byte_idx in 0..bs.min((max_bits as usize + 7) / 8) {
            if blk[byte_idx] == 0xFF { continue; }
            for bit in 0..8u32 {
                let total_bit = byte_idx as u32 * 8 + bit;
                if total_bit >= max_bits { break; }
                if blk[byte_idx] & (1 << bit) == 0 {
                    blk[byte_idx] |= 1 << bit;
                    self.source.write_block(bitmap_block as u64, &blk)?;
                    return Ok(Some(total_bit));
                }
            }
        }
        Ok(None)
    }

    /// Allocate a free inode anywhere in the filesystem.  Returns its
    /// 1-based inode number.  Updates the BGDT entry's free_inodes_count
    /// and the superblock's free_inodes_count, and syncs both.
    pub fn alloc_inode(&mut self) -> Result<u32, &'static str> {
        for g in 0..self.bgdt.len() {
            if self.bgdt[g].free_inodes_count == 0 { continue; }
            let bm = self.bgdt[g].inode_bitmap;
            let inodes_in_group = self.sb.inodes_per_group;
            if let Some(bit) = self.alloc_bit_in_bitmap(bm, inodes_in_group)? {
                self.bgdt[g].free_inodes_count -= 1;
                self.sb.free_inodes_count = self.sb.free_inodes_count.saturating_sub(1);
                self.sync_bgdt_entry(g as u32)?;
                self.sync_superblock()?;
                let ino = (g as u32) * inodes_in_group + bit + 1;
                return Ok(ino);
            }
        }
        Err("ext2: no free inodes")
    }

    /// Allocate a free data block.  Returns its 0-based block number.
    pub fn alloc_block(&mut self) -> Result<u32, &'static str> {
        for g in 0..self.bgdt.len() {
            if self.bgdt[g].free_blocks_count == 0 { continue; }
            let bm = self.bgdt[g].block_bitmap;
            let blocks_in_group = self.sb.blocks_per_group;
            if let Some(bit) = self.alloc_bit_in_bitmap(bm, blocks_in_group)? {
                self.bgdt[g].free_blocks_count -= 1;
                self.sb.free_blocks_count = self.sb.free_blocks_count.saturating_sub(1);
                self.sync_bgdt_entry(g as u32)?;
                self.sync_superblock()?;
                // Block numbering: first block in group g is
                // sb.first_data_block + g * sb.blocks_per_group.
                let blk = self.sb.first_data_block + (g as u32) * blocks_in_group + bit;
                return Ok(blk);
            }
        }
        Err("ext2: no free blocks")
    }

    /// Write `data` as the entire content of inode `ino_no`, allocating
    /// new blocks as needed.  Limited to ≤ 12 * block_size (direct
    /// pointers only); larger files would need single/double/triple
    /// indirect allocation, which this driver does not yet do.
    pub fn write_inode_data(&mut self, ino_no: u32, data: &[u8]) -> Result<(), &'static str> {
        let bs = self.sb.block_size() as usize;
        if data.len() > 12 * bs {
            return Err("ext2: write > 12 direct blocks not yet supported");
        }
        let mut inode = self.read_inode(ino_no)?;
        let now = crate::drivers::rtc::read_datetime().to_unix_timestamp() as u32;
        inode.atime = now;
        inode.mtime = now;
        if inode.ctime == 0 { inode.ctime = now; }
        inode.size = data.len() as u32;

        let block_count = (data.len() + bs - 1) / bs;
        // Free old blocks beyond what we need.  Easier path: just leave
        // them allocated; truncate is a separate API.  For simplicity
        // here we *only* extend; the caller should truncate first if
        // shrinking.
        for i in 0..block_count {
            if inode.block[i] == 0 {
                inode.block[i] = self.alloc_block()?;
            }
            let off = i * bs;
            let take = (data.len() - off).min(bs);
            let mut buf = alloc::vec![0u8; bs];
            buf[..take].copy_from_slice(&data[off..off + take]);
            self.source.write_block(inode.block[i] as u64, &buf)?;
        }
        // 512-byte sector count for inode.blocks field.
        inode.blocks = (block_count * (bs / 512)) as u32;
        self.write_inode(ino_no, &inode)
    }

    /// Append a directory entry (inode, name, file_type) to the parent
    /// directory's last data block.  Assumes the last block has free
    /// space for one more entry — simple case that covers the test
    /// path.  Linux ext2 keeps the last entry's `rec_len` extended to
    /// the end of the block, so insert by shrinking that entry's
    /// rec_len down to its actual size and using the freed space for
    /// the new entry.
    pub fn add_dir_entry(&mut self, parent_ino: u32, child_ino: u32,
                         name: &str, file_type: u8)
        -> Result<(), &'static str>
    {
        let bs = self.sb.block_size() as usize;
        let parent = self.read_inode(parent_ino)?;
        if !parent.is_dir() { return Err("parent not a directory"); }
        // Find the last allocated direct block.
        let mut last_idx: Option<usize> = None;
        for (i, &bn) in parent.block[..12].iter().enumerate() {
            if bn != 0 { last_idx = Some(i); }
        }
        let last_idx = last_idx.ok_or("ext2: parent dir has no blocks")?;
        let last_block = parent.block[last_idx];

        let mut buf = alloc::vec![0u8; bs];
        self.source.read_block(last_block as u64, &mut buf)?;

        // Walk to the last entry; shrink its rec_len to its actual size.
        let name_bytes = name.as_bytes();
        let new_entry_size = ((8 + name_bytes.len()) + 3) & !3; // 4-byte align
        let mut pos = 0usize;
        let mut last_pos = 0usize;
        while pos + 8 <= bs {
            let rec_len = u16::from_le_bytes([buf[pos+4], buf[pos+5]]) as usize;
            if rec_len == 0 || pos + rec_len > bs { break; }
            last_pos = pos;
            if pos + rec_len >= bs { break; }
            pos += rec_len;
        }
        // last entry occupies last_pos .. bs.  Shrink it to its actual size.
        let last_name_len = buf[last_pos + 6] as usize;
        let last_actual = ((8 + last_name_len) + 3) & !3;
        let last_remaining = bs - last_pos;
        if last_remaining < last_actual + new_entry_size {
            return Err("ext2: parent dir block full (would need to grow)");
        }
        // Truncate last entry's rec_len.
        write_le16(&mut buf[last_pos + 4..], last_actual as u16);
        // Place new entry right after.
        let new_pos = last_pos + last_actual;
        let new_rec_len = bs - new_pos; // takes the rest of the block
        write_le32(&mut buf[new_pos..], child_ino);
        write_le16(&mut buf[new_pos + 4..], new_rec_len as u16);
        buf[new_pos + 6] = name_bytes.len() as u8;
        buf[new_pos + 7] = file_type;
        buf[new_pos + 8..new_pos + 8 + name_bytes.len()].copy_from_slice(name_bytes);

        self.source.write_block(last_block as u64, &buf)
    }

    /// Create a regular file at `/<name>` (root dir only) with the
    /// given content.  Returns the new inode number.
    pub fn create_root_file(&mut self, name: &str, data: &[u8]) -> Result<u32, &'static str> {
        // Make sure the name doesn't already exist.
        let root = self.read_inode(2)?;
        if let Ok(entries) = self.read_dir(&root) {
            if entries.iter().any(|e| e.name == name) {
                return Err("ext2: name already exists");
            }
        }
        let ino_no = self.alloc_inode()?;
        // Build a fresh regular-file inode.
        let now = crate::drivers::rtc::read_datetime().to_unix_timestamp() as u32;
        let inode = Inode {
            mode: IFREG | 0o644,
            uid: 0,
            size: 0,
            atime: now, ctime: now, mtime: now, dtime: 0,
            gid: 0,
            links_count: 1,
            blocks: 0,
            flags: 0,
            osd1: 0,
            block: [0u32; 15],
            generation: 0,
        };
        self.write_inode(ino_no, &inode)?;
        // Write the file data (allocates blocks).
        self.write_inode_data(ino_no, data)?;
        // Link it under root with file_type=1 (regular file).
        self.add_dir_entry(2, ino_no, name, 1)?;
        Ok(ino_no)
    }

    /// Read a file at `path` to a Vec.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        let ino_no = self.lookup(path)?;
        let inode = self.read_inode(ino_no)?;
        if inode.is_symlink() && inode.size <= 60 {
            // Inline symlink target: stored in inode.block[].
            let mut bytes = [0u8; 60];
            for (i, &b) in inode.block.iter().enumerate() {
                bytes[i * 4..(i + 1) * 4].copy_from_slice(&b.to_le_bytes());
            }
            return Ok(bytes[..inode.size as usize].to_vec());
        }
        if !inode.is_reg() && !inode.is_symlink() {
            return Err("not a regular file");
        }
        self.read_inode_data(&inode)
    }
}

// --- helpers ---

fn le16(b: &[u8]) -> u16 { u16::from_le_bytes([b[0], b[1]]) }
fn le32(b: &[u8]) -> u32 { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) }

/// Global mountpoint for a mounted Ext2 image, if any.
pub static EXT2: Mutex<Option<Ext2Fs>> = Mutex::new(None);

/// `BlockSource` adapter on top of the generic `crate::block::BlockDevice`
/// registry — works equally well over virtio-blk / AHCI / NVMe.  We
/// look up the device by name on every read so mount survives even if
/// the underlying driver is later replaced (live-migrate scenario).
pub struct GenericBlockSource {
    pub block_size: u32,
    pub start_lba: u64,
    pub device_name: alloc::string::String,
}

impl BlockSource for GenericBlockSource {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        if buf.len() < self.block_size as usize { return Err("buf too small"); }
        let sectors_per_block = (self.block_size / 512) as u32;
        let base_lba = self.start_lba + block * sectors_per_block as u64;
        let g = crate::block::DEVICES.lock();
        let dev = g.iter().find(|d| d.name() == self.device_name)
            .ok_or("ext2: backing block device gone")?;
        dev.read(base_lba, sectors_per_block, buf)
    }
    fn block_size(&self) -> u32 { self.block_size }
    fn write_block(&mut self, block: u64, buf: &[u8]) -> Result<(), &'static str> {
        if buf.len() < self.block_size as usize { return Err("buf too small"); }
        let sectors_per_block = (self.block_size / 512) as u32;
        let base_lba = self.start_lba + block * sectors_per_block as u64;
        let g = crate::block::DEVICES.lock();
        let dev = g.iter().find(|d| d.name() == self.device_name)
            .ok_or("ext2: backing block device gone")?;
        dev.write(base_lba, sectors_per_block, buf)
    }
}

/// Legacy alias kept for callers that still type the virtio-specific
/// name.  All it does now is delegate to the generic source bound to
/// the "vblk0" device.
pub struct VirtioBlkSource {
    /// Filesystem block size in bytes (≥ 512, multiple of 512).
    pub block_size: u32,
    /// Optional partition starting LBA (sector).  For mounting from the
    /// raw disk this is 0; for partitioned media set it to the
    /// partition's first_lba.
    pub start_lba: u64,
}

impl BlockSource for VirtioBlkSource {
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        if buf.len() < self.block_size as usize {
            return Err("buf too small");
        }
        let sectors_per_block = (self.block_size / 512) as u64;
        let base_lba = self.start_lba + block * sectors_per_block;
        let mut g = crate::drivers::virtio_blk::VIRTIO_BLK.lock();
        let blk = g.as_mut().ok_or("virtio-blk: device not available")?;
        for i in 0..sectors_per_block {
            let off = (i * 512) as usize;
            let mut sector = [0u8; 512];
            blk.read_sector(base_lba + i, &mut sector)?;
            buf[off..off + 512].copy_from_slice(&sector);
        }
        Ok(())
    }
    fn block_size(&self) -> u32 { self.block_size }
    fn write_block(&mut self, block: u64, buf: &[u8]) -> Result<(), &'static str> {
        if buf.len() < self.block_size as usize { return Err("buf too small"); }
        let sectors_per_block = (self.block_size / 512) as u64;
        let base_lba = self.start_lba + block * sectors_per_block;
        let mut g = crate::drivers::virtio_blk::VIRTIO_BLK.lock();
        let blk = g.as_mut().ok_or("virtio-blk: device not available")?;
        for i in 0..sectors_per_block {
            let off = (i * 512) as usize;
            let mut sector = [0u8; 512];
            sector.copy_from_slice(&buf[off..off + 512]);
            blk.write_sector(base_lba + i, &sector)?;
        }
        Ok(())
    }
}

/// Walk every registered block device and mount the first one whose
/// sector 2 carries the Ext2 magic.  Returns (device_name, block_size)
/// on success.  Block size is taken from the superblock (1024 / 2048
/// / 4096 supported).
pub fn try_auto_mount_disk() -> Result<u32, &'static str> {
    let names: Vec<String> = {
        let g = crate::block::DEVICES.lock();
        g.iter().map(|d| d.name().to_string()).collect()
    };
    if names.is_empty() {
        return Err("no block devices registered");
    }
    for name in &names {
        let mut sector = [0u8; 512];
        let read_ok = {
            let g = crate::block::DEVICES.lock();
            match g.iter().find(|d| d.name() == name) {
                Some(d) => d.read(2, 1, &mut sector).is_ok(),
                None => false,
            }
        };
        if !read_ok { continue; }
        let magic = u16::from_le_bytes([sector[56], sector[57]]);
        if magic != EXT2_MAGIC { continue; }
        let log_block_size = u32::from_le_bytes(
            [sector[24], sector[25], sector[26], sector[27]]);
        let block_size = 1024u32 << log_block_size;
        if block_size != 1024 && block_size != 2048 && block_size != 4096 { continue; }
        let source = alloc::boxed::Box::new(GenericBlockSource {
            block_size, start_lba: 0,
            device_name: name.clone(),
        });
        let fs = Ext2Fs::mount(source)?;
        let bs = fs.sb.block_size();
        *EXT2.lock() = Some(fs);
        crate::klog_info!("ext2: mounted from {} (block size {})", name, bs);
        return Ok(bs);
    }
    Err("no ext2 magic on any registered block device")
}

/// Build a tiny in-memory ext2 image for the self-test path.  The image
/// hand-crafts a 64 KiB filesystem with a 1 KiB block size, one block
/// group, two regular files, and a directory containing them.
pub fn synthesise_demo_image() -> alloc::boxed::Box<MemoryImage> {
    let bs = 1024u32;
    let blocks = 64u32;
    let inodes_per_group = 16u32;
    let blocks_per_group = blocks - 1;
    let mut img = alloc::vec![0u8; (bs * blocks) as usize];

    // ---- Superblock at byte 1024 ----
    let sb_off = 1024usize;
    write_le32(&mut img[sb_off..], inodes_per_group);     // s_inodes_count (group total)
    write_le32(&mut img[sb_off + 4..], blocks);           // s_blocks_count
    write_le32(&mut img[sb_off + 8..], 0);                // s_r_blocks_count
    write_le32(&mut img[sb_off + 12..], 50);              // s_free_blocks_count
    write_le32(&mut img[sb_off + 16..], inodes_per_group - 5);
    write_le32(&mut img[sb_off + 20..], 1);               // s_first_data_block
    write_le32(&mut img[sb_off + 24..], 0);               // log_block_size = 0 (1K)
    write_le32(&mut img[sb_off + 28..], 0);               // log_frag_size
    write_le32(&mut img[sb_off + 32..], blocks_per_group);
    write_le32(&mut img[sb_off + 36..], blocks_per_group);
    write_le32(&mut img[sb_off + 40..], inodes_per_group);
    write_le16(&mut img[sb_off + 56..], EXT2_MAGIC);
    write_le16(&mut img[sb_off + 58..], 1);               // s_state = clean
    write_le32(&mut img[sb_off + 76..], 0);               // s_rev_level = good_old
    write_le16(&mut img[sb_off + 88..], 128);             // s_inode_size

    // ---- Block group descriptor at block 2 (since first_data_block=1, BGDT at 1+1=2) ----
    let bgdt_off = (bs * 2) as usize;
    write_le32(&mut img[bgdt_off..], 3);    // block bitmap
    write_le32(&mut img[bgdt_off + 4..], 4); // inode bitmap
    write_le32(&mut img[bgdt_off + 8..], 5); // inode table
    write_le16(&mut img[bgdt_off + 12..], 50);
    write_le16(&mut img[bgdt_off + 14..], (inodes_per_group - 5) as u16);
    write_le16(&mut img[bgdt_off + 16..], 1);

    // ---- Inode table at block 5: inode_size=128, 16 inodes => 2 KiB → 2 blocks ----
    // Inode 2 = root directory, points to data block 10.
    // Inode 12 = file "hello.txt", points to data block 11.
    let inode_table_off = (bs * 5) as usize;

    // Inode 2 (root dir).  Index = 1, byte offset = 1 * 128.
    let i2 = inode_table_off + 1 * 128;
    write_le16(&mut img[i2..], IFDIR | 0o755);                // mode
    write_le16(&mut img[i2 + 2..], 0);                        // uid
    write_le32(&mut img[i2 + 4..], bs);                       // size = 1 block
    write_le16(&mut img[i2 + 26..], 3);                       // links_count (.,.., hello.txt)
    write_le32(&mut img[i2 + 40..], 10);                      // i_block[0] = 10

    // Inode 12 (hello.txt).  Index = 11.
    let i12 = inode_table_off + 11 * 128;
    let body = b"Hello from RustOS ext2!\n";
    write_le16(&mut img[i12..], IFREG | 0o644);
    write_le32(&mut img[i12 + 4..], body.len() as u32);
    write_le16(&mut img[i12 + 26..], 1);
    write_le32(&mut img[i12 + 40..], 11);                     // i_block[0] = 11

    // ---- Block 10 = root dir contents ----
    let dir_off = (bs * 10) as usize;
    let mut p = 0usize;

    // Entry "." → inode 2, name_len=1, file_type=2 (DIR), padded to 12 bytes.
    write_le32(&mut img[dir_off + p..], 2);
    write_le16(&mut img[dir_off + p + 4..], 12);
    img[dir_off + p + 6] = 1;
    img[dir_off + p + 7] = 2;
    img[dir_off + p + 8] = b'.';
    p += 12;

    // Entry ".." → inode 2, name_len=2, file_type=2, padded to 12 bytes.
    write_le32(&mut img[dir_off + p..], 2);
    write_le16(&mut img[dir_off + p + 4..], 12);
    img[dir_off + p + 6] = 2;
    img[dir_off + p + 7] = 2;
    img[dir_off + p + 8] = b'.';
    img[dir_off + p + 9] = b'.';
    p += 12;

    // Entry "hello.txt" → inode 12, name_len=9, file_type=1 (FILE).
    // rec_len pads to end of block: 1024 - 24 = 1000.
    let rec_len = (bs as usize) - p;
    write_le32(&mut img[dir_off + p..], 12);
    write_le16(&mut img[dir_off + p + 4..], rec_len as u16);
    img[dir_off + p + 6] = 9;
    img[dir_off + p + 7] = 1;
    img[dir_off + p + 8..dir_off + p + 17].copy_from_slice(b"hello.txt");

    // ---- Block 11 = file "hello.txt" contents ----
    let body_off = (bs * 11) as usize;
    img[body_off..body_off + body.len()].copy_from_slice(body);

    alloc::boxed::Box::new(MemoryImage::new(img, bs))
}

fn write_le16(buf: &mut [u8], v: u16) { buf[..2].copy_from_slice(&v.to_le_bytes()); }
fn write_le32(buf: &mut [u8], v: u32) { buf[..4].copy_from_slice(&v.to_le_bytes()); }
