# The day jdrgb wedged a UPS

2026-08-03. Written for whoever works on jdrgb next, because the bug is invisible
from inside this repo: nothing here misbehaved, no test could have caught it, and
the damage landed on a different device belonging to a different program. The fix
is one commit (`2a3c918`), and the reason it matters is worth more than the diff.

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

## The fix

`disable_device_discovery()` + `add_devices(VID, PID)`, behind one helper that
every hidapi handle in the program now comes from:

```rust
fn aura_api() -> Result<HidApi, String> {
    static DISABLE: std::sync::Once = std::sync::Once::new();
    DISABLE.call_once(HidApi::disable_device_discovery);
    let mut api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    api.add_devices(VENDOR_ID, PRODUCT_ID)
        .map_err(|e| format!("could not look for the Aura controller: {e}"))?;
    Ok(api)
}
```

Six call sites (`set_solid`, the two other paint paths, `Live::open`, the state
dump, `probe_mb`) now route through it. `HidApi::new()` appears nowhere else.

Notes for anyone touching this:

- **`new_without_enumerate()` is deprecated** and hidapi says why: it is a global
  operation, so libraries must not do it and applications should be explicit.
  jdrgb is an application, so it calls `disable_device_discovery()` itself. That
  is the sanctioned path, not a workaround.
- **The `Once` is not paranoia.** `disable_device_discovery()` panics if a context
  was already built *with* discovery. One helper, one disable, no ordering
  question. If you ever add a second hidapi entry point, route it through
  `aura_api()` or you reintroduce the panic *and* the bug.
- **Adding a second controller means a second `add_devices` call**, not a return
  to broad enumeration. Filtering after the fact is what caused this.

## Verification

- `jdrgb amber --all --stash` and `jdrgb restore --all` both still drive the
  motherboard strip and the GPU correctly.
- Full test suite green, clippy clean.
- One live plug-pull afterwards: 3.5 minutes on battery, replug, lights restored
  from the stash, **and the UPS kept answering** — reads, serial and all.

Evidence status, stated honestly: **2-for-2 wedges before the fix, 1-for-1 clean
after.** That is likely-fixed, not proven. The outstanding test is a deeper
discharge, because both wedges had the UPS's `BelowRemainingCapacityLimit` flag
asserted and the clean pulls did not.

## Side finding: GPU LED writes can glitch the display

Unrelated to the wedge, discovered the same evening and worth knowing.

`gpu.rs` reaches the card's ENE controller over the **GPU's own I2C bus via
NVAPI**, which is display plumbing — the NVAPI descriptor literally carries
`display_mask` and `is_ddc_port`. Two GPU writes seconds apart (`amber` then
`restore`, back to back during manual testing) visibly glitched the monitor for a
few seconds. A single write, and writes minutes apart, did not: the real
power-event sequence produced none.

So no code change was made. But if a future feature paints the GPU rapidly —
animation, a pulse effect, live tuning at speed — expect display disturbance, and
consider rate-limiting the GPU path specifically. `--mb` is the escape hatch for
any caller that wants lights without touching the card.

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
