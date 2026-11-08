//! Egg (server template) system.
//!
//! Eggs are JSON documents stored in `<data_dir>/eggs`. A set of curated eggs is
//! embedded in the binary and written out on first start so a fresh install can
//! immediately create servers. Imported eggs are validated before being saved.

use anyhow::{bail, Context, Result};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(RustEmbed)]
#[folder = "../../eggs"]
struct EmbeddedEggs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Egg {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_author")]
    pub author: String,
    #[serde(default = "default_category")]
    pub category: String,
    /// Selectable Docker images; the first is the default.
    #[serde(default)]
    pub docker_images: Vec<String>,
    /// Optional entrypoint override. When absent the image's entrypoint runs.
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    /// Shell command executed inside the container. `{{VAR}}` is substituted.
    #[serde(default)]
    pub startup: String,
    /// Command written to the console for a graceful stop (may be empty).
    #[serde(default)]
    pub stop_command: String,
    /// Text that marks the server as ready in its console output.
    #[serde(default)]
    pub startup_detection: Option<String>,
    #[serde(default)]
    pub install: Option<EggInstall>,
    #[serde(default)]
    pub variables: Vec<EggVariable>,
    #[serde(default)]
    pub ports: Vec<EggPort>,
    #[serde(default)]
    pub notes: String,
    #[serde(default = "default_version")]
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EggInstall {
    pub image: String,
    /// POSIX shell script executed once in a throwaway container with the volume
    /// mounted at /mnt/server. It runs as the container user, never on the host.
    pub script: String,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EggVariable {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default_value: String,
    #[serde(default)]
    pub user_editable: bool,
    #[serde(default)]
    pub rules: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EggPort {
    pub name: String,
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_author() -> String {
    "Porgous".into()
}
fn default_category() -> String {
    "Generic".into()
}
fn default_version() -> u32 {
    1
}
fn default_protocol() -> String {
    "tcp".into()
}

impl Egg {
    pub fn default_image(&self) -> Result<&str> {
        self.docker_images
            .first()
            .map(|s| s.as_str())
            .ok_or_else(|| anyhow::anyhow!("egg has no docker image"))
    }

    pub fn variable_defaults(&self) -> BTreeMap<String, String> {
        self.variables
            .iter()
            .map(|v| (v.key.clone(), v.default_value.clone()))
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.len() < 2 || self.id.len() > 64 {
            bail!("egg id must be 2-64 characters");
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            bail!("egg id may only contain lowercase letters, digits, dash and underscore");
        }
        if self.name.trim().is_empty() || self.name.len() > 120 {
            bail!("egg name must be 1-120 characters");
        }
        if self.docker_images.is_empty() || self.docker_images.len() > 10 {
            bail!("egg must have between 1 and 10 docker images");
        }
        for image in &self.docker_images {
            validate_image_reference(image)?;
        }
        if self.startup.len() > 4000 {
            bail!("startup command is too long");
        }
        if let Some(install) = &self.install {
            if install.script.len() > 64 * 1024 {
                bail!("install script exceeds 64 KiB");
            }
            validate_image_reference(&install.image)?;
        }
        let mut keys = std::collections::HashSet::new();
        if self.variables.len() > 64 {
            bail!("too many variables (max 64)");
        }
        for variable in &self.variables {
            if variable.key.is_empty() || variable.key.len() > 64 {
                bail!("variable key must be 1-64 characters");
            }
            if !variable
                .key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                bail!(
                    "variable key '{}' must be uppercase letters, digits or underscore",
                    variable.key
                );
            }
            if !keys.insert(variable.key.clone()) {
                bail!("duplicate variable key '{}'", variable.key);
            }
            if variable.rules.len() > 512 {
                bail!("variable rules are too long");
            }
        }
        if self.ports.len() > 32 {
            bail!("too many ports");
        }
        for port in &self.ports {
            if !matches!(port.protocol.as_str(), "tcp" | "udp") {
                bail!("port protocol must be tcp or udp");
            }
        }
        Ok(())
    }
}

/// Basic Docker image reference validation. Rejects shell metacharacters and
/// references to the host Docker socket.
pub fn validate_image_reference(image: &str) -> Result<()> {
    if image.is_empty() || image.len() > 255 {
        bail!("invalid docker image");
    }
    if !image
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | ':' | '@' | '-' | '_'))
    {
        bail!("docker image contains invalid characters");
    }
    if image.starts_with('/') || image.contains("docker.sock") {
        bail!("invalid docker image");
    }
    Ok(())
}

pub fn render_template(template: &str, variables: &std::collections::HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            let key = after[..end].trim();
            if let Some(value) = variables.get(key) {
                out.push_str(value);
            }
            rest = &after[end + 2..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Validate a user supplied variable value against the egg's simple rules DSL.
pub fn validate_variable(variable: &EggVariable, value: &str) -> Result<()> {
    if value.len() > 4096 {
        bail!("value for {} is too long", variable.key);
    }
    if value.contains('\0') {
        bail!("value for {} contains a NUL byte", variable.key);
    }
    for rule in variable
        .rules
        .split('|')
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
    {
        let (name, arg) = rule
            .split_once(':')
            .map(|(a, b)| (a.trim(), Some(b.trim())))
            .unwrap_or((rule, None));
        match name {
            "required" => {
                if value.trim().is_empty() {
                    bail!("{} is required", variable.name);
                }
            }
            "string" => {}
            "numeric" | "integer" => {
                if !value.trim().is_empty() && value.trim().parse::<i64>().is_err() {
                    bail!("{} must be a whole number", variable.name);
                }
            }
            "boolean" => {
                if !value.trim().is_empty()
                    && !matches!(
                        value.trim(),
                        "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off"
                    )
                {
                    bail!("{} must be true or false", variable.name);
                }
            }
            "max" => {
                let max: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(value.len());
                if value.len() > max {
                    bail!("{} must be at most {} characters", variable.name, max);
                }
            }
            "min" => {
                if let Some(min) = arg.and_then(|a| a.parse::<usize>().ok()) {
                    if value.len() < min {
                        bail!("{} must be at least {} characters", variable.name, min);
                    }
                }
            }
            "in" => {
                if let Some(list) = arg {
                    if !value.is_empty() && !list.split(',').any(|v| v.trim() == value) {
                        bail!("{} must be one of: {}", variable.name, list);
                    }
                }
            }
            "regex" => {
                // A tiny, non-backtracking allowlist for common patterns avoids a regex dependency.
                if let Some(pattern) = arg {
                    if pattern == "[a-zA-Z0-9_-]+"
                        && !value
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        bail!("{} has invalid characters", variable.name);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Filesystem-backed egg repository.
pub struct EggStore {
    dir: PathBuf,
    eggs: RwLock<BTreeMap<String, Egg>>,
}

impl EggStore {
    pub fn load(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating egg directory {}", dir.display()))?;
        for file in EmbeddedEggs::iter() {
            let name = file.as_ref();
            if !name.ends_with(".json") {
                continue;
            }
            let target = dir.join(name);
            if target.exists() {
                continue;
            }
            if let Some(content) = EmbeddedEggs::get(name) {
                let _ = std::fs::write(&target, content.data.as_ref());
            }
        }
        let store = Self {
            dir: dir.to_path_buf(),
            eggs: RwLock::new(BTreeMap::new()),
        };
        store.reload()?;
        Ok(store)
    }

    pub fn reload(&self) -> Result<()> {
        let mut map = BTreeMap::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)?;
            match serde_json::from_str::<Egg>(&raw).and_then(|egg| {
                egg.validate().map(|_| egg).map_err(|e| {
                    serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                })
            }) {
                Ok(egg) => {
                    map.insert(egg.id.clone(), egg);
                }
                Err(err) => tracing::warn!(path = %path.display(), error = %err, "skipping invalid egg"),
            }
        }
        *self.eggs.write().unwrap() = map;
        Ok(())
    }

    pub fn list(&self) -> Vec<Egg> {
        self.eggs.read().unwrap().values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<Egg> {
        self.eggs.read().unwrap().get(id).cloned()
    }

    pub fn import(&self, egg: Egg, overwrite: bool) -> Result<()> {
        egg.validate()?;
        let existing = self.get(&egg.id);
        if existing.is_some() && !overwrite {
            bail!("an egg with id '{}' already exists", egg.id);
        }
        let json = serde_json::to_string_pretty(&egg)?;
        let path = self.dir.join(format!("{}.json", egg.id));
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        self.eggs.write().unwrap().insert(egg.id.clone(), egg);
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let path = self.dir.join(format!("{id}.json"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        self.eggs.write().unwrap().remove(id);
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Egg {
        Egg {
            id: "test-egg".into(),
            name: "Test".into(),
            description: String::new(),
            author: String::new(),
            category: String::new(),
            docker_images: vec!["alpine:3.20".into()],
            entrypoint: None,
            startup: "echo {{SERVER_MEMORY}}".into(),
            stop_command: String::new(),
            startup_detection: None,
            install: None,
            variables: vec![EggVariable {
                key: "SERVER_MEMORY".into(),
                name: "Memory".into(),
                description: String::new(),
                default_value: "1024".into(),
                user_editable: false,
                rules: "required|numeric".into(),
            }],
            ports: vec![],
            notes: String::new(),
            version: 1,
        }
    }

    #[test]
    fn validates_good_egg_and_rejects_bad_ids() {
        sample().validate().unwrap();
        let mut bad = sample();
        bad.id = "Bad Egg!".into();
        assert!(bad.validate().is_err());
        let mut bad_image = sample();
        bad_image.docker_images = vec!["alpine; rm -rf /".into()];
        assert!(bad_image.validate().is_err());
    }

    #[test]
    fn renders_templates() {
        let vars = std::collections::HashMap::from([
            ("SERVER_MEMORY".to_string(), "2048".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]);
        assert_eq!(render_template("-Xmx{{SERVER_MEMORY}}M", &vars), "-Xmx2048M");
        assert_eq!(render_template("{{EMPTY}}x", &vars), "x");
        assert_eq!(render_template("{{MISSING}}!", &vars), "!");
        assert_eq!(render_template("plain", &vars), "plain");
    }

    #[test]
    fn variable_rules() {
        let mut var = sample().variables.remove(0);
        assert!(validate_variable(&var, "1024").is_ok());
        assert!(validate_variable(&var, "ten").is_err());
        assert!(validate_variable(&var, "").is_err());
        var.rules = "required|in:a,b".into();
        assert!(validate_variable(&var, "a").is_ok());
        assert!(validate_variable(&var, "c").is_err());
    }

    #[test]
    fn embedded_eggs_are_valid() {
        for name in EmbeddedEggs::iter() {
            let content = EmbeddedEggs::get(&name).unwrap();
            let egg: Egg = serde_json::from_slice(content.data.as_ref()).expect(&name);
            egg.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = EggStore::load(dir.path()).unwrap();
        let egg = sample();
        store.import(egg.clone(), false).unwrap();
        assert_eq!(store.get("test-egg").unwrap().name, "Test");
        assert!(store.import(egg, false).is_err());
        store.delete("test-egg").unwrap();
        assert!(store.get("test-egg").is_none());
    }
}
