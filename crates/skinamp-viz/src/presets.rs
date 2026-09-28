//! Presets: the built-ins, plus any `*.wgsl` in
//! ~/.config/skinamp/viz/. A user file with a built-in's name
//! replaces it; others are added after the built-ins. Each is a `scene`
//! function (see shaders/prelude.wgsl), rescanned every second so edits
//! show up live.

use std::path::PathBuf;
use std::time::SystemTime;

pub const PRELUDE: &str = include_str!("shaders/prelude.wgsl");

const BUILTIN: [(&str, &str); 5] = [
    ("battery", include_str!("shaders/battery.wgsl")),
    ("alchemy", include_str!("shaders/alchemy.wgsl")),
    ("spectrum", include_str!("shaders/spectrum.wgsl")),
    ("ambience", include_str!("shaders/ambience.wgsl")),
    ("warp", include_str!("shaders/warp.wgsl")),
];

#[derive(Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub source: String,
    /// The user file it came from, if any, and its modification time.
    pub file: Option<(PathBuf, Option<SystemTime>)>,
}

impl Preset {
    /// The complete shader: the prelude's helpers, then the preset. The
    /// prelude's entry points call `scene`, which WGSL allows to be
    /// declared after them.
    pub fn shader(&self) -> String {
        format!("{PRELUDE}\n// ---- {}\n{}", self.name, self.source)
    }

    /// A compile error with its line numbers moved from the combined
    /// shader to the preset's own file ("wgsl:116:47" to "name.wgsl:1:47").
    pub fn explain(&self, error: &str) -> String {
        let offset = PRELUDE.lines().count() + 2;
        let file = format!("{}.wgsl", self.name);
        let mut out = String::new();
        let mut rest = error;
        while let Some(i) = rest.find("wgsl:") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 5..];
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            match after[..digits].parse::<usize>() {
                Ok(line) if line > offset => {
                    out.push_str(&format!("{file}:{}", line - offset));
                }
                Ok(line) => out.push_str(&format!("prelude.wgsl:{line}")),
                Err(_) => out.push_str("wgsl:"),
            }
            rest = &after[digits..];
        }
        out.push_str(rest);
        out
    }
}

pub fn user_dir() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    config.join("skinamp").join("viz")
}

pub fn load() -> Vec<Preset> {
    let mut all: Vec<Preset> = BUILTIN
        .iter()
        .map(|(name, src)| Preset {
            name: name.to_string(),
            source: src.to_string(),
            file: None,
        })
        .collect();
    let mut extra: Vec<Preset> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(user_dir()) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "wgsl") {
                continue;
            }
            let (Some(name), Ok(source)) = (
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string),
                std::fs::read_to_string(&path),
            ) else {
                continue;
            };
            let stamp = entry.metadata().and_then(|m| m.modified()).ok();
            let preset = Preset {
                name: name.clone(),
                source,
                file: Some((path, stamp)),
            };
            match all.iter_mut().find(|p| p.name == name) {
                Some(p) => *p = preset,
                None => extra.push(preset),
            }
        }
    }
    extra.sort_by(|a, b| a.name.cmp(&b.name));
    all.extend(extra);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in (and the example) parses and validates as WGSL, prelude
    /// included.
    #[test]
    fn builtins_are_valid_wgsl() {
        let example = (
            "ring (example)",
            include_str!("../../../examples/viz/ring.wgsl"),
        );
        for (name, src) in BUILTIN.into_iter().chain([example]) {
            let preset = Preset {
                name: name.into(),
                source: src.into(),
                file: None,
            };
            let module = naga::front::wgsl::parse_str(&preset.shader())
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&preset.shader())));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

    #[test]
    fn errors_point_at_the_preset_file() {
        let p = Preset {
            name: "mine".into(),
            source: "fn scene(uv: vec2<f32>) -> vec3<f32> { return oops; }".into(),
            file: None,
        };
        let line = PRELUDE.lines().count() + 3;
        let e = p.explain(&format!("error at wgsl:{line}:47 here"));
        assert_eq!(e, "error at mine.wgsl:1:47 here");
    }

    #[test]
    fn blit_is_valid_wgsl() {
        let src = include_str!("shaders/blit.wgsl");
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap();
    }
}
