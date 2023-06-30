use std::{
    collections::HashMap,
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
    ea,
    ResultContext,
};
use manual_future::ManualFuture;
use taskmanager::TaskManager;

pub fn build(
    tm: &TaskManager,
    source: Device,
    dest: ManualFuture<Arc<Mutex<VirtualDevice>>>,
    dest_buttons: &mut Vec<KeyCode>,
    available_buttons: &mut Vec<KeyCode>,
) -> Result<(), loga::Error> {
    let mut button_codes = HashMap::new();
    let mut buttons = HashMap::new();
    let mut last_buttons = HashMap::new();
    for source_code in source.supported_keys().map(|a| a.iter()).into_iter().flatten() {
        let dest_code =
            available_buttons
                .pop()
                .ok_or_else(
                    || loga::Error::new(
                        "Ran out of buttons; total keys across trackpads and keyboards is too large",
                        ea!(),
                    ),
                )?;
        dest_buttons.push(dest_code);
        button_codes.insert(source_code, dest_code);
        buttons.insert(source_code, false);
        last_buttons.insert(source_code, false);
    }

    // Read and write events
    let mut source = source.into_event_stream().context("Couldn't make input device async", ea!())?;
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
                                let last_on = last_buttons[&k];
                                if *on && !last_on {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, button_codes[&k].0, 1));
                                } else if !on && last_on {
                                    dest_events.push(InputEvent::new(EventType::KEY.0, button_codes[&k].0, 0));
                                }
                            }
                            last_buttons = buttons.clone();
                            if dest_events.len() > 0 {
                                dest
                                    .lock()
                                    .unwrap()
                                    .emit(&dest_events)
                                    .context("Failed to send events to virtual device", ea!())?;
                            }
                        }
                    },
                    evdev::EventSummary::Key(_, t, v) => {
                        buttons.insert(t, v != 0);
                    },
                    _ => { },
                }
            }
            return Ok(());
        }
    });
    return Ok(());
}
