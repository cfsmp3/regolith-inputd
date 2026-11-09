use crate::utils;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use swayipc::Fallible;

/// An input device as sway reports it: identifier, type and libinput
/// settings.
#[derive(Clone)]
struct Device {
    identifier: String,
    input_type: String,
    libinput: Map<String, Value>,
}

/// The libinput settings of sway's input devices. Several devices can
/// share an identifier (a mouse with a relative and an absolute node, a
/// receiver for several devices), so they are kept in a list.
#[derive(Default)]
pub struct SwayState(Vec<Device>);

impl SwayState {
    pub fn read() -> Fallible<SwayState> {
        Ok(SwayState::from_inputs(utils::get_inputs_json()?))
    }

    /// The state of the devices in a GET_INPUTS reply.
    pub fn from_inputs(inputs: Vec<Value>) -> SwayState {
        let mut devices = Vec::new();
        for input in inputs {
            let (Some(identifier), Some(input_type)) =
                (input["identifier"].as_str(), input["type"].as_str())
            else {
                continue;
            };
            devices.push(Device {
                identifier: identifier.to_string(),
                input_type: input_type.to_string(),
                libinput: input["libinput"].as_object().cloned().unwrap_or_default(),
            });
        }
        SwayState(devices)
    }

    fn identifiers(&self, input_type: &str) -> Vec<&str> {
        let mut identifiers: Vec<&str> = self
            .0
            .iter()
            .filter(|d| d.input_type == input_type)
            .map(|d| d.identifier.as_str())
            .collect();
        identifiers.sort_unstable();
        identifiers
    }

    /// The settings on which all devices of `input_type` that have them
    /// agree. GSettings has one value for all devices of a type, so a
    /// setting that differs between devices has none; a device without a
    /// setting (an absolute pointer has no acceleration) doesn't count,
    /// whether sway leaves the setting out or reports it as null.
    fn common_settings(&self, input_type: &str) -> BTreeMap<&str, &Value> {
        let mut common: BTreeMap<&str, Option<&Value>> = BTreeMap::new();
        for device in self.0.iter().filter(|d| d.input_type == input_type) {
            for (setting, value) in device.libinput.iter().filter(|(_, v)| !v.is_null()) {
                common
                    .entry(setting)
                    .and_modify(|agreed| {
                        if *agreed != Some(value) {
                            *agreed = None;
                        }
                    })
                    .or_insert(Some(value));
            }
        }
        common
            .into_iter()
            .filter_map(|(setting, value)| Some((setting, value?)))
            .collect()
    }

    /// The settings, by device type, that every device of the type changed
    /// to between `self` and `new`: what a user does with `swaymsg input
    /// type:...` or by configuring their only device. Types whose devices
    /// were added or removed in between are left out, since a device that
    /// was just plugged in is no change the user made.
    pub fn changes(&self, new: &SwayState) -> Vec<(String, String, Value)> {
        let types: BTreeSet<&str> = new.0.iter().map(|d| d.input_type.as_str()).collect();
        let mut changes = Vec::new();
        for input_type in types {
            if self.identifiers(input_type) != new.identifiers(input_type) {
                continue;
            }
            let old = self.common_settings(input_type);
            for (setting, value) in new.common_settings(input_type) {
                if old.get(setting) != Some(&value) {
                    changes.push((input_type.to_string(), setting.to_string(), value.clone()));
                }
            }
        }
        changes
    }

    /// Take the devices of every type whose devices were added or removed
    /// between `self` and `new` as they are in `new`.
    pub fn adopt_devices(&mut self, new: &SwayState) {
        let types: BTreeSet<String> = self
            .0
            .iter()
            .chain(&new.0)
            .map(|d| d.input_type.clone())
            .collect();
        for input_type in types {
            if self.identifiers(&input_type) != new.identifiers(&input_type) {
                self.0.retain(|d| d.input_type != input_type);
                self.0
                    .extend(new.0.iter().filter(|d| d.input_type == input_type).cloned());
            }
        }
    }

    /// Whether every device of `input_type` that has `setting` still
    /// agrees on `value`, with the same devices as in `before`.
    pub fn still_holds(
        &self,
        before: &SwayState,
        (input_type, setting, value): &(String, String, Value),
    ) -> bool {
        self.identifiers(input_type) == before.identifiers(input_type)
            && self.common_settings(input_type).get(setting.as_str()) == Some(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::SwayState;
    use serde_json::{json, Value};

    fn device(identifier: &str, input_type: &str, libinput: Value) -> Value {
        json!({"identifier": identifier, "type": input_type, "libinput": libinput})
    }

    fn state(devices: &[Value]) -> SwayState {
        SwayState::from_inputs(devices.to_vec())
    }

    #[test]
    fn a_change_to_every_device_of_a_type_is_reported() {
        let old = state(&[
            device(
                "1:1:Pad",
                "touchpad",
                json!({"tap": "disabled", "dwt": "enabled"}),
            ),
            device(
                "2:2:Mouse",
                "pointer",
                json!({"natural_scroll": "disabled"}),
            ),
        ]);
        let new = state(&[
            device(
                "1:1:Pad",
                "touchpad",
                json!({"tap": "enabled", "dwt": "enabled"}),
            ),
            device(
                "2:2:Mouse",
                "pointer",
                json!({"natural_scroll": "disabled"}),
            ),
        ]);
        assert_eq!(
            old.changes(&new),
            [("touchpad".to_string(), "tap".to_string(), json!("enabled"))]
        );
        assert!(new.changes(&new).is_empty());
    }

    #[test]
    fn a_change_to_one_of_several_devices_is_not_reported() {
        let mice = |second: &str| {
            state(&[
                device("1:1:A", "pointer", json!({"left_handed": "disabled"})),
                device("2:2:B", "pointer", json!({"left_handed": second})),
            ])
        };
        assert!(mice("disabled").changes(&mice("enabled")).is_empty());
        // When the devices agree again, their common value is reported.
        assert_eq!(
            mice("enabled").changes(&mice("disabled")),
            [(
                "pointer".to_string(),
                "left_handed".to_string(),
                json!("disabled")
            )]
        );
    }

    #[test]
    fn devices_plugged_in_or_out_are_not_changes() {
        let one = state(&[device("1:1:A", "pointer", json!({"accel_speed": 0.5}))]);
        let two = state(&[
            device("1:1:A", "pointer", json!({"accel_speed": 0.5})),
            device("2:2:B", "pointer", json!({"accel_speed": 0.0})),
        ]);
        let other = state(&[device("2:2:B", "pointer", json!({"accel_speed": 0.0}))]);
        assert!(SwayState::default().changes(&one).is_empty());
        assert!(one.changes(&two).is_empty());
        assert!(two.changes(&other).is_empty());
        assert!(one.changes(&other).is_empty());
    }

    #[test]
    fn changes_after_adopting_plugged_in_devices_are_reported() {
        let one = state(&[device("1:1:A", "pointer", json!({"tap": "disabled"}))]);
        let two = |second: &str| {
            state(&[
                device("1:1:A", "pointer", json!({"tap": "disabled"})),
                device("2:2:B", "pointer", json!({"tap": second})),
            ])
        };
        let mut known = one;
        known.adopt_devices(&two("enabled"));
        assert!(known.changes(&two("enabled")).is_empty());
        assert_eq!(
            known.changes(&state(&[
                device("1:1:A", "pointer", json!({"tap": "enabled"})),
                device("2:2:B", "pointer", json!({"tap": "enabled"})),
            ])),
            [("pointer".to_string(), "tap".to_string(), json!("enabled"))]
        );
    }

    #[test]
    fn devices_sharing_an_identifier_are_all_kept() {
        let mouse = |relative: &str, absolute: &str| {
            state(&[
                device(
                    "2:19:VMMouse",
                    "pointer",
                    json!({"natural_scroll": relative}),
                ),
                device(
                    "2:19:VMMouse",
                    "pointer",
                    json!({"natural_scroll": absolute}),
                ),
            ])
        };
        let one_node = state(&[device(
            "2:19:VMMouse",
            "pointer",
            json!({"natural_scroll": "enabled"}),
        )]);
        assert!(mouse("disabled", "disabled")
            .changes(&mouse("enabled", "disabled"))
            .is_empty());
        assert_eq!(
            mouse("disabled", "disabled").changes(&mouse("enabled", "enabled")),
            [(
                "pointer".to_string(),
                "natural_scroll".to_string(),
                json!("enabled")
            )]
        );
        assert!(mouse("disabled", "disabled").changes(&one_node).is_empty());
    }

    #[test]
    fn devices_without_a_setting_do_not_count() {
        let pointers = |speed: f64| {
            state(&[
                device("1:1:Mouse", "pointer", json!({"accel_speed": speed})),
                device(
                    "2:2:Absolute",
                    "pointer",
                    json!({"left_handed": "disabled"}),
                ),
            ])
        };
        assert_eq!(
            pointers(0.0).changes(&pointers(0.5)),
            [("pointer".to_string(), "accel_speed".to_string(), json!(0.5))]
        );
    }

    #[test]
    fn settings_reported_as_null_do_not_count() {
        let pointers = |speed: f64| {
            state(&[
                device("1:1:Mouse", "pointer", json!({"accel_speed": speed})),
                device(
                    "2:2:Absolute",
                    "pointer",
                    json!({"accel_speed": null, "tap": null}),
                ),
            ])
        };
        assert_eq!(
            pointers(0.0).changes(&pointers(0.25)),
            [(
                "pointer".to_string(),
                "accel_speed".to_string(),
                json!(0.25)
            )]
        );
    }

    #[test]
    fn devices_without_identifier_or_libinput_are_tolerated() {
        let new = state(&[
            json!({"name": "no identifier", "type": "keyboard"}),
            device("0:0:wlr_virtual_keyboard_v1", "keyboard", Value::Null),
        ]);
        assert!(SwayState::default().changes(&new).is_empty());
    }
}
