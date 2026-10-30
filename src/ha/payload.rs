//! The JSON on both sides of the broker: HA's commands in, discovery and state
//! out. HA's MQTT light, JSON schema — see docs/home-assistant-plan.md for the
//! fields and the HA behaviour each choice here rests on.

use serde_json::{Value, json};

use jdrgb::palette::{COOL_KELVIN, PRESETS, WARM_KELVIN, appearance_hs, display_name, hsv_of, kelvin_of};

use crate::config::{Config, Mode};
use crate::model::{Look, Message, Projection, Shown, brightness_for};

/// Every preset as HA shows it, in table order — `Black` included, since a
/// device showing black is on and has to be reportable.
pub fn effect_list() -> Vec<String> {
    PRESETS.iter().map(|&(n, _)| display_name(n)).collect()
}

/// `Warm White` → `warmwhite`. The reverse of `display_name`, which
/// `a_display_name_round_trips_only_through_lowercase` pins in palette.rs.
pub fn preset_for_effect(effect: &str) -> Option<&'static str> {
    let key = effect.to_ascii_lowercase().replace(' ', "");
    PRESETS.iter().map(|&(n, _)| n).find(|&n| n == key)
}

/// Parse one command. Unknown fields are ignored (HA may add `transition`
/// even though discovery says no), and a field that is present but unusable is
/// dropped with a note rather than failing the whole command.
///
/// Within one message the look fields take precedence `effect` > `color` >
/// `color_temp`. In `Presets` mode HA offers no wheel or brightness, so those
/// fields are ignored even if they arrive.
pub fn parse_command(bytes: &[u8], mode: Mode) -> Result<(Message, Vec<String>), String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("command isn't JSON: {e}"))?;
    let obj = v.as_object().ok_or("command isn't a JSON object")?;
    let mut notes = Vec::new();
    let mut msg = Message::default();

    match obj.get("state").and_then(Value::as_str).map(str::to_ascii_uppercase).as_deref() {
        Some("ON") => msg.on = Some(true),
        Some("OFF") => msg.on = Some(false),
        Some(other) => notes.push(format!("ignored state '{other}'")),
        None => {}
    }

    let effect = obj.get("effect").and_then(Value::as_str).and_then(|e| {
        let p = preset_for_effect(e);
        if p.is_none() {
            notes.push(format!("ignored unknown effect '{e}'"));
        }
        p
    });
    let wheel = mode == Mode::Wheel;
    let hs = obj
        .get("color")
        .filter(|_| wheel)
        .and_then(|c| Some((c.get("h")?.as_f64()?, c.get("s")?.as_f64()?)));
    let kelvin = obj
        .get("color_temp")
        .filter(|_| wheel)
        .and_then(Value::as_f64)
        .map(|k| k.round().clamp(1.0, 100_000.0) as u32);
    msg.look = match (effect, hs, kelvin) {
        (Some(e), _, _) => Some(Look::Effect(e)),
        (None, Some((h, s)), _) => Some(Look::Hs(h, s)),
        (None, None, Some(k)) => Some(Look::Kelvin(k)),
        (None, None, None) => None,
    };
    msg.brightness = obj
        .get("brightness")
        .filter(|_| wheel)
        .and_then(Value::as_f64)
        .map(|b| b.round().clamp(1.0, 255.0) as u8);

    // `{"state":"ON"}` alone is a plain ON. A message with fields but no state
    // is treated as ON, which is what HA means by sending them.
    if msg.on.is_none() && (msg.look.is_some() || msg.brightness.is_some()) {
        msg.on = Some(true);
    }
    Ok((msg, notes))
}

/// The retained discovery message that makes HA build the light.
pub fn discovery(cfg: &Config) -> String {
    let id = cfg.id();
    let mut d = json!({
        // Named after the device: `light.<name>`.
        "name": Value::Null,
        "unique_id": format!("{id}_leds"),
        "schema": "json",
        "command_topic": cfg.topic("set"),
        // What makes HA wait for confirmed state instead of going optimistic.
        "state_topic": cfg.topic("state"),
        "availability_topic": cfg.topic("availability"),
        "effect": true,
        "effect_list": effect_list(),
        // Both default to true in HA. Left alone, HA would offer fades and
        // blinking — and a fade would be a stream of writes the card must not see.
        "transition": false,
        "flash": false,
        "device": {
            "identifiers": [id],
            "name": cfg.name,
            "manufacturer": "jdrgb",
            "model": "jdrgb-ha",
            "sw_version": env!("CARGO_PKG_VERSION"),
        },
    });
    match cfg.mode {
        Mode::Wheel => {
            let m = d.as_object_mut().expect("an object");
            m.insert("supported_color_modes".into(), json!(["hs", "color_temp"]));
            m.insert("color_temp_kelvin".into(), json!(true));
            m.insert("min_kelvin".into(), json!(WARM_KELVIN));
            m.insert("max_kelvin".into(), json!(COOL_KELVIN));
        }
        Mode::Presets => {
            d.as_object_mut().expect("an object").insert("supported_color_modes".into(), json!(["onoff"]));
        }
    }
    d.to_string()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The retained state message for a projection.
///
/// `effect` is always present when on — `null` for a color that is no preset —
/// because HA's state handler only touches the attribute when the key is there,
/// so leaving it out would leave the last preset showing.
pub fn state(p: &Projection, mode: Mode) -> String {
    let on = match p.on {
        None => return json!({ "state": Value::Null }).to_string(),
        Some(false) => return json!({ "state": "OFF" }).to_string(),
        Some(true) => p,
    };
    let (effect, level, hs_rgb) = match &on.look {
        Some(Shown::Preset { name, level }) => (Value::String(display_name(name)), *level, None),
        Some(Shown::Approx(rgb)) => (Value::Null, 100, Some(*rgb)),
        None => (Value::Null, 100, None),
    };
    if mode == Mode::Presets {
        return json!({ "state": "ON", "color_mode": "onoff", "effect": effect }).to_string();
    }
    let brightness = brightness_for(level);
    // The two whites always report as color temperatures: the file can't say
    // which control chose them, so the answer is fixed rather than remembered.
    if let Some(Shown::Preset { name, .. }) = &on.look {
        if let Some(k) = kelvin_of(name) {
            return json!({
                "state": "ON", "color_mode": "color_temp", "color_temp": k,
                "brightness": brightness, "effect": effect,
            })
            .to_string();
        }
    }
    let (h, s) = match (&on.look, hs_rgb) {
        (Some(Shown::Preset { name, .. }), _) => appearance_hs(name),
        (_, Some(rgb)) => hsv_of(rgb),
        _ => (0.0, 0.0),
    };
    json!({
        "state": "ON", "color_mode": "hs",
        "color": { "h": round1(h), "s": round1(s * 100.0) },
        "brightness": brightness, "effect": effect,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: Mode) -> Config {
        let mut c = Config::parse("host=h", "DESK").unwrap();
        c.mode = mode;
        c
    }

    fn parse(s: &str) -> Message {
        parse_command(s.as_bytes(), Mode::Wheel).unwrap().0
    }

    /// HA's own payloads, as measured on the house's install.
    #[test]
    fn has_measured_payloads() {
        assert_eq!(parse(r#"{"state":"OFF"}"#), Message { on: Some(false), ..Default::default() });
        assert_eq!(parse(r#"{"state":"ON"}"#), Message { on: Some(true), ..Default::default() });
        assert_eq!(parse(r#"{"state":"ON","effect":"Cerise"}"#).look, Some(Look::Effect("cerise")));
    }

    /// What a wheel-mode `scene.create` restore actually sends, measured on the
    /// house's HA: color and brightness ride along with the effect. The effect
    /// must win, and the brightness come back as the level it was reported from.
    #[test]
    fn a_scene_restore_lands_on_the_exact_preset_and_level() {
        use crate::model::{Cmd, Intent, level_for, resolve};
        let st = jdrgb::palette::State::default();
        for (payload, name, brightness) in [
            (r#"{"state":"ON","color":{"h":30.0,"s":100.0},"brightness":102,"effect":"Orange"}"#, "orange", 102),
            (r#"{"state":"ON","brightness":255,"color_temp":2700,"effect":"Warm White"}"#, "warmwhite", 255),
        ] {
            let m = parse(payload);
            assert_eq!(m.look, Some(Look::Effect(name)));
            let intent = Intent::merge(None, &m).unwrap();
            assert_eq!(resolve(&intent, &st, false), Some(Cmd::Preset { name, level: level_for(brightness) }));
        }
    }

    #[test]
    fn effect_beats_color_beats_color_temp_within_one_message() {
        let all = r#"{"state":"ON","effect":"Warm White","color":{"h":10,"s":90},"color_temp":6500}"#;
        assert_eq!(parse(all).look, Some(Look::Effect("warmwhite")));
        let two = r#"{"state":"ON","color":{"h":10,"s":90},"color_temp":6500}"#;
        assert_eq!(parse(two).look, Some(Look::Hs(10.0, 90.0)));
        assert_eq!(parse(r#"{"state":"ON","color_temp":2700}"#).look, Some(Look::Kelvin(2700)));
    }

    #[test]
    fn brightness_is_clamped_and_unknown_fields_ignored() {
        let m = parse(r#"{"state":"ON","brightness":300,"transition":2}"#);
        assert_eq!(m.brightness, Some(255));
        assert_eq!(parse(r#"{"state":"ON","brightness":0}"#).brightness, Some(1));
    }

    #[test]
    fn an_unknown_effect_is_noted_and_dropped() {
        let (m, notes) = parse_command(br#"{"state":"ON","effect":"Disco"}"#, Mode::Wheel).unwrap();
        assert_eq!(m, Message { on: Some(true), ..Default::default() }, "becomes a plain ON");
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn presets_mode_ignores_the_wheel_and_brightness() {
        let (m, _) = parse_command(br#"{"state":"ON","color":{"h":10,"s":90},"brightness":10}"#, Mode::Presets).unwrap();
        assert_eq!(m, Message { on: Some(true), ..Default::default() });
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_command(b"not json", Mode::Wheel).is_err());
        assert!(parse_command(b"[1,2]", Mode::Wheel).is_err());
    }

    #[test]
    fn every_effect_name_maps_back_to_its_preset() {
        for (shown, &(name, _)) in effect_list().iter().zip(PRESETS) {
            assert_eq!(preset_for_effect(shown), Some(name));
        }
        assert_eq!(effect_list().len(), 29);
        assert!(effect_list().contains(&"Black".to_string()));
    }

    #[test]
    fn discovery_has_what_ha_needs_and_turns_off_what_it_cant_do() {
        let d: Value = serde_json::from_str(&discovery(&cfg(Mode::Wheel))).unwrap();
        assert_eq!(d["name"], Value::Null);
        assert_eq!(d["unique_id"], "jdrgb_desk_leds");
        assert_eq!(d["schema"], "json");
        assert_eq!(d["command_topic"], "jdrgb/jdrgb_desk/set");
        assert_eq!(d["state_topic"], "jdrgb/jdrgb_desk/state");
        assert_eq!(d["availability_topic"], "jdrgb/jdrgb_desk/availability");
        assert_eq!(d["transition"], false);
        assert_eq!(d["flash"], false);
        assert_eq!(d["supported_color_modes"], json!(["hs", "color_temp"]));
        assert_eq!(d["color_temp_kelvin"], true);
        assert_eq!((d["min_kelvin"].as_u64(), d["max_kelvin"].as_u64()), (Some(2700), Some(6500)));
        assert_eq!(d["device"]["name"], "Desk LEDs");
        assert_eq!(d["effect_list"].as_array().unwrap().len(), 29);

        let p: Value = serde_json::from_str(&discovery(&cfg(Mode::Presets))).unwrap();
        assert_eq!(p["supported_color_modes"], json!(["onoff"]), "onoff must be the only mode");
        assert!(p.get("color_temp_kelvin").is_none());
    }

    fn st(p: Projection, mode: Mode) -> Value {
        serde_json::from_str(&state(&p, mode)).unwrap()
    }

    #[test]
    fn state_messages() {
        assert_eq!(st(Projection { on: None, look: None }, Mode::Wheel), json!({ "state": null }));
        assert_eq!(st(Projection { on: Some(false), look: None }, Mode::Wheel), json!({ "state": "OFF" }));

        let warm = Projection { on: Some(true), look: Some(Shown::Preset { name: "warmwhite", level: 100 }) };
        assert_eq!(
            st(warm.clone(), Mode::Wheel),
            json!({ "state": "ON", "color_mode": "color_temp", "color_temp": 2700, "brightness": 255, "effect": "Warm White" })
        );
        assert_eq!(st(warm, Mode::Presets), json!({ "state": "ON", "color_mode": "onoff", "effect": "Warm White" }));

        let orange = st(Projection { on: Some(true), look: Some(Shown::Preset { name: "orange", level: 40 }) }, Mode::Wheel);
        assert_eq!(orange["color_mode"], "hs");
        assert_eq!(orange["effect"], "Orange");
        assert_eq!(orange["brightness"], brightness_for(40));
    }

    /// Null, not omitted: HA only clears an effect when the key is present.
    #[test]
    fn a_non_preset_reports_an_explicit_null_effect() {
        let v = st(Projection { on: Some(true), look: Some(Shown::Approx((0x12, 0x34, 0x56))) }, Mode::Wheel);
        assert!(v.as_object().unwrap().contains_key("effect"));
        assert_eq!(v["effect"], Value::Null);
        assert_eq!(v["brightness"], 255);
        let p = st(Projection { on: Some(true), look: Some(Shown::Approx((1, 2, 3))) }, Mode::Presets);
        assert!(p.as_object().unwrap().contains_key("effect"));
    }
}
