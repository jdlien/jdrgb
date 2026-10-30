//! `%LOCALAPPDATA%\jdrgb\ha.conf`, and the names everything else is built from.

use std::path::{Path, PathBuf};

/// How much of a light Home Assistant is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The full light: color wheel and white temperature snapped to presets,
    /// brightness, and the presets as effects. The default.
    Wheel,
    /// On/off and the presets as effects only. The fallback if a client — the
    /// plan's open question is Apple Home — fights the snapped echo of a wheel.
    Presets,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// The device's name in HA, which also names the entity.
    pub name: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub mode: Mode,
    /// The computer's name, lowercased and reduced to `[a-z0-9_]` — the part of
    /// every id and topic that tells two PCs apart.
    pub machine: String,
}

/// Where the bridge looks for its config and writes its log.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("jdrgb"))
}

pub fn default_path() -> Option<PathBuf> {
    dir().map(|d| d.join("ha.conf"))
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "pc".into())
}

/// `DESK` → `desk`; `DESKTOP-AB12` → `desktop_ab12`. What MQTT topics and
/// HA ids can carry without escaping, and what the broker's ACL names.
pub fn machine_id(computer: &str) -> String {
    let id: String = computer
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if id.is_empty() { "pc".into() } else { id }
}

/// `DESK` → `Desk LEDs`. A house can have more than one PC, so a fixed
/// default would collide.
pub fn default_name(computer: &str) -> String {
    let mut chars = computer.chars();
    let titled: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => "PC".into(),
    };
    format!("{titled} LEDs")
}

impl Config {
    /// Parse `key=value` lines. `#` starts a comment; unknown keys are an
    /// error, because a misspelled `hots=` silently ignored would leave the
    /// bridge connecting nowhere with no idea why.
    pub fn parse(text: &str, computer: &str) -> Result<Config, String> {
        let mut host = None;
        let mut port = 1883u16;
        let mut name = None;
        let mut user = None;
        let mut password = None;
        let mut mode = Mode::Wheel;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("ha.conf:{}: expected key=value, got '{line}'", n + 1));
            };
            let value = value.trim().to_string();
            match key.trim().to_ascii_lowercase().as_str() {
                "host" => host = Some(value),
                "port" => {
                    port = value.parse().map_err(|_| format!("ha.conf:{}: port must be a number, not '{value}'", n + 1))?
                }
                "name" => name = Some(value),
                "user" => user = Some(value),
                "password" => password = Some(value),
                "mode" => {
                    mode = match value.to_ascii_lowercase().as_str() {
                        "wheel" => Mode::Wheel,
                        "presets" => Mode::Presets,
                        _ => return Err(format!("ha.conf:{}: mode is `wheel` or `presets`, not '{value}'", n + 1)),
                    }
                }
                other => return Err(format!("ha.conf:{}: unknown key '{other}'", n + 1)),
            }
        }
        let host = host.filter(|h| !h.is_empty()).ok_or("ha.conf has no host= line — the broker's address")?;
        if password.is_some() && user.is_none() {
            return Err("ha.conf has a password but no user".into());
        }
        Ok(Config {
            host,
            port,
            name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| default_name(computer)),
            user,
            password,
            mode,
            machine: machine_id(computer),
        })
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Config::parse(&text, &computer_name())
    }

    /// `jdrgb_desk` — the device's id in HA, and the middle of every topic.
    pub fn id(&self) -> String {
        format!("jdrgb_{}", self.machine)
    }

    /// `jdrgb-ha-desk`. Fixed per machine, so a second bridge anywhere kicks
    /// the first off the broker rather than silently sharing its topics.
    pub fn client_id(&self) -> String {
        format!("jdrgb-ha-{}", self.machine)
    }

    pub fn topic(&self, leaf: &str) -> String {
        format!("jdrgb/{}/{leaf}", self.id())
    }

    pub fn discovery_topic(&self) -> String {
        format!("homeassistant/light/{}/leds/config", self.id())
    }
}

pub const BIRTH_TOPIC: &str = "homeassistant/status";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_houses_config_is_host_and_port_with_the_name_from_the_machine() {
        let c = Config::parse("host=192.0.2.10\nport=1884\n", "DESK").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("192.0.2.10", 1884));
        assert_eq!(c.name, "Desk LEDs");
        assert_eq!(c.user, None, "anonymous unless a user is given");
        assert_eq!(c.mode, Mode::Wheel);
        assert_eq!(c.id(), "jdrgb_desk");
        assert_eq!(c.client_id(), "jdrgb-ha-desk");
        assert_eq!(c.topic("set"), "jdrgb/jdrgb_desk/set");
        assert_eq!(c.discovery_topic(), "homeassistant/light/jdrgb_desk/leds/config");
    }

    /// The ACL the broker's owner installs names exactly these.
    #[test]
    fn every_topic_sits_under_the_acls_two_subtrees() {
        let c = Config::parse("host=h", "DESK").unwrap();
        for leaf in ["set", "state", "availability", "echo"] {
            assert!(c.topic(leaf).starts_with("jdrgb/jdrgb_desk/"));
        }
        assert!(c.discovery_topic().starts_with("homeassistant/light/jdrgb_desk/"));
    }

    #[test]
    fn optional_keys_comments_and_defaults() {
        let c = Config::parse(
            "# broker\nhost = mqtt.lan  \nname=Desk Glow\nuser=jd\npassword=x\nmode=presets\n",
            "DESKTOP-AB12",
        )
        .unwrap();
        assert_eq!(c.port, 1883, "the MQTT standard when not given");
        assert_eq!(c.name, "Desk Glow");
        assert_eq!(c.user.as_deref(), Some("jd"));
        assert_eq!(c.mode, Mode::Presets);
        assert_eq!(c.id(), "jdrgb_desktop_ab12");
    }

    #[test]
    fn mistakes_are_errors_not_silence() {
        assert!(Config::parse("port=1884\n", "PC").is_err(), "no host");
        assert!(Config::parse("hots=h\n", "PC").is_err(), "a misspelled key");
        assert!(Config::parse("host=h\nport=lots\n", "PC").is_err());
        assert!(Config::parse("host=h\npassword=x\n", "PC").is_err(), "a password with no user");
        assert!(Config::parse("host=h\nmode=rainbow\n", "PC").is_err());
        assert!(Config::parse("just a line\n", "PC").is_err());
    }

    #[test]
    fn names_from_odd_computer_names() {
        assert_eq!(machine_id("Desk-PC 2"), "desk_pc_2");
        assert_eq!(default_name("DESK"), "Desk LEDs");
        assert_eq!(machine_id(""), "pc");
    }
}
