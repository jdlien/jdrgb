//! GPU LEDs — the ENE controller on an ASUS TUF RTX 5090, reached over the
//! graphics card's own I2C bus via NVIDIA's NVAPI.
//!
//! Nothing here needs a kernel driver: `nvapi64.dll` is a normal userspace DLL,
//! and `NvAPI_I2CWriteEx`/`NvAPI_I2CReadEx` tunnel SMBus transactions out to the
//! card's I2C port 1, where ASUS hangs an ENE RGB controller at address 0x67.
//!
//! Protocol ported from OpenRGB's ENESMBusController + i2c_smbus_nvapi
//! (GPL-2.0-or-later). See README for lineage.
//!
//! Two guards stand in front of every write, in this order:
//!   1. an exact PCI vendor/device/subsystem match — an I2C read still puts a
//!      command byte on the wire, so the signature check below is *not* enough
//!      on its own to avoid poking an unrelated controller;
//!   2. the ENE signature: registers 0xA0..0xAF must read back 0x00..0x0F.

use std::ffi::c_void;
use std::mem::offset_of;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

// ---- Device -----------------------------------------------------------------

/// (pci_vendor, pci_device, subsystem_vendor, subsystem_device).
/// Deliberately exact, in the same spirit as the motherboard's hardcoded
/// 0B05:19AF — this utility targets one machine, not a hardware zoo.
const SUPPORTED: &[(u16, u16, u16, u16)] = &[
    (0x10DE, 0x2B85, 0x1043, 0x89EF), // ASUS TUF GeForce RTX 5090 Gaming (non-OC)
    (0x10DE, 0x2B85, 0x1043, 0x89EE), // ASUS TUF GeForce RTX 5090 Gaming OC
];

/// The ENE firmware string this card reports, from register 0x1000. Confirmed by
/// `jdrgb --gpu probe` on this machine's card.
///
/// OpenRGB maps this same string to the v2 color banks and reads the LED count
/// from config offset 0x03 — which is what the code below does. The write path
/// refuses any other string rather than guessing at an unverified register
/// layout, so a mismatch fails safe instead of writing garbage.
const EXPECTED_ENE_NAME: &str = "AUMA0-E6K5-1107";

// ---- ENE protocol -----------------------------------------------------------

const ENE_ADDR: u8 = 0x67;

const REG_DEVICE_NAME: u16 = 0x1000; // firmware string, 16 bytes
const REG_CONFIG_TABLE: u16 = 0x1C00; // config table, 64 bytes
const REG_DIRECT: u16 = 0x8020; // "direct access" flag — overrides the effect mode
const REG_MODE: u16 = 0x8021;
const REG_SPEED: u16 = 0x8022;
const REG_DIRECTION: u16 = 0x8023;
const REG_APPLY: u16 = 0x80A0;
const REG_COLORS_EFFECT_V2: u16 = 0x8160; // 30 bytes = 10 LEDs

const APPLY: u8 = 0x01; // commit to the live registers
const SAVE: u8 = 0xAA; // commit to non-volatile flash

const CONFIG_LED_COUNT: usize = 0x03; // offset of the LED count in the config table
const MAX_BLOCK: usize = 3; // largest block write the NVAPI SMBus path accepts
const MAX_LEDS: usize = 10; // the v2 color bank is 30 bytes

// ---- NVAPI ------------------------------------------------------------------

type NvStatus = i32;
type NvHandle = *mut c_void;

const NVAPI_OK: NvStatus = 0;

// nvapi_QueryInterface IDs, from OpenRGB's dependencies/NVFC/nvapi.cpp.
const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_ENUM_PHYSICAL_GPUS: u32 = 0xE5AC_921F;
const ID_GET_PCI_IDENTIFIERS: u32 = 0x2DDF_B66E;
const ID_I2C_WRITE_EX: u32 = 0x283A_C65A;
const ID_I2C_READ_EX: u32 = 0x4D7B_0709;

type FnQueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
type FnInitialize = unsafe extern "C" fn() -> NvStatus;
type FnEnumPhysicalGpus = unsafe extern "C" fn(*mut NvHandle, *mut i32) -> NvStatus;
type FnGetPciIdentifiers =
    unsafe extern "C" fn(NvHandle, *mut u32, *mut u32, *mut u32, *mut u32) -> NvStatus;
type FnI2cEx = unsafe extern "C" fn(NvHandle, *mut NvI2cInfoV3, *mut u32) -> NvStatus;

/// NVAPI's I2C descriptor, version 3. The layout is load-bearing and unforgiving:
/// a wrong offset silently sends the wrong bytes to real hardware, so every field
/// position is asserted at compile time below.
#[repr(C)]
struct NvI2cInfoV3 {
    version: u32,
    display_mask: u32,
    is_ddc_port: u8,
    i2c_dev_address: u8,
    i2c_reg_address: *mut u8,
    reg_addr_size: u32,
    data: *mut u8,
    size: u32,
    i2c_speed: u32,
    i2c_speed_khz: u32,
    port_id: u8,
    is_port_id_set: u32,
}

/// `NV_STRUCT_VERSION(NV_I2C_INFO_V3, 3)` — the version word encodes sizeof.
const NV_I2C_INFO_V3_VERSION: u32 = (3 << 16) | 64;

// Size alone can't catch two field errors that cancel out, so pin every offset.
const _: () = {
    assert!(size_of::<NvI2cInfoV3>() == 64);
    assert!(offset_of!(NvI2cInfoV3, version) == 0);
    assert!(offset_of!(NvI2cInfoV3, display_mask) == 4);
    assert!(offset_of!(NvI2cInfoV3, is_ddc_port) == 8);
    assert!(offset_of!(NvI2cInfoV3, i2c_dev_address) == 9);
    assert!(offset_of!(NvI2cInfoV3, i2c_reg_address) == 16);
    assert!(offset_of!(NvI2cInfoV3, reg_addr_size) == 24);
    assert!(offset_of!(NvI2cInfoV3, data) == 32);
    assert!(offset_of!(NvI2cInfoV3, size) == 40);
    assert!(offset_of!(NvI2cInfoV3, i2c_speed) == 44);
    assert!(offset_of!(NvI2cInfoV3, i2c_speed_khz) == 48);
    assert!(offset_of!(NvI2cInfoV3, port_id) == 52);
    assert!(offset_of!(NvI2cInfoV3, is_port_id_set) == 56);
};

impl NvI2cInfoV3 {
    /// A descriptor aimed at `addr` on GPU I2C port 1, where the RGB controller
    /// lives. Caller fills in the register/data pointers.
    fn new(addr: u8) -> Self {
        NvI2cInfoV3 {
            version: NV_I2C_INFO_V3_VERSION,
            display_mask: 0,
            is_ddc_port: 0,
            i2c_dev_address: addr << 1, // NVAPI wants the 8-bit form
            i2c_reg_address: std::ptr::null_mut(),
            reg_addr_size: 0,
            data: std::ptr::null_mut(),
            size: 0,
            i2c_speed: 0xFFFF, // "use i2c_speed_khz instead"
            i2c_speed_khz: 0,  // NVAPI_I2C_SPEED_DEFAULT
            port_id: 1,
            is_port_id_set: 1,
        }
    }
}

// ---- Errors -----------------------------------------------------------------

/// A failure, tagged with whether retrying could ever help. `--wait` retries
/// transient readiness problems at boot; a PCI mismatch or a bad signature will
/// never fix itself, so those fail immediately instead of burning 60 seconds.
pub struct Error {
    pub msg: String,
    pub permanent: bool,
}

impl Error {
    fn transient(msg: impl Into<String>) -> Self {
        Error { msg: msg.into(), permanent: false }
    }
    fn permanent(msg: impl Into<String>) -> Self {
        Error { msg: msg.into(), permanent: true }
    }
}

type Result<T> = std::result::Result<T, Error>;

// ---- NVAPI entry points -----------------------------------------------------

struct Nvapi {
    enum_gpus: FnEnumPhysicalGpus,
    pci_ids: FnGetPciIdentifiers,
    i2c_write: FnI2cEx,
    i2c_read: FnI2cEx,
}

fn load_nvapi() -> Result<Nvapi> {
    unsafe {
        let lib = LoadLibraryA(c"nvapi64.dll".as_ptr() as *const u8);
        if lib.is_null() {
            return Err(Error::transient("nvapi64.dll not loadable (no NVIDIA driver yet?)"));
        }

        let query = GetProcAddress(lib, c"nvapi_QueryInterface".as_ptr() as *const u8)
            .ok_or_else(|| Error::permanent("nvapi64.dll has no nvapi_QueryInterface"))?;
        let query: FnQueryInterface = std::mem::transmute(query);

        // A null here means the driver dropped an interface we depend on — that
        // is a permanent incompatibility, not a boot-time readiness problem.
        let resolve = |id: u32, what: &str| -> Result<*mut c_void> {
            let p = query(id);
            if p.is_null() {
                Err(Error::permanent(format!("NVAPI interface {what} ({id:#010X}) unavailable")))
            } else {
                Ok(p)
            }
        };

        let init: FnInitialize = std::mem::transmute(resolve(ID_INITIALIZE, "Initialize")?);
        let enum_gpus: FnEnumPhysicalGpus =
            std::mem::transmute(resolve(ID_ENUM_PHYSICAL_GPUS, "EnumPhysicalGPUs")?);
        let pci_ids: FnGetPciIdentifiers =
            std::mem::transmute(resolve(ID_GET_PCI_IDENTIFIERS, "GetPCIIdentifiers")?);
        let i2c_write: FnI2cEx = std::mem::transmute(resolve(ID_I2C_WRITE_EX, "I2CWriteEx")?);
        let i2c_read: FnI2cEx = std::mem::transmute(resolve(ID_I2C_READ_EX, "I2CReadEx")?);

        let status = init();
        if status != NVAPI_OK {
            return Err(Error::transient(format!("NvAPI_Initialize failed ({status})")));
        }

        Ok(Nvapi { enum_gpus, pci_ids, i2c_write, i2c_read })
    }
}

// ---- PCI identifiers --------------------------------------------------------

/// The four IDs NVAPI packs into two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciIds {
    pub vendor: u16,
    pub device: u16,
    pub sub_vendor: u16,
    pub sub_device: u16,
}

/// Unpack `NvAPI_GPU_GetPCIIdentifiers`' two words. Note the asymmetry: the
/// vendor is in the *low* half of each word and the device in the high half.
pub fn unpack_ids(device_id: u32, subsystem_id: u32) -> PciIds {
    PciIds {
        vendor: (device_id & 0xFFFF) as u16,
        device: (device_id >> 16) as u16,
        sub_vendor: (subsystem_id & 0xFFFF) as u16,
        sub_device: (subsystem_id >> 16) as u16,
    }
}

pub fn is_supported(ids: PciIds) -> bool {
    SUPPORTED
        .iter()
        .any(|&(v, d, sv, sd)| ids.vendor == v && ids.device == d && ids.sub_vendor == sv && ids.sub_device == sd)
}

// ---- The device -------------------------------------------------------------

pub struct Gpu {
    api: Nvapi,
    handle: NvHandle,
    pub ids: PciIds,
    /// Firmware string from 0x1000, as reported — not yet validated.
    pub ene_name: String,
    /// LED count straight from the config table, as reported — not yet bounded.
    pub raw_led_count: u8,
    config_table: [u8; 64],
}

/// Find the supported card and confirm an ENE controller is really at 0x67.
/// Read-only: this never writes an LED register.
pub fn detect() -> Result<Gpu> {
    let api = load_nvapi()?;

    let mut handles: [NvHandle; 64] = [std::ptr::null_mut(); 64];
    let mut count: i32 = 0;
    let status = unsafe { (api.enum_gpus)(handles.as_mut_ptr(), &mut count) };
    if status != NVAPI_OK {
        return Err(Error::transient(format!("NvAPI_EnumPhysicalGPUs failed ({status})")));
    }
    if count <= 0 {
        return Err(Error::transient("NVAPI reported no GPUs"));
    }

    let mut seen = Vec::new();
    for &handle in handles.iter().take(count as usize) {
        let (mut dev, mut sub, mut rev, mut ext) = (0u32, 0u32, 0u32, 0u32);
        let status = unsafe { (api.pci_ids)(handle, &mut dev, &mut sub, &mut rev, &mut ext) };
        if status != NVAPI_OK {
            return Err(Error::transient(format!("NvAPI_GPU_GetPCIIdentifiers failed ({status})")));
        }

        let ids = unpack_ids(dev, sub);
        seen.push(format!("{:04X}:{:04X}/{:04X}:{:04X}", ids.vendor, ids.device, ids.sub_vendor, ids.sub_device));
        if !is_supported(ids) {
            continue;
        }

        // An exact ID match is the gate; nothing has touched I2C yet.
        return Ok(Gpu {
            api,
            handle,
            ids,
            ene_name: String::new(),
            raw_led_count: 0,
            config_table: [0u8; 64],
        });
    }

    Err(Error::permanent(format!(
        "no supported ASUS GPU found (saw {})",
        if seen.is_empty() { "nothing".to_string() } else { seen.join(", ") }
    )))
}

impl Gpu {
    /// Raw read of the signature window, 0xA0..0xAF. On an ENE controller these
    /// read back 0x00..0x0F. Returned unjudged so the probe can show what's
    /// actually there.
    pub fn signature(&self) -> Result<[u8; 16]> {
        // Prime the bus and discard the result. A cold I2C bus hands back the
        // previous transaction's data on the first read — observed here as 0x11
        // where 0x00 was expected, then correct on every subsequent run. OpenRGB
        // does the same throwaway read before its signature loop. This matters
        // most at boot, where the bus is always cold.
        let _ = self.read_byte_data(0xA0);

        let mut out = [0u8; 16];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.read_byte_data(0xA0 + i as u8)?;
        }
        Ok(out)
    }

    pub fn signature_ok(sig: &[u8; 16]) -> bool {
        sig.iter().enumerate().all(|(i, &b)| b as usize == i)
    }

    /// Read the firmware string and config table. Only meaningful once the
    /// signature has confirmed an ENE controller is answering.
    pub fn read_identity(&mut self) -> Result<()> {
        self.ene_name = self.read_name()?;
        for i in 0..64 {
            self.config_table[i] = self.ene_read(REG_CONFIG_TABLE + i as u16)?;
        }
        self.raw_led_count = self.config_table[CONFIG_LED_COUNT];
        Ok(())
    }

    fn read_name(&self) -> Result<String> {
        let mut buf = [0u8; 16];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.ene_read(REG_DEVICE_NAME + i as u16)?;
        }
        Ok(buf.iter().take_while(|&&c| c != 0).filter(|c| c.is_ascii_graphic()).map(|&c| c as char).collect())
    }

    /// Full readiness check for any write path: confirm an ENE controller really
    /// is answering at 0x67, learn what it is, and make sure it looks sane.
    /// Returns the validated LED count.
    pub fn prepare(&mut self) -> Result<usize> {
        let sig = self.signature()?;
        if !Self::signature_ok(&sig) {
            let hex: Vec<String> = sig.iter().map(|b| format!("{b:02X}")).collect();
            return Err(Error::permanent(format!(
                "no ENE controller at {ENE_ADDR:#04X}: registers A0..AF read [{}], expected [00 01 .. 0F]",
                hex.join(" ")
            )));
        }
        self.read_identity()?;
        self.validate()
    }

    /// Gate the write path: the firmware string must be the one we've verified,
    /// and the LED count must fit the 30-byte color bank. A corrupted read that
    /// claimed 200 LEDs would otherwise write far past the end of the bank.
    pub fn validate(&self) -> Result<usize> {
        if self.ene_name != EXPECTED_ENE_NAME {
            return Err(Error::permanent(format!(
                "unrecognized ENE firmware '{}' (expected '{EXPECTED_ENE_NAME}') — \
                 register layout unverified, refusing to write",
                self.ene_name
            )));
        }
        let n = self.raw_led_count as usize;
        if n == 0 || n > MAX_LEDS {
            return Err(Error::permanent(format!(
                "implausible LED count {n} from the config table (expected 1..={MAX_LEDS})"
            )));
        }
        Ok(n)
    }

    pub fn label(&self) -> String {
        format!("ASUS GPU {:04X}:{:04X} ({})", self.ids.device, self.ids.sub_device, self.ene_name)
    }

    pub fn config_table(&self) -> &[u8; 64] {
        &self.config_table
    }

    /// Paint every LED one color in static mode.
    ///
    /// The final two writes are not optional: a stale "direct" flag overrides the
    /// effect mode entirely, so the colors would land but never show.
    pub fn apply_solid(&self, mode: u8, color: (u8, u8, u8)) -> Result<()> {
        let led_count = self.validate()?;
        let buf = pack_colors(&vec![color; led_count]);

        let mut sent = 0;
        while sent < buf.len() {
            let n = (buf.len() - sent).min(MAX_BLOCK);
            self.ene_write_block(REG_COLORS_EFFECT_V2 + sent as u16, &buf[sent..sent + n])?;
            sent += n;
        }

        self.ene_write(REG_MODE, mode)?;
        self.ene_write(REG_SPEED, 0)?;
        self.ene_write(REG_DIRECTION, 0)?;
        self.ene_write(REG_APPLY, APPLY)?;
        self.ene_write(REG_DIRECT, 0)?;
        self.ene_write(REG_APPLY, APPLY)?;
        Ok(())
    }

    /// Commit whatever is currently live to the controller's flash.
    ///
    /// Deliberately separate from `apply_solid` and never called automatically:
    /// flash has finite write cycles, so this belongs in a deliberate one-shot
    /// command, not in anything that runs at boot.
    pub fn save_to_flash(&self) -> Result<()> {
        self.ene_write(REG_APPLY, SAVE)
    }

    // ---- ENE register layer -------------------------------------------------
    //
    // ENE's 16-bit registers are tunneled over 8-bit SMBus: select the register
    // via command 0x00, then read via 0x81 / write via 0x01 / block-write via
    // 0x03.

    fn ene_select(&self, reg: u16) -> Result<()> {
        // Byte-swapped so the register goes out high byte first.
        self.write_word_data(0x00, reg.swap_bytes())
    }

    fn ene_read(&self, reg: u16) -> Result<u8> {
        self.ene_select(reg)?;
        self.read_byte_data(0x81)
    }

    fn ene_write(&self, reg: u16, val: u8) -> Result<()> {
        self.ene_select(reg)?;
        self.write_byte_data(0x01, val)
    }

    fn ene_write_block(&self, reg: u16, data: &[u8]) -> Result<()> {
        self.ene_select(reg)?;
        self.write_block_data(0x03, data)
    }

    // ---- SMBus over NVAPI ---------------------------------------------------

    fn read_byte_data(&self, cmd: u8) -> Result<u8> {
        let mut data = [0u8; 1];
        self.smbus(false, cmd, &mut data)?;
        Ok(data[0])
    }

    fn write_byte_data(&self, cmd: u8, val: u8) -> Result<()> {
        let mut data = [val];
        self.smbus(true, cmd, &mut data)
    }

    fn write_word_data(&self, cmd: u8, word: u16) -> Result<()> {
        let mut data = [(word & 0xFF) as u8, (word >> 8) as u8];
        self.smbus(true, cmd, &mut data)
    }

    fn write_block_data(&self, cmd: u8, block: &[u8]) -> Result<()> {
        let mut data = Vec::with_capacity(block.len() + 1);
        data.push(block.len() as u8);
        data.extend_from_slice(block);
        self.smbus(true, cmd, &mut data)
    }

    /// One SMBus transaction: `cmd` is the register address byte, `data` the
    /// payload (filled in on read).
    ///
    /// `reg` and `info` are locals that outlive the call, so the pointers NVAPI
    /// receives stay valid for its duration.
    fn smbus(&self, write: bool, cmd: u8, data: &mut [u8]) -> Result<()> {
        let mut reg = [cmd];
        let mut info = NvI2cInfoV3::new(ENE_ADDR);
        info.i2c_reg_address = reg.as_mut_ptr();
        info.reg_addr_size = 1;
        info.data = data.as_mut_ptr();
        info.size = data.len() as u32;

        let mut unknown = 0u32;
        let status = unsafe {
            if write {
                (self.api.i2c_write)(self.handle, &mut info, &mut unknown)
            } else {
                (self.api.i2c_read)(self.handle, &mut info, &mut unknown)
            }
        };

        if status != NVAPI_OK {
            let op = if write { "write" } else { "read" };
            return Err(Error::transient(format!("NVAPI I2C {op} failed ({status}) at command {cmd:#04X}")));
        }
        Ok(())
    }
}

/// Flatten colors into the ENE effect bank's byte order, which is **R, B, G** —
/// not RGB. Getting this wrong swaps green and blue.
pub fn pack_colors(colors: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(colors.len() * 3);
    for &(r, g, b) in colors {
        out.push(r);
        out.push(b);
        out.push(g);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_this_machines_ids() {
        // From `PCI\VEN_10DE&DEV_2B85&SUBSYS_89EF1043`: NVAPI packs the vendor
        // in the low half and the device in the high half of each word.
        let ids = unpack_ids(0x2B85_10DE, 0x89EF_1043);
        assert_eq!(ids, PciIds { vendor: 0x10DE, device: 0x2B85, sub_vendor: 0x1043, sub_device: 0x89EF });
    }

    #[test]
    fn accepts_both_tuf_5090_variants() {
        assert!(is_supported(unpack_ids(0x2B85_10DE, 0x89EF_1043))); // non-OC
        assert!(is_supported(unpack_ids(0x2B85_10DE, 0x89EE_1043))); // OC
    }

    #[test]
    fn rejects_other_cards() {
        // Right card, wrong brand.
        assert!(!is_supported(unpack_ids(0x2B85_10DE, 0x1234_1458)));
        // ASUS, but a different GPU.
        assert!(!is_supported(unpack_ids(0x2704_10DE, 0x89EF_1043)));
        // Not NVIDIA at all.
        assert!(!is_supported(unpack_ids(0x13C0_1002, 0x8877_1043)));
    }

    #[test]
    fn packs_colors_as_r_b_g() {
        // Pure red must stay in byte 0; pure green must land in byte 2.
        assert_eq!(pack_colors(&[(0xFF, 0x00, 0x00)]), vec![0xFF, 0x00, 0x00]);
        assert_eq!(pack_colors(&[(0x00, 0xFF, 0x00)]), vec![0x00, 0x00, 0xFF]);
        assert_eq!(pack_colors(&[(0x00, 0x00, 0xFF)]), vec![0x00, 0xFF, 0x00]);
        assert_eq!(pack_colors(&[(0x11, 0x22, 0x33)]), vec![0x11, 0x33, 0x22]);
    }

    #[test]
    fn packs_every_led() {
        assert_eq!(pack_colors(&[(1, 2, 3); 4]).len(), 12);
        assert!(pack_colors(&[]).is_empty());
    }

    #[test]
    fn color_bank_holds_max_leds() {
        // 30-byte bank; MAX_LEDS must not let us write past it.
        assert_eq!(MAX_LEDS * 3, 30);
    }
}
