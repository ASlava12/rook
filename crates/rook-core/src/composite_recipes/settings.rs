use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_depth: usize,
    pub max_definitions: usize,
    pub max_stages: usize,
    pub max_calls: usize,
    pub source_bytes: usize,
    pub result_bytes: usize,
    pub state_bytes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_definitions: 16,
            max_stages: 16,
            max_calls: 32,
            source_bytes: 512 * 1024,
            result_bytes: 32768,
            state_bytes: 1024 * 1024,
        }
    }
}
impl Settings {
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, value, min, max) in [
            ("max_depth", self.max_depth, 0, 8),
            ("max_definitions", self.max_definitions, 1, 64),
            ("max_stages", self.max_stages, 1, 64),
            ("max_calls", self.max_calls, 1, 128),
            ("source_bytes", self.source_bytes, 4096, 4 * 1024 * 1024),
            ("result_bytes", self.result_bytes, 1024, 128 * 1024),
            ("state_bytes", self.state_bytes, 4096, 16 * 1024 * 1024),
        ] {
            if !(min..=max).contains(&value) {
                errors.push(format!("composite_recipes.{name}: expected {min}..={max}"));
            }
        }
        errors
    }
}
