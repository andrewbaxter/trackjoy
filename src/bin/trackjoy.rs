pub mod trackjoycore;

use std::{
    sync::{
        Arc,
        Mutex,
    },
    collections::HashSet,
};
use aargvark::vark;
use evdev::{
    uinput::{
        VirtualDeviceBuilder,
    },
    AbsInfo,
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
use trackjoycore::data::{
    DEST_HALF,
    DEST_MAX,
};
use crate::trackjoycore::{
    pad,
    keys,
};

mod args {
    use std::path::PathBuf;
    use aargvark::{
        Aargvark,
        AargvarkJson,
    };

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
        pub config: AargvarkJson<trackjoy::Config>,
        /// List of touchpad devices (`/dev/input/*-event-mouse`).  Each one will be
        /// converted into new joystick and four buttons on the virtual gamepad.
        pub devices: Vec<Device>,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    async fn inner() -> Result<(), loga::Error> {
        let tm = taskmanager::TaskManager::new();
        let log = loga::new(loga::Level::Info);

        // # Get and check args
        let args: args::Args = vark();
        let config = args.config.value;

        // Turn into always positive, at 0 curve is 1
        let curve = 1.37f32.powf(config.curve.unwrap_or(0.));
        let y_smash = 1.37f32.powf(config.y_smash.unwrap_or(1.));
        let active_low = config.dead_inner.unwrap_or(0.0);
        let active_high = 1.0 - config.dead_outer.unwrap_or(0.4);
        if active_high - active_low < 0. {
            return Err(loga::err("Dead zones overlap"));
        }

        // Dest prep
        let mut dest_completers = vec![];
        let mut dest_buttons = HashSet::new();
        let mut dest_axes = vec![];

        // Set up each source device, launch thread waiting for destination setup to
        // complete
        let mut pad_buttons_i = 0;
        let mut keys_buttons_i = 0;
        for dev in args.devices {
            let log = log.fork(ea!(device = dev.path.to_string_lossy()));
            let (dest, dest_completer) = ManualFuture::new();
            dest_completers.push(dest_completer);
            let mut source = Device::open(&dev.path).log_context(&log, "Error opening device")?;
            source.grab().log_context(&log, "Failed to grab device")?;
            match dev.device {
                args::DeviceType::Pad => {
                    let mappings = match config.pad_mappings.get(pad_buttons_i) {
                        Some(c) => {
                            pad_buttons_i += 1;
                            c
                        },
                        None => {
                            return Err(
                                log.new_err_with(
                                    "Config doesn't contain enough button mappings for selected pad devices",
                                    ea!(pad = pad_buttons_i, config_pads = config.pad_mappings.len()),
                                ),
                            );
                        },
                    };
                    pad::build(
                        &tm,
                        source,
                        mappings.axes,
                        mappings.buttons,
                        dest,
                        &mut dest_buttons,
                        &mut dest_axes,
                        config.multitouch,
                        config.width,
                        config.height,
                        active_high,
                        active_low,
                        curve,
                        y_smash,
                    )?
                },
                args::DeviceType::Keys => keys::build(&tm, source, match config.keys_mappings.get(keys_buttons_i) {
                    Some(c) => {
                        keys_buttons_i += 1;
                        c.clone()
                    },
                    None => {
                        return Err(
                            log.new_err_with(
                                "Config doesn't contain enough button mappings for selected key devices",
                                ea!(pad = keys_buttons_i, config_keys = config.keys_mappings.len()),
                            ),
                        );
                    },
                }, dest, &mut dest_buttons)?,
            }
        }

        // Set up dest
        {
            let mut dest =
                VirtualDeviceBuilder::new().context("Error creating virtual device builder")?.name("Trackpad JS");
            let dest_axis_setup = AbsInfo::new(DEST_HALF, 0, DEST_MAX, 20, 0, 1);
            for axis in dest_axes {
                dest =
                    dest
                        .with_absolute_axis(&UinputAbsSetup::new(axis, dest_axis_setup))
                        .context_with("Error adding axis to virtual device", ea!(axis = axis.dbg_str()))?;
            }
            let mut keys = AttributeSet::<KeyCode>::new();
            for button in dest_buttons {
                keys.insert(button);
            }
            let mut dest =
                dest
                    .with_keys(&keys)
                    .context("Error adding keys to virtual device")?
                    .build()
                    .context("Unable to create virtual joystick device")?;
            for path in dest.enumerate_dev_nodes_blocking().context("Error listing virtual device dev nodes")? {
                let path = path.context("Error getting virtual device node path")?;
                println!("Virtual device created at: {}", path.display());
            }
            let dest = Arc::new(Mutex::new(dest));
            for completer in dest_completers {
                completer.complete(dest.clone()).await;
            }
        }

        // Run
        tm.join().await.context("Error in critical task")?;
        return Ok(());
    }

    match inner().await {
        Ok(_) => { },
        Err(e) => {
            fatal(e);
        },
    }
}
