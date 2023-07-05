use std::{
    collections::{
        HashMap,
        HashSet,
    },
    sync::{
        Arc,
        Mutex,
    },
};
use evdev::{
    SynchronizationCode,
    InputEvent,
    EventType,
    Device,
    uinput::VirtualDevice,
    KeyCode,
};
use loga::{
    ResultContext,
};
use manual_future::ManualFuture;
use taskmanager::TaskManager;

pub fn build(
    tm: &TaskManager,
    source: Device,
    button_codes: HashMap<KeyCode, KeyCode>,
    dest: ManualFuture<Arc<Mutex<VirtualDevice>>>,
    dest_buttons: &mut HashSet<KeyCode>,
) -> Result<(), loga::Error> {
    let mut buttons = HashMap::new();
    let mut last_buttons = HashMap::new();
    for (_, dest_code) in &button_codes {
        dest_buttons.insert(*dest_code);
        buttons.insert(*dest_code, false);
        last_buttons.insert(*dest_code, false);
    }

    // Read and write events
    let mut source = source.into_event_stream().context("Couldn't make input device async")?;
    tm.critical_task::<_, loga::Error>({
        let tm = tm.clone();
        async move {
            let dest = dest.await;
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
                            let mut dest_events = vec![];
                            for (k, on) in &buttons {
                                let last_on = last_buttons[k];
                                if *on && !last_on {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, k.0, 1));
                                } else if !on && last_on {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, k.0, 0));
                                }
                            }
                            last_buttons = buttons.clone();
                            if dest_events.len() > 0 {
                                dest
                                    .lock()
                                    .unwrap()
                                    .emit(&dest_events)
                                    .context("Failed to send events to virtual device")?;
                            }
                        }
                    },
                    evdev::EventSummary::Key(_, t, v) => {
                        match button_codes.get(&t) {
                            Some(c) => {
                                buttons.insert(*c, v != 0);
                            },
                            None => (),
                        }
                    },
                    _ => { },
                }
            }
            return Ok(());
        }
    });
    return Ok(());
}
