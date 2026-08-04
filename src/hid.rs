//! Just enough Windows HID to talk to one known device — and nothing else.
//!
//! This module exists because of `docs/ups-wedge-incident.md`. The short version:
//! `hidapi`'s `HidApi::new()` enumerates the whole HID bus, and on Windows
//! enumeration is not passive — `hid_enumerate` opens every HID interface with
//! `CreateFileW` and asks the matching ones for their string descriptors, which
//! are control transfers on the device's default endpoint. Doing that to an APC
//! Back-UPS at the instant mains returned deadlocked its firmware until the cable
//! was physically replugged, twice.
//!
//! The obvious fix does not work. `hid_enumerate`'s VID/PID argument is applied
//! *after* every device is already open, and `HidApi::new()` calls
//! `add_devices(0, 0)` unconditionally (hidapi 2.6.6 `src/lib.rs:190`) — so no
//! argument to that crate avoids the sweep, and `disable_device_discovery()` is a
//! no-op on Windows, where it is only read under `#[cfg(libusb)]`.
//!
//! So the sweep has to go, not be filtered. The path here is:
//!
//!   1. `interfaces()` asks the configuration manager for the HID interface list
//!      and matches the VID/PID **in the path text**. It opens nothing, and puts
//!      nothing on any wire.
//!   2. `Interface::open()` calls `CreateFileW` on exactly one path.
//!
//! A device that is not the Aura controller is therefore never opened, never
//! interrogated, and never sent a single byte.

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    GUID_DEVINTERFACE_HID, HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS,
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetPreparsedData, HidD_SetNumInputBuffers,
    HidP_GetCaps, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

/// How long a single report write may take before we give up on it. Generous:
/// the controller answers in about 4ms, and a stall here means something is
/// wrong rather than slow.
const WRITE_TIMEOUT_MS: u32 = 1000;

// ---------------------------------------------------------------------------
// Discovery — reads the PnP database, opens nothing
// ---------------------------------------------------------------------------

/// One HID interface, named but not opened.
pub struct Interface {
    /// NUL-terminated wide path, ready for `CreateFileW`.
    path: Vec<u16>,
    /// The same path as text, for diagnostics.
    text: String,
    /// What the path claimed the IDs were, re-checked against the device itself
    /// once it is open. See `Interface::open`.
    want: (u16, u16),
}

impl Interface {
    /// The `MI_xx` field of the path, which for a USB composite device is the
    /// interface number. `None` if the path has no such field.
    ///
    /// Taken from the path rather than from the device because that is free —
    /// the alternative, `HidD_GetAttributes`, needs an open handle.
    ///
    /// Case-insensitive because Windows hands these back in either case: this
    /// machine's controller arrives as `...&MI_02#...` while the same field is
    /// conventionally written lowercase elsewhere.
    pub fn interface_number(&self) -> Option<u8> {
        let lower = self.text.to_ascii_lowercase();
        let at = lower.find("&mi_")? + 4;
        u8::from_str_radix(lower[at..].get(..2)?, 16).ok()
    }

    pub fn path(&self) -> &str {
        &self.text
    }
}

/// Every present HID interface whose path names this VID and PID.
///
/// `CM_Get_Device_Interface_ListW` returns the configuration manager's own
/// records — a NUL-separated, double-NUL-terminated block of paths that look
/// like `\\?\HID#VID_0B05&PID_19AF&MI_02#7&...#{4d1e55b2-...}`. Matching the IDs
/// as text is what lets this answer "which interfaces are the controller's?"
/// without opening anything.
///
/// Bluetooth HID paths don't carry `VID_`/`PID_` fields in this form, so they
/// simply never match — which is correct here, the Aura controller is USB.
pub fn interfaces(vid: u16, pid: u16) -> Result<Vec<Interface>, String> {
    let needle = format!("vid_{vid:04x}&pid_{pid:04x}");

    // Sizing and fetching are two calls, so the list can grow in between. That
    // is what CR_BUFFER_SMALL reports, and retrying is the documented answer.
    for _ in 0..4 {
        let mut len: u32 = 0;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &GUID_DEVINTERFACE_HID,
                std::ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr != CR_SUCCESS {
            return Err(format!("could not size the HID interface list (CONFIGRET {cr})"));
        }

        let mut buf = vec![0u16; len as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &GUID_DEVINTERFACE_HID,
                std::ptr::null(),
                buf.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(format!("could not read the HID interface list (CONFIGRET {cr})"));
        }

        return Ok(buf
            .split(|&c| c == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| {
                let text = String::from_utf16_lossy(p);
                text.to_ascii_lowercase().contains(&needle).then(|| Interface {
                    path: p.iter().copied().chain(std::iter::once(0)).collect(),
                    text,
                    want: (vid, pid),
                })
            })
            .collect());
    }
    Err("the HID interface list kept changing while it was being read".into())
}

// ---------------------------------------------------------------------------
// The open device
// ---------------------------------------------------------------------------

/// An open HID interface. Closes itself on drop.
pub struct Device {
    handle: HANDLE,
    /// One manual-reset event, reused by every transfer. The `OVERLAPPED` that
    /// points at it is a local in each call, so nothing outlives an in-flight
    /// transfer — see the cancel path in `transfer`.
    event: HANDLE,
    /// `OutputReportByteLength` as the device reports it. 0 if the driver
    /// wouldn't say, in which case `write` sends what it is given.
    output_report_len: usize,
}

impl Interface {
    /// Open this one interface for reading and writing.
    ///
    /// Shared read/write, like every other HID client: taking exclusive access
    /// would lock out anything else that legitimately talks to the controller.
    ///
    /// The VID and PID are re-read from the device with `HidD_GetAttributes` and
    /// checked before this returns. Selecting the device by its path text is
    /// what avoids opening anything else, but Windows documents these symbolic
    /// links as opaque and a substring match is not a parse — so the string
    /// decides what to *open*, and the device itself decides what we are willing
    /// to *write to*. The check costs one local call on a handle we already
    /// hold, and it is free of the hazard this module exists to avoid, because
    /// by this point the device is ours.
    pub fn open(&self) -> Result<Device, String> {
        unsafe {
            let handle = CreateFileW(
                self.path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(format!("could not open HID interface (error {})", GetLastError()));
            }

            let mut attrs: HIDD_ATTRIBUTES = std::mem::zeroed();
            attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
            let got = HidD_GetAttributes(handle, &mut attrs)
                .then_some((attrs.VendorID, attrs.ProductID));
            if got != Some(self.want) {
                CloseHandle(handle);
                let (wv, wp) = self.want;
                return Err(match got {
                    Some((v, p)) => format!(
                        "{} is {v:04X}:{p:04X}, not the {wv:04X}:{wp:04X} its path claimed",
                        self.text
                    ),
                    None => format!("{} would not report its VID/PID", self.text),
                });
            }

            // Without this the driver keeps a small input queue and drops
            // reports that arrive while we're not reading. hidapi does the same.
            HidD_SetNumInputBuffers(handle, 64);

            let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if event.is_null() {
                let e = GetLastError();
                CloseHandle(handle);
                return Err(format!("could not create an IO event (error {e})"));
            }

            let mut caps: HIDP_CAPS = std::mem::zeroed();
            let mut pp: PHIDP_PREPARSED_DATA = 0;
            if HidD_GetPreparsedData(handle, &mut pp) {
                HidP_GetCaps(pp, &mut caps);
                HidD_FreePreparsedData(pp);
            }

            Ok(Device {
                handle,
                event,
                output_report_len: caps.OutputReportByteLength as usize,
            })
        }
    }
}

impl Device {
    /// Usage page and usage, for diagnostics. `None` if the driver won't say.
    pub fn usage(&self) -> Option<(u16, u16)> {
        unsafe {
            let mut pp: PHIDP_PREPARSED_DATA = 0;
            if !HidD_GetPreparsedData(self.handle, &mut pp) {
                return None;
            }
            let mut caps: HIDP_CAPS = std::mem::zeroed();
            let ok = HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS;
            HidD_FreePreparsedData(pp);
            ok.then_some((caps.UsagePage, caps.Usage))
        }
    }

    /// Send one output report. `buf[0]` is the report ID.
    ///
    /// Windows wants a write of exactly `OutputReportByteLength` and fails with
    /// `ERROR_INVALID_PARAMETER` otherwise, so a buffer of a different size is
    /// padded or truncated to fit. hidapi made the same accommodation, and
    /// dropping it would have made this code work only on a device whose reports
    /// happen to be 65 bytes.
    pub fn write(&self, buf: &[u8]) -> Result<(), String> {
        let want = self.output_report_len;
        // Truncating would put a malformed Aura command on the wire. A device
        // claiming a report shorter than the packet is not a device we
        // understand — more likely the wrong interface or unexpected firmware —
        // so fail closed rather than send a prefix and hope.
        if want != 0 && want < buf.len() {
            return Err(format!(
                "device wants {want}-byte output reports but this command is {} bytes \
                 — refusing to send a truncated packet",
                buf.len()
            ));
        }
        let outcome = if want == 0 || buf.len() == want {
            self.transfer(Op::Write(buf), WRITE_TIMEOUT_MS)?
        } else {
            let mut sized = vec![0u8; want];
            sized[..buf.len()].copy_from_slice(buf);
            self.transfer(Op::Write(&sized), WRITE_TIMEOUT_MS)?
        };
        match outcome {
            Outcome::Done(_) => Ok(()),
            Outcome::TimedOut => Err(format!("HID write timed out after {WRITE_TIMEOUT_MS}ms")),
        }
    }

    /// Read one input report, giving up after `timeout_ms`.
    ///
    /// A timeout is `Ok(0)`, not an error: probing which of several interfaces
    /// answers a request is done by asking each one and seeing which replies, so
    /// silence is an expected answer rather than a fault.
    pub fn read_timeout(&self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, String> {
        match self.transfer(Op::Read(buf), timeout_ms)? {
            Outcome::Done(n) => Ok(n),
            Outcome::TimedOut => Ok(0),
        }
    }

    /// One overlapped transfer, started and finished inside this call.
    ///
    /// The `OVERLAPPED` lives on this stack frame, so the one thing that must
    /// never happen is returning while the kernel could still write to it. On a
    /// timeout that means `CancelIoEx` *and then* a blocking `GetOverlappedResult`
    /// — cancellation is a request, not a guarantee, and the IO is only certainly
    /// finished once that returns.
    fn transfer(&self, op: Op, timeout_ms: u32) -> Result<Outcome, String> {
        unsafe {
            ResetEvent(self.event);

            let mut ol: OVERLAPPED = std::mem::zeroed();
            ol.hEvent = self.event;

            // Named before the match, which consumes `op` — a read needs its
            // buffer by `&mut`, and casting a `*const` to `*mut` to avoid that
            // would be writing through a shared reference.
            let name = op.name();
            let started = match op {
                Op::Write(buf) => WriteFile(
                    self.handle,
                    buf.as_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut ol,
                ),
                Op::Read(buf) => ReadFile(
                    self.handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut ol,
                ),
            };

            if started == 0 {
                let err = GetLastError();
                if err != ERROR_IO_PENDING {
                    return Err(format!("{name} failed (error {err})"));
                }
            }

            let mut moved: u32 = 0;
            if WaitForSingleObject(self.event, timeout_ms) != WAIT_OBJECT_0 {
                CancelIoEx(self.handle, &ol);
                // Blocking, deliberately, and not optional: cancellation is a
                // request, not a guarantee. Until this returns the kernel may
                // still write to `ol` and to the caller's buffer, and both die
                // with this stack frame.
                let completed = GetOverlappedResult(self.handle, &ol, &mut moved, 1);

                // Three outcomes are possible here and they are not the same.
                // The transfer may have finished normally in the window between
                // the wait expiring and the cancel landing — reporting that as a
                // timeout would mean re-sending a write that already went out,
                // or discarding a reply that did arrive and so misidentifying
                // which interface is the control one.
                return if completed != 0 {
                    Ok(Outcome::Done(moved as usize))
                } else if GetLastError() == ERROR_OPERATION_ABORTED {
                    Ok(Outcome::TimedOut)
                } else {
                    Err(format!("{name} failed (error {})", GetLastError()))
                };
            }
            if GetOverlappedResult(self.handle, &ol, &mut moved, 1) == 0 {
                return Err(format!("{name} failed (error {})", GetLastError()));
            }
            Ok(Outcome::Done(moved as usize))
        }
    }
}

/// A transfer that finished, or one that ran out of time. A timeout is not an
/// error here because the caller decides what it means — for a read it is how
/// "this interface isn't the one" is discovered, for a write it is a fault.
enum Outcome {
    Done(usize),
    TimedOut,
}

enum Op<'a> {
    Write(&'a [u8]),
    Read(&'a mut [u8]),
}

impl Op<'_> {
    fn name(&self) -> &'static str {
        match self {
            Op::Write(_) => "HID write",
            Op::Read(_) => "HID read",
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The path filter is the whole safety property of this module: it is what
    /// decides which devices get opened at all. Lowercase and uppercase paths
    /// both occur in the wild.
    fn matches(path: &str, vid: u16, pid: u16) -> bool {
        path.to_ascii_lowercase().contains(&format!("vid_{vid:04x}&pid_{pid:04x}"))
    }

    #[test]
    fn matches_the_aura_controller_in_either_case() {
        let lower = r"\\?\hid#vid_0b05&pid_19af&mi_02#7&abc&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let upper = r"\\?\HID#VID_0B05&PID_19AF&MI_02#7&ABC&0&0000#{4D1E55B2-F16F-11CF-88CB-001111000030}";
        assert!(matches(lower, 0x0B05, 0x19AF));
        assert!(matches(upper, 0x0B05, 0x19AF));
    }

    /// The UPS that started all this. It must never match, whatever else changes.
    #[test]
    fn never_matches_the_ups_or_other_devices() {
        let ups = r"\\?\hid#vid_051d&pid_0002#8&xyz&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert!(!matches(ups, 0x0B05, 0x19AF));
        assert!(matches(ups, 0x051D, 0x0002), "the matcher works, it just isn't ours");

        // A near miss on each half, so the test would fail if the two IDs were
        // ever compared independently rather than as one contiguous field.
        let same_vid = r"\\?\hid#vid_0b05&pid_1866&mi_00#...";
        let same_pid = r"\\?\hid#vid_1043&pid_19af&mi_00#...";
        assert!(!matches(same_vid, 0x0B05, 0x19AF));
        assert!(!matches(same_pid, 0x0B05, 0x19AF));
    }

    fn iface(text: &str) -> Interface {
        Interface { path: Vec::new(), text: text.into(), want: (0x0B05, 0x19AF) }
    }

    /// Windows really does return this field uppercase — the controller on the
    /// machine this was written for arrives as `&MI_02`, and reading it as
    /// lowercase-only reported every interface as -1.
    #[test]
    fn reads_the_interface_number_in_either_case() {
        assert_eq!(iface(r"\\?\hid#vid_0b05&pid_19af&mi_02#7&abc&0&0000#{guid}").interface_number(), Some(2));
        assert_eq!(iface(r"\\?\HID#VID_0B05&PID_19AF&MI_02#c&384&0&0000#{guid}").interface_number(), Some(2));
        assert_eq!(iface(r"\\?\HID#VID_0B05&PID_19AF&MI_0A#c&384&0&0000#{guid}").interface_number(), Some(10));
    }

    #[test]
    fn a_path_without_an_interface_field_has_no_number() {
        assert_eq!(iface(r"\\?\hid#vid_051d&pid_0002#8&xyz&0&0000#{guid}").interface_number(), None);
        // Truncated rather than absent: must not panic on a short slice.
        assert_eq!(iface(r"\\?\hid#vid_0b05&pid_19af&mi_").interface_number(), None);
    }
}
