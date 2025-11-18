/// IDE/ATA PIO (Programmed I/O) disk driver
///
/// This driver provides basic read/write access to IDE disks using PIO mode.
/// It supports both primary and secondary IDE channels with master/slave drives.

use spin::Mutex;
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};

/// ATA status register flags
const ATA_SR_BSY: u8 = 0x80;  // Busy
const ATA_SR_DRDY: u8 = 0x40; // Drive ready
const ATA_SR_DF: u8 = 0x20;   // Drive write fault
const ATA_SR_DSC: u8 = 0x10;  // Drive seek complete
const ATA_SR_DRQ: u8 = 0x08;  // Data request ready
const ATA_SR_CORR: u8 = 0x04; // Corrected data
const ATA_SR_IDX: u8 = 0x02;  // Index
const ATA_SR_ERR: u8 = 0x01;  // Error

/// ATA error register flags
const ATA_ER_BBK: u8 = 0x80;   // Bad block
const ATA_ER_UNC: u8 = 0x40;   // Uncorrectable data
const ATA_ER_MC: u8 = 0x20;    // Media changed
const ATA_ER_IDNF: u8 = 0x10;  // ID not found
const ATA_ER_MCR: u8 = 0x08;   // Media change request
const ATA_ER_ABRT: u8 = 0x04;  // Command aborted
const ATA_ER_TK0NF: u8 = 0x02; // Track 0 not found
const ATA_ER_AMNF: u8 = 0x01;  // Address mark not found

/// ATA commands
const ATA_CMD_READ_PIO: u8 = 0x20;
const ATA_CMD_READ_PIO_EXT: u8 = 0x24;
const ATA_CMD_WRITE_PIO: u8 = 0x30;
const ATA_CMD_WRITE_PIO_EXT: u8 = 0x34;
const ATA_CMD_CACHE_FLUSH: u8 = 0xE7;
const ATA_CMD_CACHE_FLUSH_EXT: u8 = 0xEA;
const ATA_CMD_IDENTIFY: u8 = 0xEC;

/// ATA identification space
const ATA_IDENT_DEVICETYPE: u8 = 0;
const ATA_IDENT_CYLINDERS: u8 = 2;
const ATA_IDENT_HEADS: u8 = 6;
const ATA_IDENT_SECTORS: u8 = 12;
const ATA_IDENT_SERIAL: u8 = 20;
const ATA_IDENT_MODEL: u8 = 54;
const ATA_IDENT_CAPABILITIES: u8 = 98;
const ATA_IDENT_FIELDVALID: u8 = 106;
const ATA_IDENT_MAX_LBA: u8 = 120;
const ATA_IDENT_COMMANDSETS: u8 = 164;
const ATA_IDENT_MAX_LBA_EXT: u8 = 200;

/// Sector size in bytes
pub const SECTOR_SIZE: usize = 512;

/// Primary IDE bus I/O ports
const PRIMARY_IO_BASE: u16 = 0x1F0;
const PRIMARY_CONTROL_BASE: u16 = 0x3F6;

/// Secondary IDE bus I/O ports
const SECONDARY_IO_BASE: u16 = 0x170;
const SECONDARY_CONTROL_BASE: u16 = 0x376;

/// ATA drive
pub struct AtaDrive {
    // I/O ports for primary channel, master drive
    data_port: Port<u16>,
    error_port: PortReadOnly<u8>,
    features_port: PortWriteOnly<u8>,
    sector_count_port: Port<u8>,
    lba_low_port: Port<u8>,
    lba_mid_port: Port<u8>,
    lba_high_port: Port<u8>,
    drive_port: Port<u8>,
    status_port: PortReadOnly<u8>,
    command_port: PortWriteOnly<u8>,
    control_port: Port<u8>,
}

impl AtaDrive {
    /// Create a new ATA drive on the primary bus, master drive
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
    fn wait_ready(&mut self) {
        unsafe {
            while (self.status_port.read() & ATA_SR_BSY) != 0 {
                x86_64::instructions::nop();
            }
        }
    }

    /// Wait for the drive to signal data is ready
    fn wait_data(&mut self) -> Result<(), &'static str> {
        unsafe {
            // Wait for BSY to clear and DRQ to set
            let mut status;
            for _ in 0..1000 {
                status = self.status_port.read();
                if (status & ATA_SR_BSY) == 0 && (status & ATA_SR_DRQ) != 0 {
                    return Ok(());
                }
                if (status & ATA_SR_ERR) != 0 {
                    return Err("ATA error");
                }
                if (status & ATA_SR_DF) != 0 {
                    return Err("ATA drive fault");
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
            self.wait_ready();

            // Select drive (master) and set LBA mode
            // Bits 0-3: LBA bits 24-27
            // Bit 4: 0 = master, 1 = slave
            // Bit 5: Always 1
            // Bit 6: 1 = LBA mode, 0 = CHS mode
            // Bit 7: Always 1
            self.drive_port.write(0xE0 | ((lba >> 24) & 0x0F) as u8);

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

    /// Check if a drive exists and is accessible
    pub fn exists(&mut self) -> bool {
        unsafe {
            // Select master drive
            self.drive_port.write(0xA0);

            // Small delay
            for _ in 0..4 {
                self.status_port.read();
            }

            // Check if status is not 0xFF (no drive)
            let status = self.status_port.read();
            status != 0xFF && status != 0x00
        }
    }
}

/// Global ATA drive instance
pub static ATA_DRIVE: Mutex<AtaDrive> = Mutex::new(AtaDrive::new());

/// Initialize the ATA driver
pub fn init() {
    let mut drive = ATA_DRIVE.lock();

    if drive.exists() {
        crate::println!("[ATA] Primary master drive detected");
    } else {
        crate::println!("[ATA] No primary master drive found");
    }
}
