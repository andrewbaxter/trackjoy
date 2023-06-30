use std::{
    sync::{
        Arc,
        Mutex,
    },
};
use aargvark::vark;
use evdev::{
    uinput::{
        VirtualDeviceBuilder,
    },
    AbsInfo,
    AbsoluteAxisCode,
    AttributeSet,
    Device,
    KeyCode,
    UinputAbsSetup,
};
use loga::{
    ea,
    fatal,
    ResultContext,
    DebugDisplay,
};
use manual_future::ManualFuture;
use crate::data::{
    DEST_HALF,
    DEST_MAX,
};

pub mod data;
pub mod keys;
pub mod pad;

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

    #[derive(Aargvark)]
    pub enum DeviceType {
        /// A trackpad, becomes 1 stick and 4 buttons.
        Pad,
        /// Something with keys, each key is turned into a button. Too many keys will run
        /// you out of buttons, beware.
        Keys,
    }

    #[derive(Aargvark)]
    pub struct Device {
        pub device: DeviceType,
        pub path: PathBuf,
    }

    /// Creates a single virtual gamepad.
    #[derive(Aargvark)]
    pub struct Args {
        /// List of touchpad devices (`/dev/input/*-event-mouse`).  Each one will be
        /// converted into new joystick and four buttons on the virtual gamepad.
        pub devices: Vec<Device>,
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
        let mut available_buttons =
            vec![
                KeyCode::BTN_EAST,
                KeyCode::BTN_SOUTH,
                KeyCode::BTN_NORTH,
                KeyCode::BTN_WEST,
                KeyCode::BTN_TR,
                KeyCode::BTN_TL,
                KeyCode::BTN_TR2,
                KeyCode::BTN_TL2,
                KeyCode::BTN_THUMBR,
                KeyCode::BTN_THUMBL,
                KeyCode::BTN_TRIGGER_HAPPY1,
                KeyCode::BTN_TRIGGER_HAPPY2,
                KeyCode::BTN_TRIGGER_HAPPY3,
                KeyCode::BTN_TRIGGER_HAPPY4,
                KeyCode::BTN_TRIGGER_HAPPY5,
                KeyCode::BTN_TRIGGER_HAPPY6,
                KeyCode::BTN_TRIGGER_HAPPY7,
                KeyCode::BTN_TRIGGER_HAPPY8,
                KeyCode::BTN_0,
                KeyCode::BTN_1,
                KeyCode::BTN_2,
                KeyCode::BTN_3,
                KeyCode::BTN_4,
                KeyCode::BTN_5,
                KeyCode::BTN_6,
                KeyCode::BTN_7,
                KeyCode::BTN_8,
                KeyCode::BTN_9
            ];
        let mut available_axes =
            vec![
                AbsoluteAxisCode::ABS_X,
                AbsoluteAxisCode::ABS_Y,
                AbsoluteAxisCode::ABS_RX,
                AbsoluteAxisCode::ABS_RY
            ];

        // Dest prep
        let mut dest_completers = vec![];
        let mut dest_buttons = vec![];
        let mut dest_axes = vec![];

        // Set up each source device, launch thread waiting for destination setup to
        // complete
        for dev in args.devices {
            let (dest, dest_completer) = ManualFuture::new();
            dest_completers.push(dest_completer);
            let mut source = Device::open(&dev.path).context("Error opening device", ea!())?;
            source.grab().context("Failed to grab device", ea!())?;
            match dev.device {
                args::DeviceType::Pad => pad::build(
                    &tm,
                    source,
                    dest,
                    &mut dest_buttons,
                    &mut dest_axes,
                    &mut available_buttons,
                    &mut available_axes,
                    active_high,
                    active_low,
                    curve,
                    y_smash,
                )?,
                args::DeviceType::Keys => keys::build(&tm, source, dest, &mut dest_buttons, &mut available_buttons)?,
            }
        }

        // Set up dest
        {
            let mut dest =
                VirtualDeviceBuilder::new()
                    .context("Error creating virtual device builder", ea!())?
                    .name("Trackpad JS");
            let dest_axis_setup = AbsInfo::new(DEST_HALF, 0, DEST_MAX, 20, 0, 1);
            for axis in dest_axes {
                dest =
                    dest
                        .with_absolute_axis(&UinputAbsSetup::new(axis, dest_axis_setup))
                        .context("Error adding axis to virtual device", ea!(axis = axis.dbg_str()))?;
            }
            let mut keys = AttributeSet::<KeyCode>::new();
            for button in dest_buttons {
                keys.insert(button);
            }
            let mut dest =
                dest
                    .with_keys(&keys)
                    .context("Error adding keys to virtual device", ea!())?
                    .build()
                    .context("Unable to create virtual joystick device", ea!())?;
            for path in dest.enumerate_dev_nodes_blocking().context("Error listing virtual device dev nodes", ea!())? {
                let path = path.context("Error getting virtual device node path", ea!())?;
                println!("Virtual device created at: {}", path.display());
            }
            let dest = Arc::new(Mutex::new(dest));
            for completer in dest_completers {
                completer.complete(dest.clone()).await;
            }
        }

        // Run
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
