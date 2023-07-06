use std::{
    sync::{
        Mutex,
        Arc,
    },
    collections::HashSet,
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
    ResultContext,
};
use manual_future::ManualFuture;
use taskmanager::TaskManager;
use crate::trackjoycore::data::DEST_MAX;
use super::data::DEST_HALF;

const BUTTON_COUNT: usize = 4;

pub fn build(
    tm: &TaskManager,
    source: Device,
    axis_codes: [AbsoluteAxisCode; 2],
    button_codes: [KeyCode; 4],
    dest: ManualFuture<Arc<Mutex<VirtualDevice>>>,
    dest_buttons: &mut HashSet<KeyCode>,
    dest_axes: &mut Vec<AbsoluteAxisCode>,
    multitouch: bool,
    cm_x_radius: Option<f32>,
    cm_y_radius: Option<f32>,
    active_high: f32,
    active_low: f32,
    curve: f32,
    y_smash: f32,
) -> Result<(), loga::Error> {
    // Allocate buttons/axes
    for c in &button_codes {
        dest_buttons.insert(*c);
    }
    dest_axes.extend_from_slice(&axis_codes);

    // Prep spatial info
    let source_axes = source.get_abs_state().context("Error getting trackpad absolute state")?;
    let source_x_axis = source_axes.get(0).ok_or_else(|| loga::err("Failed to get trackpad x axis info"))?;
    let source_y_axis = source_axes.get(1).ok_or_else(|| loga::err("Failed to get trackpad y axis state"))?;
    let source_max = Vec2::new(source_x_axis.maximum as f32, source_y_axis.maximum as f32);
    let source_min = Vec2::new(source_x_axis.minimum as f32, source_y_axis.minimum as f32);
    let resolution = Vec2::new(source_x_axis.resolution as f32, source_y_axis.resolution as f32);
    let phys_size = (source_max - source_min) / resolution / 10.;
    let source_range_half = (source_max - source_min) / 2.;
    let source_middle = source_min + source_range_half;
    let mut unit_divisor;
    if phys_size.x > phys_size.y {
        unit_divisor = Vec2::new(source_range_half.y * resolution.x / resolution.y, source_range_half.y);
    } else {
        unit_divisor = Vec2::new(source_range_half.x, source_range_half.x * resolution.y / resolution.x);
    }
    if let Some(x_radius) = cm_x_radius {
        unit_divisor.x = x_radius * 10. * resolution.x;
    }
    if let Some(y_radius) = cm_y_radius {
        unit_divisor.y = y_radius * 10. * resolution.x;
    }
    let dest_half = Vec2::new(DEST_HALF as f32, DEST_HALF as f32);

    // Read and write events
    let mut source = source.into_event_stream().context("Couldn't make input device async")?;
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
                dest: Arc<Mutex<VirtualDevice>>,
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
                            let mut axis_sum = Vec2::ZERO;
                            let mut axis_sum_count = 0usize;
                            let mut buttons = [false; BUTTON_COUNT];
                            for (state_i, state) in state.touch_states.iter_mut().enumerate() {
                                if !state.enabled {
                                    continue;
                                }
                                if state_i > 0 && !multitouch {
                                    continue;
                                }

                                // narrowest axis is -1 .. 1 for full span of trackpad; -1 is up; trans axis may
                                // be over or under 1 depending on resolution ratio ratio
                                let mut unitspace_vec = (state.pos - source_middle) / unit_divisor;

                                // y-space compressed downward (towards 1) with low numbers of y_smash
                                unitspace_vec.y = ((unitspace_vec.y / 2. + 0.52).clamp(0., 1.1).powf(y_smash) - 0.52) * 2.;
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
                                // Average of axis touches, unit vec (-1 .. 1 both axes)
                                let mut unitspace_vec = axis_sum / (axis_sum_count as f32);
                                let dist = unitspace_vec.length();
                                if dist < active_low {
                                    // Center dead space
                                    unitspace_vec = Vec2::ZERO;
                                } else {
                                    if dist >= active_high {
                                        // Outer dead space (set length to 1)
                                        unitspace_vec /= dist;
                                    } else {
                                        // Scale linearly between dead spaces
                                        let activespace_dist = (dist - active_low) / (active_high - active_low);
                                        unitspace_vec *= activespace_dist / dist;

                                        // Apply a curve
                                        unitspace_vec = unitspace_vec * (activespace_dist.powf(curve) / activespace_dist);
                                    }
                                }
                                let out = unitspace_vec * dest_half + dest_half;
                                [(out.x as i32).clamp(0, DEST_MAX), (out.y as i32).clamp(0, DEST_MAX)]
                            } else {
                                [dest_half.x as i32, dest_half.y as i32]
                            };
                            if axis != state.last_axis {
                                dest_events.push(*AbsoluteAxisEvent::new(axis_codes[0], axis[0]));
                                dest_events.push(*AbsoluteAxisEvent::new(axis_codes[1], axis[1]));
                            }
                            state.last_axis = axis;

                            // Prepare events for button changes
                            for i in 0 .. BUTTON_COUNT {
                                let on = buttons[i];
                                if on && !state.last_buttons[i] {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, button_codes[i].0, 1));
                                } else if !on && state.last_buttons[i] {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, button_codes[i].0, 0));
                                }
                                state.last_buttons[i] = on;
                            }

                            // Send
                            if dest_events.len() > 0 {
                                state
                                    .dest
                                    .lock()
                                    .unwrap()
                                    .emit(&dest_events)
                                    .context("Failed to send events to virtual device")?;
                            }
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
                            let enabled = value != -1;
                            state.touch_states[state.slot].enabled = enabled;
                            if !enabled {
                                if let TouchBake::Button(i) = state.touch_states[state.slot].baked {
                                    // Sometimes evdev doesn't send release events for slots so they get stuck. Make
                                    // another press + release reset the button as an intuitive workaround/fix...
                                    for s in &mut state.touch_states {
                                        if s.enabled && match s.baked {
                                            TouchBake::Button(j) if i == j => true,
                                            _ => false,
                                        } {
                                            s.enabled = false;
                                            s.baked = TouchBake::Indeterminate;
                                        }
                                    }
                                }
                                state.touch_states[state.slot].baked = TouchBake::Indeterminate;
                            }
                        },
                        _ => (),
                    },
                    _ => { },
                }
            }
            return Ok(());
        }
    });
    return Ok(());
}
