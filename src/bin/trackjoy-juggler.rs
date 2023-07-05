use std::{
    fs::{
        create_dir_all,
        read_dir,
    },
    ffi::{
        OsStr,
        OsString,
    },
    os::unix::prelude::OsStrExt,
    collections::HashMap,
    time::Duration,
};
use aargvark::vark;
use futures::{
    executor::block_on,
};
use loga::{
    ResultContext,
    ea,
    fatal,
    DebugDisplay,
};
use notify::{
    RecommendedWatcher,
    RecursiveMode,
    Watcher,
    Event,
};
use tokio::{
    sync::mpsc::channel,
    process::Child,
};

trait OsStrMissing {
    fn ends_with(&self, s: &[u8]) -> bool;
}

impl OsStrMissing for OsStr {
    fn ends_with(&self, s: &[u8]) -> bool {
        if self.len() < s.len() {
            return false;
        }
        return &self.as_bytes()[self.len() - s.len() .. self.len()] == s;
    }
}

#[derive(PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
enum DevType {
    Keys,
    Pad,
}

fn find_groupings(
    want_keys: usize,
    want_pads: usize,
    mut values: Vec<OsString>,
) -> Result<Vec<Vec<(DevType, OsString)>>, loga::Error> {
    values.sort();
    let mut working = vec![];
    for value in values {
        let value = if value.as_os_str().ends_with("__keys".as_bytes()) {
            (DevType::Keys, value)
        } else if value.as_os_str().ends_with("__pad".as_bytes()) {
            (DevType::Pad, value)
        } else {
            return Err(
                loga::err_with(
                    "Device that doesn't match type suffix found in dev dir",
                    ea!(dev = value.to_string_lossy()),
                ),
            );
        };
        working.push(value);
    }
    let mut groups = vec![];
    while working.len() > 0 {
        let mut keys_count = 0usize;
        let mut pads_count = 0usize;
        let mut ok_until = 0;
        for (i, (type_, _)) in working.iter().enumerate() {
            match type_ {
                DevType::Keys => {
                    keys_count += 1;
                },
                DevType::Pad => {
                    pads_count += 1;
                },
            }
            if keys_count > want_keys || pads_count > want_pads {
                break;
            }
            ok_until = i + 1;
        }
        if ok_until == 0 {
            return Err(
                loga::err_with(
                    "Encountered device type with no config",
                    ea!(
                        type_ = working.get(0).unwrap().0.dbg_str(),
                        device = working.get(0).unwrap().1.to_string_lossy()
                    ),
                ),
            );
        }
        let new_working = working.split_off(ok_until);
        groups.push(working.split_off(0));
        working = new_working;
    }
    return Ok(groups);
}

mod args {
    use std::path::PathBuf;
    use aargvark::{
        Aargvark,
        AargvarkJson,
    };
    use trackjoy::Config;

    #[derive(Aargvark)]
    pub struct Args {
        pub config: AargvarkJson<Config>,
        pub dev_dir: PathBuf,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    async fn inner() -> Result<(), loga::Error> {
        let args = vark::<args::Args>();
        let config_source = match args.config.source {
            aargvark::Source::Stdin => {
                return Err(loga::err("Configuration must be in a file to provide to child processes"));
            },
            aargvark::Source::File(f) => f,
        };
        let tm = taskmanager::TaskManager::new();
        let log = &loga::new(loga::Level::Info);
        let (event_transmit, mut event_receive) = channel(1);
        tm.critical_task({
            let log = log.clone();
            let tm = tm.clone();
            let event_transmit = event_transmit.clone();
            async move {
                let log = &log;
                let mut procs: HashMap<Vec<(DevType, OsString)>, Child> = HashMap::new();
                create_dir_all(&args.dev_dir).log_context(log, "Failed to ensure dev node dir")?;

                // Debounce loop - outer waits forever, ignore first event + subsequent events
                // until a timeout, then go back to waiting forever
                let mut watcher = RecommendedWatcher::new(move |res: Result<Event, notify::Error>| {
                    block_on(async {
                        _ = event_transmit.send(res.map(|_| ())).await;
                    })
                }, notify::Config::default()).log_context(log, "Failed to configure dev node watcher")?;
                watcher.watch(&args.dev_dir, RecursiveMode::NonRecursive).log_context(log, "Error starting watch")?;
                'event_loop: while let Some(Some(_)) = tm.if_alive(event_receive.recv()).await {
                    while let Some(timeout_res) =
                        tm.if_alive(tokio::time::timeout(Duration::from_millis(1000), event_receive.recv())).await {
                        match timeout_res {
                            Ok(channel_res) => match channel_res {
                                Some(event) => {
                                    if let Err(e) = event {
                                        log.warn_e(e.into(), "Watch event error", ea!());
                                        continue;
                                    } else {
                                        // Not timeout - not debounced; continue until timeout
                                        continue;
                                    }
                                },
                                None => {
                                    break 'event_loop;
                                },
                            },
                            Err(_) => {
                                // Timeout elapsed
                            },
                        }
                        match read_dir(&args.dev_dir) {
                            Ok(devices) => {
                                let mut device_list = vec![];
                                for device in devices {
                                    let device = match device {
                                        Ok(d) => d,
                                        Err(e) => {
                                            log.warn_e(e.into(), "Error reading dev tree entry", ea!());
                                            continue;
                                        },
                                    };
                                    device_list.push(device.file_name());
                                }
                                let mut new_procs = HashMap::new();
                                let mut pre_new_procs = vec![];
                                for group in find_groupings(
                                    args.config.value.keys_mappings.len() as usize,
                                    args.config.value.pad_mappings.len() as usize,
                                    device_list.into_iter().collect(),
                                )? {
                                    if let Some(proc_group) = procs.remove(&group) {
                                        new_procs.insert(group, proc_group);
                                        continue;
                                    }
                                    pre_new_procs.push(group);
                                }
                                for (group, mut proc) in procs {
                                    log.info("Stopping trackjoy", ea!(group = group.dbg_str()));
                                    match proc.kill().await {
                                        Ok(_) => { },
                                        Err(e) => {
                                            log.warn_e(
                                                e.into(),
                                                "Failed to kill child for stale grouping",
                                                ea!(child = proc.dbg_str()),
                                            );
                                            continue;
                                        },
                                    };
                                    match proc.wait().await {
                                        Ok(_) => { },
                                        Err(e) => {
                                            log.warn_e(
                                                e.into(),
                                                "Failed to wait for child to stop in stale grouping",
                                                ea!(child = proc.dbg_str()),
                                            );
                                            continue;
                                        },
                                    };
                                }
                                procs = new_procs;
                                for group in pre_new_procs {
                                    log.info("Launching trackjoy", ea!(group = group.dbg_str()));
                                    let mut c = tokio::process::Command::new("trackjoy");
                                    c.arg(config_source.as_os_str());
                                    for (type_, path) in &group {
                                        match type_ {
                                            DevType::Keys => {
                                                c.arg("keys");
                                            },
                                            DevType::Pad => {
                                                c.arg("pad");
                                            },
                                        }
                                        c.arg(path);
                                    }
                                    let proc = match c.spawn() {
                                        Ok(p) => p,
                                        Err(e) => {
                                            log.warn_e(
                                                e.into(),
                                                "Error starting trackjoy process on dev group",
                                                ea!(cmd = c.dbg_str()),
                                            );
                                            continue;
                                        },
                                    };
                                    procs.insert(group, proc);
                                }
                            },
                            Err(e) => {
                                log.warn_e(e.into(), "Failed to list devices", ea!());
                            },
                        };
                    }
                }
                return Ok(()) as Result<(), loga::Error>;
            }
        });

        // Initial scan
        _ = event_transmit.send(Ok(())).await;

        // Wait for shutdown
        tm.join().await?;
        return Ok(());
    }

    match inner().await {
        Err(e) => {
            fatal(e);
        },
        Ok(_) => { },
    }
}
