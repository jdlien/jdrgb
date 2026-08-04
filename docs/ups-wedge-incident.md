# The day jdrgb wedged a UPS

2026-08-03. Written for whoever works on jdrgb next, because the bug is invisible
from inside this repo: nothing here misbehaved, no test could have caught it, and
the damage landed on a different device belonging to a different program.

It took two attempts. The first (`2a3c918`) was wrong in a way that read as
obviously correct, and both sections are kept below — the failed fix is the more
useful half, because the mistake is the kind anyone would make.

## What happened

The sibling project `../jdups` monitors an APC Back-UPS over USB HID and shuts
the machine down on a sustained power failure. It gained **power-event hooks**:
config lines that run any command when the power state changes. The machine's
config wired them to jdrgb, so the case lights go amber on battery, red while a
shutdown counts down, and back to normal when mains returns:

```
on_battery_cmd = C:\bin\jdrgb.exe amber --all --stash
on_pending_cmd = C:\bin\jdrgb.exe red --all --stash
on_mains_cmd   = C:\bin\jdrgb.exe restore --all
```

Twice that evening, **the UPS stopped talking to the entire machine.** Symptoms:

- It still enumerated, and `CreateFile` on it still succeeded.
- Every request failed, from every process, including brand-new ones: HID feature
  reads, input reads (`error 1, "Incorrect function"`), and even the serial
  string, which had been reading `0B2148N01995` and started reading `(none)`.
- Reopening the handle did nothing. Restarting the services did nothing.
- **Only physically unplugging and replugging the USB cable recovered it.**

Both times it happened within seconds of **mains returning** after an outage.
With an armed shutdown agent on the other side of that silence, this was not a
cosmetic problem: the agent held a latched outage it could no longer see the end
of, and came within about twenty minutes of cleanly shutting down a machine whose
power was fine.

## How jdrgb was identified

Not quickly, and not by me. Two independent AI reviews (Codex and a second Claude
Code session) both ranked *APC firmware deadlock at transfer-back* as the leading
cause, with concurrent USB traffic as an amplifier. That was reasonable and it
was incomplete, because everyone was looking at the UPS.

The owner supplied the discriminating observation:

> the initial set to amber on power loss always works fine, it's only the
> restore that seems to hurt

That distinction is what broke the case open. Amber runs at the *start* of an
outage; `restore` runs at **mains-return** — the exact moment of both failures.
Same binary, same bus, same traffic; different instant. Firmware fragility alone
does not explain why one invocation is harmless and the other is fatal. Something
jdrgb did *at that moment* had to be the trigger.

## The mechanism

```rust
let api = HidApi::new()?;                  // <- the bug
let candidates = api.device_list()
    .filter(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID);
```

That filter looks careful. It is also far too late.

**`HidApi::new()` enumerates every HID device on the machine, and on Windows
enumeration is not passive: it opens each device and asks it for its string
descriptors** (manufacturer, product, serial). Those are control transfers on the
device's default endpoint. So every single `jdrgb` invocation — every color
change, every preset, every `state` dump — was opening and interrogating every
keyboard, mouse, GPU controller **and the UPS** on the machine, before the
VID/PID filter ever ran.

On a quiet system that is merely rude and slow. At mains-return it was fatal: the
UPS's MCU is running its relay transfer, restarting its charger and recomputing
runtime, Windows' newly bound `HidBatt` driver is bursting its own queries at the
power-state change, and jdrgb walks up and asks it for its serial number. The
firmware's control path deadlocks, and nothing short of VBUS loss brings it back.

Worth stressing: **the UPS firmware is genuinely buggy.** No sequence of *reads*
is illegal, and a device that deadlocks under them is at fault. But jdrgb was
supplying the provocation, at the worst possible instant, for no reason at all —
it needs one device and was touching two dozen.

## The first fix, which did not work

Commit `2a3c918` routed every hidapi handle through one helper that called
`disable_device_discovery()` and then `add_devices(VID, PID)`. It was wrong in
three separate ways, and this section is kept rather than deleted because the
reasoning looked sound and would be easy to repeat.

**`disable_device_discovery()` does nothing on Windows.** The flag it sets is
read in exactly one place that has any effect — hidapi 2.6.6 `src/lib.rs:166` —
and that line sits inside `#[cfg(all(libusb, not(target_os = "freebsd")))]`. It
disables device scanning inside `libusb_init()`, for Android. Windows builds
compile the bundled C backend and emit only `cargo:rustc-cfg=hidapi`; you can
confirm it in the build artifact:

```
$ grep rustc-cfg target/release/build/hidapi-*/output
cargo:rustc-cfg=hidapi          # and no libusb
```

**`HidApi::new()` enumerates regardless.** It ends with an unconditional
`api.add_devices(0, 0)?` (`src/lib.rs:190`) — outside every guard, never gated on
the discovery flag. `new_without_enumerate()` is literally
`disable_device_discovery(); Self::new()`, so it is the same function and equally
ineffective here.

**A VID/PID filter would not have been enough anyway.** `hid_enumerate` opens
*every* HID interface with `CreateFileW` and calls `HidD_GetAttributes` on it
**before** comparing the IDs (`etc/hidapi/windows/hid.c:863`). The filter only
decides whether to go on and read the string descriptors. Filtering is therefore
worth having — the string reads are the part that reaches the device — but "looks
at one VID/PID and nothing else" was never true of it.

Net effect: the helper did the full unfiltered sweep exactly as before, then a
second filtered sweep on top, and `add_devices` appends rather than replaces, so
every Aura interface appeared twice in the device list. It was slower and
touched more, not less. The one-live-plug-pull verification below passed because
a single enumeration on a healthy UPS is usually survivable, not because the
provocation had been removed.

## The fix that works

`src/hid.rs`: about 200 lines of Win32 that open one device by path. hidapi is
gone from `Cargo.toml` entirely.

```rust
// Nothing is opened here. CM_Get_Device_Interface_ListW returns the
// configuration manager's own records, and the VID/PID are matched as text in
// the path: \\?\HID#VID_0B05&PID_19AF&MI_02#...
let candidates = hid::interfaces(VENDOR_ID, PRODUCT_ID)?;
for info in &candidates {
    let dev = info.open()?;   // CreateFileW on exactly one path
    ...
}
```

A device that is not the Aura controller is now never opened, never sent a
control transfer, and never asked for anything. Not "filtered afterwards" —
never touched.

Notes for anyone touching this:

- **Do not reintroduce hidapi.** There is no argument to it that avoids the
  sweep; that is the whole finding above. If you need a second controller, add
  its VID/PID to the path match in `hid::interfaces`.
- **`hid::interfaces` must stay free of device opens.** It is the one function
  whose job is to answer "which devices are ours?" without touching anything. The
  unit tests in `src/hid.rs` pin the matcher, including a case that asserts the
  UPS's own path never matches.
- **Windows returns these paths in either case.** The controller here arrives as
  `&MI_02`, uppercase; matching lowercase-only reported every interface as -1.

## Verification

- `jdrgb probe --all` reports the controller and the card correctly, and now
  lists the control interface **once** rather than twice.
- Ten back-to-back `--all` applies, then `jdups --once`: the UPS still reads its
  serial `0B2148N01995` and answers every request.
- Full test suite green (81 tests), clippy clean.
- One live plug-pull under the *first* fix: 3.5 minutes on battery, replug,
  lights restored from the stash, and the UPS kept answering.

Evidence status, stated honestly: **2-for-2 wedges before any fix.** The clean
plug-pull was run against the first fix, which we now know changed nothing about
what gets touched — so it is evidence that a single enumeration on a healthy UPS
is survivable, not that the problem was solved. What is different now is not a
better-behaved enumeration but the absence of one: the UPS is not opened at all,
so there is no provocation left to be unlucky with. The outstanding test is still
a deeper discharge, because both wedges had the UPS's
`BelowRemainingCapacityLimit` flag asserted and the clean pulls did not.

## Side finding: GPU LED writes can glitch the display

Unrelated to the wedge, discovered the same evening and worth knowing.

`gpu.rs` reaches the card's ENE controller over the **GPU's own I2C bus via
NVAPI**, which is display plumbing — the NVAPI descriptor literally carries
`display_mask` and `is_ddc_port`. Two GPU writes seconds apart (`amber` then
`restore`, back to back during manual testing) visibly glitched the monitor for a
few seconds. A single write, and writes minutes apart, did not: the real
power-event sequence produced none.

That has since been acted on, because an automated agent driving the CLI in a
loop reproduced it:

- **A machine-wide minimum gap between GPU writes**, default 1500ms, recorded in
  `%ProgramData%\jdrgb\gpu-last-write`. `JDRGB_GPU_GAP_MS` tunes it; `0` disables
  it. The number is a guess constrained by one observation — "seconds apart" was
  bad — not a measured safe threshold. Repaints inside a single `tune`/`preview`
  session use a 100ms floor instead; the full gap per keypress would make live
  tuning unusable, and that session holds the bus mutex throughout anyway. The
  marker is stamped before a burst as well as after, so a write that fails
  part-way still paces whatever retries it.
- **Far less I2C per write.** `prepare()` used to read all 64 config bytes to use
  one of them, and each ENE register read is two NVAPI transactions. A
  `jdrgb --gpu COLOR` went from ~209 transactions to ~83.
- **A machine-wide mutex** (`Global\jdrgb-gpu-i2c`) held for the life of a `Gpu`
  handle. This closes a real race as well as capping concurrency: ENE registers
  are selected in one transaction and read or written in the next, so two
  processes interleaving could have one read back the register the other just
  selected. A command that cannot get the lock fails with a transient error
  rather than proceeding without it — carrying on unserialised would have
  reintroduced exactly that race, and `tune`/`preview` hold the lock for whole
  minutes. Serialisation between the SYSTEM boot task and a logged-in user is
  not guaranteed, because the mutex inherits the creator's default DACL; between
  processes of one user it is.

`--mb` remains the escape hatch for any caller that wants lights without touching
the card, and `JDRGB_NO_HW=1` makes every hardware path refuse — which is what an
automated test run should set.

## The transferable lesson

`../jdups/docs/power-hooks.md` now carries the rule this incident produced, and
it applies to anything jdrgb is ever wired into:

> **A hook command should touch nothing the calling program depends on.**
> Anything that scans USB, HID, or the power subsystem is being invoked at
> exactly the moment that subsystem is least able to take it.

jdrgb is a lighting utility that was being run *by a UPS monitor, during a power
failure*. That context turned a harmless inefficiency into a hardware lockup. The
inefficiency was always there; only the context was new.

For the fuller account from the other side — the agent's near-miss, the
survivability work, and the recovery design that was scoped but deliberately not
built — see `../jdups/docs/status.md`.
