//! Prost-generated control plane protobuf types.
//!
//! Source of truth: `proto/control.proto` (package `waywallen.control.v1`).
//! The generated Rust code lives in `$OUT_DIR/waywallen.control.v1.rs`.

include!(concat!(env!("OUT_DIR"), "/waywallen.control.v1.rs"));

use crate::plugin::renderer_registry::{SettingDef, SettingType};

/// Stringify a `toml::Value` for the wire `default_value` / `min` /
/// `max` / `step` fields. Empty string when the value is `None` /
/// structurally absent — matches the proto convention ("empty = unset").
fn toml_value_to_wire(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        // Arrays/tables aren't valid setting scalars; fall back to the
        // TOML debug repr so the UI at least sees something rather
        // than silently dropping the manifest's intent.
        other => other.to_string(),
    }
}

fn setting_type_to_proto(ty: SettingType) -> i32 {
    match ty {
        SettingType::U32 => SettingValueType::U32 as i32,
        SettingType::F32 => SettingValueType::F32 as i32,
        SettingType::String => SettingValueType::String as i32,
        SettingType::Bool => SettingValueType::Bool as i32,
    }
}

/// Convert one manifest `SettingDef` into the `SettingSchema` wire
/// shape consumed by `RendererPluginInfo.settings`. Stringifies the
/// `default` / `min` / `max` / `step` so the wire stays homogeneous
/// with `PluginSettings.values`.
pub fn setting_def_to_proto(key: &str, def: &SettingDef) -> SettingSchema {
    SettingSchema {
        key: key.to_string(),
        r#type: setting_type_to_proto(def.ty),
        default_value: toml_value_to_wire(&def.default),
        identity: def.identity,
        label_key: def.label_key.clone().unwrap_or_default(),
        description_key: def.description_key.clone().unwrap_or_default(),
        min: def.min.as_ref().map(toml_value_to_wire).unwrap_or_default(),
        max: def.max.as_ref().map(toml_value_to_wire).unwrap_or_default(),
        step: def
            .step
            .as_ref()
            .map(toml_value_to_wire)
            .unwrap_or_default(),
        choices: def.choices.clone().unwrap_or_default(),
        group: def.group.clone().unwrap_or_default(),
        order: def.order.unwrap_or(0),
    }
}
