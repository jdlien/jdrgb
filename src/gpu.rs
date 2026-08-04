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
use std::cell::Cell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExA, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

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

// ---- Keeping off the bus ----------------------------------------------------

/// Minimum gap between GPU LED writes, machine-wide.
///
/// `docs/ups-wedge-incident.md` records the observation this is built on: two
/// writes *seconds apart* visibly glitched the monitor for a few seconds, while
/// a single write, and writes minutes apart, did not. The card's RGB controller
/// hangs off the GPU's own I2C block, which is display plumbing, so that is not
/// a surprising place to find interference.
///
/// The number is a guess constrained by one observation — the honest thing to
/// say is that "seconds apart" was bad and this is comfortably longer than a
/// single write takes. `JDRGB_GPU_GAP_MS` overrides it; `0` disables the wait.
const DEFAULT_WRITE_GAP: Duration = Duration::from_millis(1500);

/// Minimum gap between repaints *within one session* — `tune` and `preview`,
/// which hold a single `Gpu` and paint repeatedly.
///
/// The full gap is wrong here. It exists to separate one program's burst from
/// the next program's, and applying it per keypress would make `tune --gpu`
/// take a second and a half to answer an arrow key. But a held key repeats
/// around 30 times a second, and letting that through unthrottled is the
/// "live tuning at speed" the incident write-up warned about, so there is still
/// a floor — just a human-scaled one.
const LIVE_WRITE_GAP: Duration = Duration::from_millis(100);

/// How long to wait for another process to finish with the bus before going
/// ahead anyway. A lock that can't be taken must not leave the lights wrong.
const LOCK_TIMEOUT_MS: u32 = 10_000;

fn write_gap() -> Duration {
    match std::env::var("JDRGB_GPU_GAP_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) => Duration::from_millis(ms),
        None => DEFAULT_WRITE_GAP,
    }
}

/// Where the time of the last GPU write is recorded.
///
/// Machine-wide rather than per-user on purpose: the boot task runs as SYSTEM
/// and a tray click runs as the logged-in user, and the whole point is that
/// those two see each other.
fn gap_marker() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("ProgramData")?).join("jdrgb");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("gpu-last-write"))
}

/// Milliseconds since the epoch, as text, written into the marker file.
///
/// The time goes in the file's *contents* rather than being read off its mtime,
/// which is what this did first and why it silently did nothing: writing zero
/// bytes doesn't update the last-write timestamp on Windows, so the marker
/// always looked ancient and the gap never applied.
fn now_ms() -> Option<u128> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis())
}

fn mark_write() {
    if let (Some(path), Some(ms)) = (gap_marker(), now_ms()) {
        let _ = std::fs::write(&path, ms.to_string());
    }
}

/// Serialises GPU I2C across every process on the machine.
///
/// Two reasons, one confirmed and one observed:
///
///   * The ENE register protocol is stateful. `ene_select` puts a register
///     address on the bus in one transaction and the read or write that follows
///     is a *separate* one, so two jdrgb processes can interleave and have one
///     read back the register the other just selected. The LED count that gates
///     every write comes through exactly that path.
///   * Writes close together disturb the display (see `DEFAULT_WRITE_GAP`).
///
/// Nothing else serialises this. The tray queues its own clicks, but a scheduled
/// task, a tray click and a hand-run command are three processes that have never
/// heard of each other.
///
/// One name, no fallback. An earlier version tried `Global\` and fell back to
/// `Local\`, which is worse than having no lock: two processes that end up on
/// different objects both believe they hold the bus. If the named mutex cannot
/// be created at all we say so and go ahead unserialised, which is at least
/// honest about what is happening.
///
/// The remaining gap is cross-account, and it is a DACL problem rather than a
/// privilege one — creating a *mutex* in the global namespace needs no special
/// privilege (`SeCreateGlobalPrivilege` covers file-mapping and symbolic-link
/// objects only). But an object created by the SYSTEM boot task gets that
/// token's default DACL, and a later `CreateMutexW` from an ordinary user asks
/// for `MUTEX_ALL_ACCESS`, which that DACL may refuse. Serialisation between the
/// boot task and a logged-in user is therefore not guaranteed; between processes
/// of the same user — the tray, a hook, a hand-run command, an agent in a loop —
/// it is.
const BUS_MUTEX: &str = "Global\\jdrgb-gpu-i2c";

struct BusLock(HANDLE);

impl BusLock {
    /// `Ok(Some)` — held. `Ok(None)` — no lock could be created, proceeding
    /// unserialised. `Err` — someone else has it.
    ///
    /// Being busy is a *transient* error rather than a silent free pass. The
    /// earlier version gave up after the timeout and carried on without the
    /// lock, which meant a long-lived holder — `tune` and `preview` keep one for
    /// the whole interactive session — turned every competing command into
    /// exactly the interleaved-select race the lock exists to prevent. Failing
    /// makes `--wait` retry and tells everyone else what happened.
    fn acquire() -> Result<Option<BusLock>> {
        let wide: Vec<u16> = BUS_MUTEX.encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if h.is_null() {
            return Ok(None);
        }
        // WAIT_ABANDONED means the last holder died without releasing — the
        // mutex is ours now, and there is nothing to repair on our side.
        match unsafe { WaitForSingleObject(h, LOCK_TIMEOUT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(BusLock(h))),
            _ => {
                unsafe { CloseHandle(h) };
                Err(Error::transient(
                    "another jdrgb is using the GPU's I2C bus (a `tune` or `preview` session holds \
                     it until it exits)",
                ))
            }
        }
    }
}

impl Drop for BusLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

// ---- NVAPI entry points -----------------------------------------------------

struct Nvapi {
    enum_gpus: FnEnumPhysicalGpus,
    pci_ids: FnGetPciIdentifiers,
    i2c_write: FnI2cEx,
    i2c_read: FnI2cEx,
}

fn load_nvapi() -> Result<Nvapi> {
    unsafe {
        // System32 only, never the search path. A bare LoadLibrary searches the
        // directory the .exe lives in first, so anyone who could drop a
        // nvapi64.dll next to jdrgb.exe would get their code run — and the
        // installed copy runs as SYSTEM from the scheduled task. The real
        // nvapi64.dll is installed to System32 by the NVIDIA driver, so nothing
        // legitimate is lost by refusing to look anywhere else.
        let lib = LoadLibraryExA(
            c"nvapi64.dll".as_ptr() as *const u8,
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if lib.is_null() {
            return Err(Error::transient("nvapi64.dll not loadable from System32 (no NVIDIA driver yet?)"));
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
    /// Held for as long as this handle lives, so every transaction it makes is
    /// serialised against other processes. `None` when the lock couldn't be
    /// taken — see `BusLock`, which is deliberately best-effort.
    _lock: Option<BusLock>,
    /// When this handle last wrote LEDs, for the in-session throttle. `None`
    /// until the first write, which is what makes that write consult the
    /// machine-wide marker instead.
    last_write: Cell<Option<Instant>>,
}

/// Find the supported card and confirm an ENE controller is really at 0x67.
/// Read-only: this never writes an LED register.
pub fn detect() -> Result<Gpu> {
    if crate::hardware_disabled() {
        return Err(Error::permanent(crate::NO_HW_MESSAGE));
    }

    // Taken before NVAPI is even loaded, and held until this `Gpu` is dropped,
    // so a whole detect/prepare/apply sequence is one atomic turn on the bus.
    let lock = BusLock::acquire()?;

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
            _lock: lock,
            last_write: Cell::new(None),
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

    /// Read the firmware string and the whole config table. Only meaningful once
    /// the signature has confirmed an ENE controller is answering.
    ///
    /// The full table is for `probe`, which prints it. Everything else wants one
    /// byte of it and should call `prepare`, which reads one byte of it — see the
    /// note there on why that is worth splitting.
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
    ///
    /// Reads the one config byte it needs rather than the whole table. Each ENE
    /// register read is two NVAPI I2C transactions — a select and an access — so
    /// the 64-byte table cost 128 of them to use one, and this path runs on every
    /// single write. Dropping it takes a `jdrgb --gpu COLOR` from roughly 209
    /// transactions to 83. That matters here specifically: the ENE controller
    /// sits on the graphics card's own I2C block, and writes close together are
    /// what glitched the monitor in `docs/ups-wedge-incident.md`. `probe` still
    /// reads the full table through `read_identity`, because it prints it.
    pub fn prepare(&mut self) -> Result<usize> {
        let sig = self.signature()?;
        if !Self::signature_ok(&sig) {
            let hex: Vec<String> = sig.iter().map(|b| format!("{b:02X}")).collect();
            return Err(Error::permanent(format!(
                "no ENE controller at {ENE_ADDR:#04X}: registers A0..AF read [{}], expected [00 01 .. 0F]",
                hex.join(" ")
            )));
        }
        self.ene_name = self.read_name()?;
        self.raw_led_count = self.ene_read(REG_CONFIG_TABLE + CONFIG_LED_COUNT as u16)?;
        self.config_table[CONFIG_LED_COUNT] = self.raw_led_count;
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

    /// Current mode and direct-flag registers, for diagnostics. Mode 0 is the
    /// controller's own off state; 1 is static.
    pub fn mode_state(&self) -> Result<(u8, u8)> {
        Ok((self.ene_read(REG_MODE)?, self.ene_read(REG_DIRECT)?))
    }

    /// Read back the colors currently in the effect bank — what the LEDs are
    /// actually showing, straight from the chip rather than from what we think
    /// we last sent. Same R, B, G byte order as the write path.
    pub fn effect_colors(&self) -> Result<Vec<(u8, u8, u8)>> {
        let mut out = Vec::new();
        for led in 0..self.raw_led_count as usize {
            let base = REG_COLORS_EFFECT_V2 + (led * 3) as u16;
            let r = self.ene_read(base)?;
            let b = self.ene_read(base + 1)?;
            let g = self.ene_read(base + 2)?;
            out.push((r, g, b));
        }
        Ok(out)
    }

    /// Paint every LED one color in static mode.
    pub fn apply_solid(&self, mode: u8, color: (u8, u8, u8)) -> Result<()> {
        self.apply_colors(mode, &[color])
    }

    /// Paint the LEDs individually in static mode. The effect bank holds three
    /// bytes per LED, and ENE static mode honours them per-LED, so the card's
    /// LEDs can each show a different color.
    ///
    /// Fewer colors than LEDs cycle to fill; more are ignored.
    ///
    /// Photographed on the TUF 5090 with red/green/blue/white on LEDs 0-3. The
    /// shroud does show four distinguishable regions, but they are *not* four
    /// isolated zones: the TUF logo rendered a smooth green-to-blue gradient
    /// rather than two flat halves, and both left tick marks took one color while
    /// both right ticks took another. That reads as four point sources behind one
    /// continuous light guide, each feature picking up whichever lamp is nearest,
    /// rather than a per-feature mapping.
    ///
    /// Which lamp sits where is not established — inferring it would need one LED
    /// lit at a time. The LEDs are not on the main PCB (a back-side teardown shows
    /// only status LEDs and what looks like the harness header), so the layout
    /// can't be read off the board either.
    ///
    /// The bleed is severe: pure `FF0000` renders pink and pure `00FF00` renders
    /// teal, despite neither having any blue in it. Saturated per-LED patterns go
    /// muddy; soft gradients between neighbouring hues are what this actually
    /// looks good doing, the blending working in your favour.
    ///
    /// The final two writes are not optional: a stale "direct" flag overrides the
    /// effect mode entirely, so the colors would land but never show.
    pub fn apply_colors(&self, mode: u8, colors: &[(u8, u8, u8)]) -> Result<()> {
        let led_count = self.validate()?;
        if colors.is_empty() {
            return Err(Error::permanent("no colors given"));
        }
        let per_led: Vec<(u8, u8, u8)> = (0..led_count).map(|i| colors[i % colors.len()]).collect();
        let buf = pack_colors(&per_led);

        // Inside the bus lock, so the check and the write can't be raced.
        self.space_out_writes();
        // Stamped before the burst as well as after it. Every `?` below leaves
        // without reaching the closing stamp, and the error path is precisely
        // when the bus is already unhappy — a `--wait` retry 500ms later was the
        // one case that escaped the gap entirely.
        self.note_write();

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

        self.note_write();
        Ok(())
    }

    /// Record that the bus was just used, here and machine-wide.
    fn note_write(&self) {
        self.last_write.set(Some(Instant::now()));
        mark_write();
    }

    /// Wait until enough time has passed since the last GPU write anywhere on
    /// the machine. See `DEFAULT_WRITE_GAP` for why this exists.
    ///
    /// Every failure here is ignored on purpose — no ProgramData, no marker
    /// file, an unreadable timestamp, a clock that went backwards. This is a
    /// courtesy to the display, and none of those are worth refusing to set the
    /// lights over.
    fn space_out_writes(&self) {
        let gap = write_gap();
        if gap.is_zero() {
            return;
        }

        // Second and later paints of one live session: this handle already owns
        // the bus lock, so nobody else is waiting, and the only question is
        // whether we are painting faster than a display can take.
        //
        // `min` so that `JDRGB_GPU_GAP_MS` can only ever make this *less*
        // permissive than it already is — someone lowering the gap to 50ms
        // should not find live repaints still pinned at 100.
        if let Some(last) = self.last_write.get() {
            if let Some(remaining) = LIVE_WRITE_GAP.min(gap).checked_sub(last.elapsed()) {
                std::thread::sleep(remaining);
            }
            return;
        }

        let Some(path) = gap_marker() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let (Ok(last), Some(now)) = (text.trim().parse::<u128>(), now_ms()) else { return };
        // A clock that moved backwards leaves `now` behind `last`. Waiting out a
        // gap computed from that could mean sleeping for days, so don't.
        let Some(since) = now.checked_sub(last) else { return };
        if let Some(remaining) = gap.checked_sub(Duration::from_millis(since as u64)) {
            std::thread::sleep(remaining);
        }
    }

    /// Commit whatever is currently live to the controller's flash.
    ///
    /// Confirmed working on this card, by a test designed to rule out the card
    /// simply never losing power: the LEDs were set to green *without* saving, so
    /// flash held warm white while the live registers held green. After a full
    /// shutdown with the PSU switched off for 30 seconds it came back warm white,
    /// with no boot task installed and no vendor software present. The green was
    /// gone, so power really was lost; the warm white came from flash.
    ///
    /// Worth recording, because the common claim is that ASUS GPUs always revert
    /// to their firmware rainbow on power loss — this one doesn't, so Armoury
    /// Crate reasserting it every boot was never necessary.
    ///
    /// Deliberately separate from `apply_solid` and never called automatically:
    /// flash has finite write cycles, so this belongs in a deliberate one-shot
    /// command, not in anything that runs at boot.
    pub fn save_to_flash(&self) -> Result<()> {
        // Paced like any other write. It is one transaction rather than a burst,
        // but it is still traffic on the card's I2C block, and it typically
        // follows an apply by a second or two — exactly the spacing that caused
        // trouble.
        self.space_out_writes();
        self.note_write();
        let out = self.ene_write(REG_APPLY, SAVE);
        self.note_write();
        out
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
