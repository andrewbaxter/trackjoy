use std::sync::{
    Arc,
    Mutex,
};
use aargvark::vark;
use evdev::{
    uinput::{
        VirtualDevice,
        VirtualDeviceBuilder,
    },
    AbsInfo,
    AbsoluteAxisCode,
    AbsoluteAxisEvent,
    AttributeSet,
    Device,
    EventType,
    InputEvent,
    KeyCode,
    SynchronizationCode,
    UinputAbsSetup,
};
use glam::Vec2;
use loga::{
    ea,
    fatal,
    ResultContext,
};

mod args {
    use std::path::PathBuf;
    use aargvark::Aargvark;

    pub struct Pct(pub f32);

    impl aargvark::AargvarkFromStr for Pct {
        fn from_str(s: &str) -> Result<Self, String> {
            let v = f32::from_str(s).map_err(|e| e.to_string())?;
            if v < 0. {
                return Err("Percent is less than zero".to_string());
            }
            if v > 100. {
                return Err("Percent is greater than 100".to_string());
            }
            return Ok(Pct(v / 100.));
        }

        fn generate_help_placeholder() -> String {
            "%".to_string()
        }
    }

    /// Creates a single virtual gamepad.
    #[derive(Aargvark)]
    pub struct Args {
        /// List of touchpad devices (`/dev/input/*-event-mouse`).  Each one will be
        /// converted into new joystick and four buttons on the virtual gamepad.
        pub pads: Vec<PathBuf>,
        /// Zero the joystick input if it's less than this percent of available space.
        /// Defaults to 20.
        pub dead_inner: Option<Pct>,
        /// Joystick input maxes out when it reaches this percent of available space.
        /// Defaults to 20.
        pub dead_outer: Option<Pct>,
        /// At 0, mapping is linear. Positive numbers mean the joystick moves less near the
        /// center (finer small inputs). Negative numbers means the joystick moves less
        /// near the edges (more sensitive). Default is -2.
        pub curve: Option<f32>,
        /// Compresses everything downwards, so smaller downward movements result in larger
        /// downward values, also making the top corner buttons larger. 0 = off, higher =
        /// more compression, default is 3.
        pub y_smash: Option<f32>,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    async fn inner() -> Result<(), loga::Error> {
        let tm = taskmanager::TaskManager::new();

        // # Get and check args
        let args: args::Args = vark();

        // Turn into always positive, at 0 curve is 1
        let curve = 1.37f32.powf(args.curve.unwrap_or(1.5));
        let y_smash = 1.37f32.powf(args.y_smash.unwrap_or(1.));
        let active_low = args.dead_inner.map(|p| p.0).unwrap_or(0.1);
        let active_high = 1.0 - args.dead_outer.map(|p| p.0).unwrap_or(0.4);
        if active_high - active_low < 0. {
            return Err(loga::Error::new("Dead zones overlap", ea!()));
        }
        let pad_zip =
            [
                (
                    AbsoluteAxisCode::ABS_X,
                    AbsoluteAxisCode::ABS_Y,
                    [KeyCode::BTN_0, KeyCode::BTN_1, KeyCode::BTN_2, KeyCode::BTN_3],
                ),
                (
                    AbsoluteAxisCode::ABS_RX,
                    AbsoluteAxisCode::ABS_RY,
                    [KeyCode::BTN_4, KeyCode::BTN_5, KeyCode::BTN_6, KeyCode::BTN_7],
                ),
            ];
        if args.pads.len() > pad_zip.len() {
            return Err(loga::Error::new("No more than two pads are supported", ea!(got = args.pads.len())));
        }

        // Set up dest
        const BUTTON_COUNT: usize = 4;
        const DEST_MAX: i32 = 1024;
        const DEST_HALF: i32 = DEST_MAX / 2;
        let dest_half = Vec2::new(DEST_HALF as f32, DEST_HALF as f32);
        let dest = {
            let mut dest =
                VirtualDeviceBuilder::new()
                    .context("Error creating virtual device builder", ea!())?
                    .name("Trackpad JS");
            let dest_axis_setup = AbsInfo::new(DEST_HALF, 0, DEST_MAX, 20, 0, 1);
            let mut dest_keys = AttributeSet::<KeyCode>::new();
            for (_, (axis_x_code, axis_y_code, button_codes)) in args.pads.iter().zip(pad_zip) {
                dest =
                    dest
                        .with_absolute_axis(&UinputAbsSetup::new(axis_x_code, dest_axis_setup))
                        .context("Error adding x axis to virtual device", ea!())?
                        .with_absolute_axis(&UinputAbsSetup::new(axis_y_code, dest_axis_setup))
                        .context("Error adding y axis to virtual device", ea!())?;
                for c in button_codes {
                    dest_keys.insert(c);
                }
            }
            let mut dest =
                dest
                    .with_keys(&dest_keys)
                    .context("Error adding keys to virtual device", ea!())?
                    .build()
                    .context("Unable to create virtual joystick device", ea!())?;
            for path in dest.enumerate_dev_nodes_blocking().context("Error listing virtual device dev nodes", ea!())? {
                let path = path.context("Error getting virtual device node path", ea!())?;
                println!("Available as {}", path.display());
            }
            Arc::new(Mutex::new(dest))
        };

        // Launch threads to write events for each touchpad
        for (source_path, (axis_x_code, axis_y_code, button_codes)) in args.pads.iter().zip(pad_zip) {
            let dest = dest.clone();
            let mut source = Device::open(
                //. "/dev/input/by-path/pci-0000:00:15.1-platform-i2c_designware.1-event-mouse",
                &source_path,
            ).context("Error opening trackpad device", ea!())?;
            source.grab().context("Failed to grab device", ea!())?;
            let source_axes = source.get_abs_state().context("Error getting trackpad absolute state", ea!())?;
            let source_x_axis =
                source_axes.get(0).ok_or_else(|| loga::Error::new("Failed to get trackpad x axis info", ea!()))?;
            let source_y_axis =
                source_axes.get(1).ok_or_else(|| loga::Error::new("Failed to get trackpad y axis state", ea!()))?;
            let source_max = Vec2::new(source_x_axis.maximum as f32, source_y_axis.maximum as f32);
            let source_min = Vec2::new(source_x_axis.minimum as f32, source_y_axis.minimum as f32);
            let source_range_half = (source_max - source_min) / 2.;
            let source_middle = source_min + source_range_half;
            let mut source = source.into_event_stream().context("Couldn't make input device async", ea!())?;

            // Read and write events
            tm.critical_task::<_, loga::Error>({
                let tm = tm.clone();
                async move {
                    struct TouchState {
                        enabled: bool,
                        pos: Vec2,
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
                            let mut sum = Vec2::ZERO;
                            let mut sum_count = 0usize;
                            for state in &self.touch_states {
                                if !state.enabled {
                                    continue;
                                }
                                sum += state.pos;
                                sum_count += 1;
                            }

                            #[derive(PartialEq)]
                            enum Decision {
                                Button(usize),
                                Axis([i32; 2]),
                                None,
                            }

                            let decision: Decision;
                            if sum_count == 0 {
                                decision = Decision::None;
                            } else {
                                let mut unitspace_vec =
                                    (sum / (sum_count as f32) - self.source_middle) /
                                        self.source_range_half.min_element();

                                //. println!("{}", unitspace_vec);
                                println!(
                                    "smash: {} -> {} -> {} -> {}",
                                    unitspace_vec.y,
                                    (unitspace_vec.y / 2. + 0.5),
                                    (unitspace_vec.y / 2. + 0.5).powf(self.y_smash),
                                    ((unitspace_vec.y / 2. + 0.5).powf(self.y_smash) - 0.5) * 2.
                                );
                                unitspace_vec.y = ((unitspace_vec.y / 2. + 0.5).powf(self.y_smash) - 0.5) * 2.;
                                let dist = unitspace_vec.length();
                                if dist < 1. {
                                    // # Joystick
                                    //
                                    // Scale so inner and outer 10% does nothing
                                    if dist < self.active_low {
                                        unitspace_vec = Vec2::ZERO;
                                    } else {
                                        if dist >= self.active_high {
                                            unitspace_vec /= dist;
                                        } else {
                                            let n = unitspace_vec.normalize();
                                            unitspace_vec =
                                                (unitspace_vec - (n * self.active_low)) /
                                                    (self.active_high - self.active_low);
                                        }
                                        let dist = unitspace_vec.length();
                                        unitspace_vec = unitspace_vec * (dist.powf(self.curve) / dist);
                                    }
                                    let out = unitspace_vec * self.dest_half + self.dest_half;
                                    decision =
                                        Decision::Axis(
                                            [(out.x as i32).clamp(0, DEST_MAX), (out.y as i32).clamp(0, DEST_MAX)],
                                        );
                                } else {
                                    // # Button quadrants
                                    match (unitspace_vec.x >= 0., unitspace_vec.y >= 0.) {
                                        (true, true) => {
                                            decision = Decision::Button(0);
                                        },
                                        (false, true) => {
                                            decision = Decision::Button(1);
                                        },
                                        (true, false) => {
                                            decision = Decision::Button(2);
                                        },
                                        (false, false) => {
                                            decision = Decision::Button(3);
                                        },
                                    }
                                }
                            }
                            let mut dest_events = vec![];
                            let axis = if let Decision::Axis(v) = decision {
                                v
                            } else {
                                [self.dest_half.x as i32, self.dest_half.y as i32]
                            };
                            if axis != self.last_axis {
                                dest_events.push(*AbsoluteAxisEvent::new(self.axis_x_code, axis[0]));
                                dest_events.push(*AbsoluteAxisEvent::new(self.axis_y_code, axis[1]));
                            }
                            self.last_axis = axis;
                            for i in 0 .. BUTTON_COUNT {
                                let on = decision == Decision::Button(i);
                                if on && !self.last_buttons[i] {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, self.button_codes[i].0, 1));
                                } else if !on && self.last_buttons[i] {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, self.button_codes[i].0, 0));
                                }
                                self.last_buttons[i] = on;
                            }
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
                        }],
                        active_high: active_high,
                        curve: curve,
                        y_smash: y_smash,
                        active_low: active_low,
                        axis_x_code: axis_x_code,
                        axis_y_code: axis_y_code,
                        button_codes: button_codes,
                        dest_half: dest_half,
                        source_middle: source_middle,
                        source_range_half: source_range_half,
                        dest: dest.clone(),
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
                                    {
                                        state.flush()?;
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
        }
        tm.join().await.context("Error in critical task", ea!())?;
        return Ok(());
    }

    match inner().await {
        Ok(_) => { },
        Err(e) => {
            fatal(e);
        },
    }
}
