use std::collections::HashMap;
use evdev::{
    KeyCode,
    AbsoluteAxisCode,
};
use serde::{
    Serialize,
    Deserialize,
};

#[derive(Serialize, Deserialize)]
pub struct PadButtonConfig {
    pub axes: [AbsoluteAxisCode; 2],
    pub buttons: [KeyCode; 4],
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// Which buttons to assign the 4 corners on each pad. Corners are right to left,
    /// bottom to top, with 0 being the bottom right. Each keyboard will get a
    /// subsequent mapping in this list. Codes are strings in this list (ex `"KEY_1"`):
    /// <https://docs.rs/evdev/latest/src/evdev/scancodes.rs.html>
    pub pad_mappings: Vec<PadButtonConfig>,
    /// Which buttons to assign each key. Each pad will get a subsequent mapping in
    /// this list. Codes are strings in this list (ex `"KEY_1"`):
    /// <https://docs.rs/evdev/latest/src/evdev/scancodes.rs.html>
    pub keys_mappings: Vec<HashMap<KeyCode, KeyCode>>,
    /// Enable multitouch. On my 3rd party USB trackpad sometimes the off events for
    /// various touches would never come, leading to stuck buttons and axes. You can
    /// usually fix it by doing multitouch and releasing again (i.e. putting 2nd and
    /// 3rd fingers down and taking them off again) but it can be disruptive. With this
    /// off (default) only the first touch is recognized.
    #[serde(default)]
    pub multitouch: bool,
    /// Set the pad oval horizontal radius (in centimeters). Otherwise use a circle
    /// with radius of the full span of the smallest axis.
    pub width: Option<f32>,
    /// Set the pad oval vertical radius (in centimeters). Otherwise use a circle with
    /// radius of the full span of the smallest axis.
    pub height: Option<f32>,
    /// Zero the joystick input if it's less than this percent (as 0-1) of available
    /// space. Defaults to 20.
    pub dead_inner: Option<f32>,
    /// Joystick input maxes out when it reaches this percent (as 0-1) of available
    /// space. Defaults to 20.
    pub dead_outer: Option<f32>,
    /// At 0, mapping is linear. Positive numbers mean the joystick moves less near the
    /// center (finer small inputs). Negative numbers means the joystick moves less
    /// near the edges (more sensitive). Default is 0.
    pub curve: Option<f32>,
    /// Compresses everything downwards, so smaller downward movements result in larger
    /// downward values, also making the top corner buttons larger. 0 = off, higher =
    /// more compression, default is 3.
    pub y_smash: Option<f32>,
}
