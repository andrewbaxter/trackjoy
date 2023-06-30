use std::sync::{
    Mutex,
    Arc,
};
use evdev::{
    Device,
    uinput::VirtualDevice,
    KeyCode,
    AbsoluteAxisCode,
    AbsoluteAxisEvent,
    InputEvent,
    EventType,
    SynchronizationCode,
};
use glam::Vec2;
use loga::{
    ea,
    ResultContext,
};
use manual_future::ManualFuture;
use taskmanager::TaskManager;
use crate::data::{
    DEST_HALF,
    DEST_MAX,
};

const BUTTON_COUNT: usize = 4;

pub fn build(
    tm: &TaskManager,
    source: Device,
    dest: ManualFuture<Arc<Mutex<VirtualDevice>>>,
    dest_buttons: &mut Vec<KeyCode>,
    dest_axes: &mut Vec<AbsoluteAxisCode>,
    available_buttons: &mut Vec<KeyCode>,
    available_axes: &mut Vec<AbsoluteAxisCode>,
    active_high: f32,
    active_low: f32,
    curve: f32,
    y_smash: f32,
) -> Result<(), loga::Error> {
    // Allocate buttons/axes
    if available_buttons.len() < BUTTON_COUNT {
        return Err(loga::Error::new("Ran out of availble buttons, too many keyboards/trackpads", ea!()));
    }
    let button_codes: [KeyCode; 4] =
        available_buttons.split_off(available_buttons.len() - BUTTON_COUNT).try_into().unwrap();
    dest_buttons.extend_from_slice(&button_codes);
    let axis_x_code =
        available_axes
            .pop()
            .ok_or_else(|| loga::Error::new("Too many axes for virtual device, try using fewer trackpads", ea!()))?;
    let axis_y_code =
        available_axes
            .pop()
            .ok_or_else(|| loga::Error::new("Too many axes for virtual device, try using fewer trackpads", ea!()))?;
    dest_axes.extend_from_slice(&[axis_x_code, axis_y_code]);

    // Prep spatial info
    let source_axes = source.get_abs_state().context("Error getting trackpad absolute state", ea!())?;
    let source_x_axis =
        source_axes.get(0).ok_or_else(|| loga::Error::new("Failed to get trackpad x axis info", ea!()))?;
    let source_y_axis =
        source_axes.get(1).ok_or_else(|| loga::Error::new("Failed to get trackpad y axis state", ea!()))?;
    let source_max = Vec2::new(source_x_axis.maximum as f32, source_y_axis.maximum as f32);
    let source_min = Vec2::new(source_x_axis.minimum as f32, source_y_axis.minimum as f32);
    let source_range_half = (source_max - source_min) / 2.;
    let source_middle = source_min + source_range_half;
    let dest_half = Vec2::new(DEST_HALF as f32, DEST_HALF as f32);

    // Read and write events
    let mut source = source.into_event_stream().context("Couldn't make input device async", ea!())?;
    tm.critical_task::<_, loga::Error>({
        let tm = tm.clone();
        async move {
            enum TouchBake {
                Indeterminate,
                Axis,
                Button(usize),
            }

            struct TouchState {
                enabled: bool,
                pos: Vec2,
                baked: TouchBake,
            }

            struct State {
                slot: usize,
                last_axis: [i32; 2],
                last_buttons: [bool; 4],
                touch_states: Vec<TouchState>,
                curve: f32,
                y_smash: f32,
                active_low: f32,
                active_high: f32,
                source_middle: Vec2,
                source_range_half: Vec2,
                dest_half: Vec2,
                axis_x_code: AbsoluteAxisCode,
                axis_y_code: AbsoluteAxisCode,
                button_codes: [KeyCode; 4],
                dest: Arc<Mutex<VirtualDevice>>,
            }

            impl State {
                fn flush(&mut self) -> Result<(), loga::Error> {
                    let mut axis_sum = Vec2::ZERO;
                    let mut axis_sum_count = 0usize;
                    let mut buttons = [false; BUTTON_COUNT];
                    for state in &mut self.touch_states {
                        if !state.enabled {
                            continue;
                        }
                        let mut unitspace_vec =
                            (state.pos - self.source_middle) / self.source_range_half.min_element();
                        unitspace_vec.y = ((unitspace_vec.y / 2. + 0.5).powf(self.y_smash) - 0.5) * 2.;
                        match state.baked {
                            TouchBake::Indeterminate => {
                                if unitspace_vec.length() <= 1. {
                                    state.baked = TouchBake::Axis;
                                    axis_sum += unitspace_vec;
                                    axis_sum_count += 1;
                                } else {
                                    let button_i = match (unitspace_vec.x >= 0., unitspace_vec.y >= 0.) {
                                        (true, true) => 0,
                                        (false, true) => 1,
                                        (true, false) => 2,
                                        (false, false) => 3,
                                    };
                                    buttons[button_i] = true;
                                    state.baked = TouchBake::Button(button_i);
                                }
                            },
                            TouchBake::Axis => {
                                axis_sum += unitspace_vec;
                                axis_sum_count += 1;
                            },
                            TouchBake::Button(button_i) => {
                                buttons[button_i] = true;
                            },
                        }
                    }
                    let mut dest_events = vec![];

                    // Prepare events for axis change
                    let axis = if axis_sum_count > 0 {
                        let mut unitspace_vec = axis_sum / (axis_sum_count as f32);
                        let dist = unitspace_vec.length();

                        // Scale so inner and outer 10% does nothing
                        if dist < self.active_low {
                            unitspace_vec = Vec2::ZERO;
                        } else {
                            if dist >= self.active_high {
                                unitspace_vec /= dist;
                            } else {
                                let n = unitspace_vec.normalize();
                                unitspace_vec =
                                    (unitspace_vec - (n * self.active_low)) / (self.active_high - self.active_low);
                            }
                            let dist = unitspace_vec.length();
                            unitspace_vec = unitspace_vec * (dist.powf(self.curve) / dist);
                        }
                        let out = unitspace_vec * self.dest_half + self.dest_half;
                        [(out.x as i32).clamp(0, DEST_MAX), (out.y as i32).clamp(0, DEST_MAX)]
                    } else {
                        [self.dest_half.x as i32, self.dest_half.y as i32]
                    };
                    if axis != self.last_axis {
                        dest_events.push(*AbsoluteAxisEvent::new(self.axis_x_code, axis[0]));
                        dest_events.push(*AbsoluteAxisEvent::new(self.axis_y_code, axis[1]));
                    }
                    self.last_axis = axis;

                    // Prepare events for button changes
                    for i in 0 .. BUTTON_COUNT {
                        let on = buttons[i];
                        if on && !self.last_buttons[i] {
                            dest_events.push(InputEvent::new(EventType::KEY.0, self.button_codes[i].0, 1));
                        } else if !on && self.last_buttons[i] {
                            dest_events.push(InputEvent::new(EventType::KEY.0, self.button_codes[i].0, 0));
                        }
                        self.last_buttons[i] = on;
                    }

                    // Send
                    if dest_events.len() > 0 {
                        self
                            .dest
                            .lock()
                            .unwrap()
                            .emit(&dest_events)
                            .context("Failed to send events to virtual device", ea!())?;
                    }
                    return Ok(());
                }
            }

            let mut state = State {
                slot: 0usize,
                last_axis: [0i32; 2],
                last_buttons: [false; 4],
                touch_states: vec![TouchState {
                    enabled: false,
                    pos: source_middle,
                    baked: TouchBake::Indeterminate,
                }],
                curve: curve,
                y_smash: y_smash,
                active_high: active_high,
                active_low: active_low,
                axis_x_code: axis_x_code,
                axis_y_code: axis_y_code,
                button_codes: button_codes,
                dest_half: dest_half,
                source_middle: source_middle,
                source_range_half: source_range_half,
                dest: dest.await,
            };
            loop {
                let ev = match tm.if_alive(source.next_event()).await {
                    Some(x) => x,
                    None => {
                        break;
                    },
                }?;
                match ev.destructure() {
                    evdev::EventSummary::Synchronization(_, t, _) => {
                        if t == SynchronizationCode::SYN_REPORT {
                            state.flush()?;
                        }
                    },
                    evdev::EventSummary::AbsoluteAxis(_, type_, value) => match type_ {
                        AbsoluteAxisCode::ABS_MT_SLOT => {
                            state.slot = value as usize;
                            while state.touch_states.len() < state.slot + 1 {
                                state.touch_states.push(TouchState {
                                    enabled: false,
                                    pos: source_middle,
                                    baked: TouchBake::Indeterminate,
                                });
                            }
                        },
                        AbsoluteAxisCode::ABS_MT_POSITION_X => {
                            state.touch_states[state.slot].pos.x = value as f32;
                        },
                        AbsoluteAxisCode::ABS_MT_POSITION_Y => {
                            state.touch_states[state.slot].pos.y = value as f32;
                        },
                        AbsoluteAxisCode::ABS_MT_TRACKING_ID => {
                            state.touch_states[state.slot].enabled = value != -1;
                            if state.touch_states.iter().all(|s| !s.enabled) {
                                state.flush()?;
                            }
                        },
                        _ => { },
                    },
                    _ => { },
                }
            }
            return Ok(());
        }
    });
    return Ok(());
}
