/// IDE/ATA PIO (Programmed I/O) disk driver
///
/// This driver provides basic read/write access to IDE disks using PIO mode.
/// It supports both primary and secondary IDE channels with master/slave drives.

use spin::Mutex;
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};

/// ATA status register flags
const ATA_SR_BSY: u8 = 0x80;  // Busy
#[allow(dead_code)]
const ATA_SR_DRDY: u8 = 0x40; // Drive ready
const ATA_SR_DF: u8 = 0x20;   // Drive write fault
#[allow(dead_code)]
const ATA_SR_DSC: u8 = 0x10;  // Drive seek complete
const ATA_SR_DRQ: u8 = 0x08;  // Data request ready
#[allow(dead_code)]
const ATA_SR_CORR: u8 = 0x04; // Corrected data
#[allow(dead_code)]
const ATA_SR_IDX: u8 = 0x02;  // Index
const ATA_SR_ERR: u8 = 0x01;  // Error

/// ATA error register flags (reserved for future diagnostics)
#[allow(dead_code)]
const ATA_ER_BBK: u8 = 0x80;   // Bad block
#[allow(dead_code)]
const ATA_ER_UNC: u8 = 0x40;   // Uncorrectable data
#[allow(dead_code)]
const ATA_ER_MC: u8 = 0x20;    // Media changed
#[allow(dead_code)]
const ATA_ER_IDNF: u8 = 0x10;  // ID not found
#[allow(dead_code)]
const ATA_ER_MCR: u8 = 0x08;   // Media change request
#[allow(dead_code)]
const ATA_ER_ABRT: u8 = 0x04;  // Command aborted
#[allow(dead_code)]
const ATA_ER_TK0NF: u8 = 0x02; // Track 0 not found
#[allow(dead_code)]
const ATA_ER_AMNF: u8 = 0x01;  // Address mark not found

/// ATA commands
const ATA_CMD_READ_PIO: u8 = 0x20;
#[allow(dead_code)]
const ATA_CMD_READ_PIO_EXT: u8 = 0x24;
const ATA_CMD_WRITE_PIO: u8 = 0x30;
#[allow(dead_code)]
const ATA_CMD_WRITE_PIO_EXT: u8 = 0x34;
#[allow(dead_code)]
const ATA_CMD_CACHE_FLUSH: u8 = 0xE7;
#[allow(dead_code)]
const ATA_CMD_CACHE_FLUSH_EXT: u8 = 0xEA;
#[allow(dead_code)]
const ATA_CMD_IDENTIFY: u8 = 0xEC;

/// ATA identification space (reserved for future use)
#[allow(dead_code)]
const ATA_IDENT_DEVICETYPE: u8 = 0;
#[allow(dead_code)]
const ATA_IDENT_CYLINDERS: u8 = 2;
#[allow(dead_code)]
const ATA_IDENT_HEADS: u8 = 6;
#[allow(dead_code)]
const ATA_IDENT_SECTORS: u8 = 12;
#[allow(dead_code)]
const ATA_IDENT_SERIAL: u8 = 20;
#[allow(dead_code)]
const ATA_IDENT_MODEL: u8 = 54;
#[allow(dead_code)]
const ATA_IDENT_CAPABILITIES: u8 = 98;
#[allow(dead_code)]
const ATA_IDENT_FIELDVALID: u8 = 106;
#[allow(dead_code)]
const ATA_IDENT_MAX_LBA: u8 = 120;
#[allow(dead_code)]
const ATA_IDENT_COMMANDSETS: u8 = 164;
#[allow(dead_code)]
const ATA_IDENT_MAX_LBA_EXT: u8 = 200;

/// Sector size in bytes
pub const SECTOR_SIZE: usize = 512;

/// Primary IDE bus I/O ports
const PRIMARY_IO_BASE: u16 = 0x1F0;
const PRIMARY_CONTROL_BASE: u16 = 0x3F6;

/// Secondary IDE bus I/O ports (reserved for future use)
#[allow(dead_code)]
const SECONDARY_IO_BASE: u16 = 0x170;
#[allow(dead_code)]
const SECONDARY_CONTROL_BASE: u16 = 0x376;

/// ATA drive
pub struct AtaDrive {
    // I/O ports for primary channel, SLAVE drive (not master!)
    data_port: Port<u16>,
    #[allow(dead_code)]
    error_port: PortReadOnly<u8>,
    #[allow(dead_code)]
    features_port: PortWriteOnly<u8>,
    sector_count_port: Port<u8>,
    lba_low_port: Port<u8>,
    lba_mid_port: Port<u8>,
    lba_high_port: Port<u8>,
    drive_port: Port<u8>,
    status_port: PortReadOnly<u8>,
    command_port: PortWriteOnly<u8>,
    #[allow(dead_code)]
    control_port: Port<u8>,
}

impl AtaDrive {
    /// Create a new ATA drive on the primary bus, SLAVE drive
    pub const fn new() -> Self {
        AtaDrive {
            data_port: Port::new(PRIMARY_IO_BASE),
            error_port: PortReadOnly::new(PRIMARY_IO_BASE + 1),
            features_port: PortWriteOnly::new(PRIMARY_IO_BASE + 1),
            sector_count_port: Port::new(PRIMARY_IO_BASE + 2),
            lba_low_port: Port::new(PRIMARY_IO_BASE + 3),
            lba_mid_port: Port::new(PRIMARY_IO_BASE + 4),
            lba_high_port: Port::new(PRIMARY_IO_BASE + 5),
            drive_port: Port::new(PRIMARY_IO_BASE + 6),
            status_port: PortReadOnly::new(PRIMARY_IO_BASE + 7),
            command_port: PortWriteOnly::new(PRIMARY_IO_BASE + 7),
            control_port: Port::new(PRIMARY_CONTROL_BASE),
        }
    }

    /// Wait for the drive to be ready (not busy)
    fn wait_ready(&mut self) -> Result<(), &'static str> {
        unsafe {
            // Increased timeout for slower QEMU/virtualized environments
            for _ in 0..100000 {
                let status = self.status_port.read();
                if (status & ATA_SR_BSY) == 0 {
                    return Ok(());
                }
                x86_64::instructions::nop();
            }
            Err("ATA timeout waiting for drive ready")
        }
    }

    /// Wait for the drive to signal data is ready
    fn wait_data(&mut self) -> Result<(), &'static str> {
        unsafe {
            // Wait for BSY to clear and DRQ to set
            // Increased timeout for slower QEMU/virtualized environments
            for _ in 0..100000 {
                let status = self.status_port.read();

                // Check for errors first
                if (status & ATA_SR_ERR) != 0 {
                    return Err("ATA error");
                }
                if (status & ATA_SR_DF) != 0 {
                    return Err("ATA drive fault");
                }

                // Check if ready (BSY=0, DRQ=1)
                if (status & ATA_SR_BSY) == 0 && (status & ATA_SR_DRQ) != 0 {
                    return Ok(());
                }
            }
            Err("ATA timeout waiting for data")
        }
    }

    /// Read a single sector from the disk
    ///
    /// # Arguments
    /// * `lba` - Logical Block Address (sector number)
    /// * `buffer` - 512-byte buffer to store the sector data
    pub fn read_sector(&mut self, lba: u32, buffer: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        if buffer.len() != SECTOR_SIZE {
            return Err("Buffer must be exactly 512 bytes");
        }

        unsafe {
            // Wait for drive to be ready
            self.wait_ready()?;

            // Select PRIMARY SLAVE drive and set LBA mode
            // Bits 0-3: LBA bits 24-27
            // Bit 4: 0 = master, 1 = slave (we use SLAVE = 1)
            // Bit 5: Always 1
            // Bit 6: 1 = LBA mode, 0 = CHS mode
            // Bit 7: Always 1
            // 0xF0 = 0xE0 | 0x10 (bit 4 set for slave)
            self.drive_port.write(0xF0 | ((lba >> 24) & 0x0F) as u8);

            // CRITICAL: 400ns delay after drive selection (15 reads)
            for _ in 0..15 {
                self.status_port.read();
            }

            // Write sector count (1 sector)
            self.sector_count_port.write(1);

            // Write LBA address
            self.lba_low_port.write((lba & 0xFF) as u8);
            self.lba_mid_port.write(((lba >> 8) & 0xFF) as u8);
            self.lba_high_port.write(((lba >> 16) & 0xFF) as u8);

            // Send read command
            self.command_port.write(ATA_CMD_READ_PIO);

            // Wait for data to be ready
            self.wait_data()?;

            // Read 256 words (512 bytes) from data port
            let buffer_ptr = buffer.as_mut_ptr() as *mut u16;
            for i in 0..256 {
                let word = self.data_port.read();
                buffer_ptr.add(i).write_volatile(word);
            }

            // 400ns delay after data transfer to reset DRQ bit
            for _ in 0..15 {
                self.status_port.read();
            }
        }

        Ok(())
    }

    /// Read multiple consecutive sectors from the disk
    ///
    /// # Arguments
    /// * `lba` - Starting Logical Block Address
    /// * `sector_count` - Number of sectors to read
    /// * `buffer` - Buffer to store the data (must be at least sector_count * 512 bytes)
    pub fn read_sectors(&mut self, lba: u32, sector_count: usize, buffer: &mut [u8]) -> Result<(), &'static str> {
        if buffer.len() < sector_count * SECTOR_SIZE {
            return Err("Buffer too small for requested sectors");
        }

        for i in 0..sector_count {
            let mut sector_buffer = [0u8; SECTOR_SIZE];
            self.read_sector(lba + i as u32, &mut sector_buffer)?;

            let offset = i * SECTOR_SIZE;
            buffer[offset..offset + SECTOR_SIZE].copy_from_slice(&sector_buffer);
        }

        Ok(())
    }

    /// Write a single sector to the disk
    ///
    /// # Arguments
    /// * `lba` - Logical Block Address (sector number)
    /// * `buffer` - 512-byte buffer containing data to write
    pub fn write_sector(&mut self, lba: u32, buffer: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        if buffer.len() != SECTOR_SIZE {
            return Err("Buffer must be exactly 512 bytes");
        }

        unsafe {
            // Wait for drive to be ready
            self.wait_ready()?;

            // Select PRIMARY SLAVE drive and set LBA mode
            // 0xF0 = slave + LBA mode (bit 4 set)
            self.drive_port.write(0xF0 | ((lba >> 24) & 0x0F) as u8);

            // CRITICAL: 400ns delay after drive selection (15 reads)
            for _ in 0..15 {
                self.status_port.read();
            }

            // Write sector count (1 sector)
            self.sector_count_port.write(1);

            // Write LBA address
            self.lba_low_port.write((lba & 0xFF) as u8);
            self.lba_mid_port.write(((lba >> 8) & 0xFF) as u8);
            self.lba_high_port.write(((lba >> 16) & 0xFF) as u8);

            // Send write command
            self.command_port.write(ATA_CMD_WRITE_PIO);

            // Wait for drive to be ready for data
            self.wait_data()?;

            // Write 256 words (512 bytes) to data port
            let buffer_ptr = buffer.as_ptr() as *const u16;
            for i in 0..256 {
                let word = buffer_ptr.add(i).read_volatile();
                self.data_port.write(word);
            }

            // 400ns delay after data transfer to reset DRQ bit
            for _ in 0..15 {
                self.status_port.read();
            }

            // Wait for write to complete
            self.wait_ready()?;
        }

        Ok(())
    }

    /// Write multiple consecutive sectors to the disk
    ///
    /// # Arguments
    /// * `lba` - Starting Logical Block Address
    /// * `sector_count` - Number of sectors to write
    /// * `buffer` - Buffer containing the data (must be at least sector_count * 512 bytes)
    pub fn write_sectors(&mut self, lba: u32, sector_count: usize, buffer: &[u8]) -> Result<(), &'static str> {
        if buffer.len() < sector_count * SECTOR_SIZE {
            return Err("Buffer too small for requested sectors");
        }

        for i in 0..sector_count {
            let offset = i * SECTOR_SIZE;
            let mut sector_buffer = [0u8; SECTOR_SIZE];
            sector_buffer.copy_from_slice(&buffer[offset..offset + SECTOR_SIZE]);
            self.write_sector(lba + i as u32, &sector_buffer)?;
        }

        Ok(())
    }

    /// Check if a drive exists and is accessible
    /// Check if the drive exists using IDENTIFY command
    ///
    /// This follows OSDev wiki recommendations:
    /// 1. Check for floating bus (status = 0xFF)
    /// 2. Send IDENTIFY command
    /// 3. Check if status = 0 (no drive)
    /// 4. Wait for DRQ and drain the data to complete IDENTIFY
    pub fn exists(&mut self) -> bool {
        unsafe {
            // IMPORTANT: Read status BEFORE writing anything to ports
            // If the bus is floating (no drive), it will read as 0xFF
            let status = self.status_port.read();
            if status == 0xFF {
                return false; // Floating bus - no drive
            }

            // Select PRIMARY SLAVE drive (0xB0)
            // We use slave because master is the boot disk
            self.drive_port.write(0xB0);

            // CRITICAL: 400ns delay after drive selection
            // OSDev Wiki: "read the Status register FIFTEEN TIMES,
            // and only pay attention to the value returned by the last one"
            // This creates ~420ns delay for drive to set correct values
            for _ in 0..15 {
                self.status_port.read();
            }

            // Set all registers to 0 for IDENTIFY
            self.sector_count_port.write(0);
            self.lba_low_port.write(0);
            self.lba_mid_port.write(0);
            self.lba_high_port.write(0);

            // Send IDENTIFY command (0xEC)
            self.command_port.write(0xEC);

            // CRITICAL: Poll status until BSY clears OR timeout
            // OSDev Wiki: Must poll BSY first, then check if status == 0
            // Increased timeout for slower QEMU/virtualized environments
            let mut timeout = 100000;
            loop {
                let status = self.status_port.read();

                // If status is 0, no drive exists
                if status == 0 {
                    return false;
                }

                // If BSY cleared, drive responded - break and continue
                if (status & ATA_SR_BSY) == 0 {
                    break;
                }

                timeout -= 1;
                if timeout == 0 {
                    return false; // Timeout waiting for BSY to clear
                }
            }

            // Now wait for DRQ (data ready) or ERR
            // BSY already cleared in previous loop
            let mut timeout = 100000;
            loop {
                let status = self.status_port.read();

                // Check for errors
                if (status & ATA_SR_ERR) != 0 || (status & ATA_SR_DF) != 0 {
                    return false; // Drive error
                }

                // Check if DRQ set (data ready)
                if (status & ATA_SR_DRQ) != 0 {
                    break; // Drive exists and data is ready
                }

                timeout -= 1;
                if timeout == 0 {
                    return false; // Timeout waiting for DRQ
                }
            }

            // IMPORTANT: Drain the IDENTIFY data (256 words)
            // If we don't read this, the next command will fail
            for _ in 0..256 {
                self.data_port.read();
            }

            // Wait for DRQ to clear after reading IDENTIFY data
            // This ensures the drive is ready for the next command
            let mut timeout = 100000;
            loop {
                let status = self.status_port.read();
                if (status & ATA_SR_DRQ) == 0 && (status & ATA_SR_BSY) == 0 {
                    break;
                }
                timeout -= 1;
                if timeout == 0 {
                    break; // Continue anyway, drive might be ready
                }
            }

            // Drive exists and responded correctly
            true
        }
    }
}

/// Global ATA drive instance
pub static ATA_DRIVE: Mutex<AtaDrive> = Mutex::new(AtaDrive::new());

/// Initialize the ATA driver
pub fn init() {
    let mut drive = ATA_DRIVE.lock();

    if drive.exists() {
        crate::println!("[ATA] Primary slave drive detected (FAT32 disk)");
    } else {
        crate::println!("[ATA] No primary slave drive found");
    }
}
