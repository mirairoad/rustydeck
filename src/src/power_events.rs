use psp::monitor::{PowerMonitor, PowerState};

pub fn init_power_events() {
	let power_monitor = Box::leak(Box::new(PowerMonitor::new()));
	let receiver = power_monitor.event_receiver();
	if let Err(error) = power_monitor.start_listening() {
		log::error!("Failed to start listening for power events: {error}");
		return;
	}

	std::thread::spawn(move || {
		while let Ok(event) = receiver.recv() {
			match event {
				PowerState::ScreenLocked => {
					crate::spawn(async {
						if let Err(error) = crate::device_sleep::sleep_for_computer_lock().await {
							log::error!("Failed to sleep devices due to screen lock: {error}");
						}
					});
				}
				PowerState::ScreenUnlocked => {
					crate::spawn(async {
						if let Err(error) = crate::device_sleep::wake_from_computer_lock().await {
							log::error!("Failed to wake devices due to screen unlock: {error}");
						}
					});
				}
				// Waking used to be broadcast to plugins; nothing listens for it now.
				PowerState::Resume => {}
				PowerState::Suspend | PowerState::Shutdown | PowerState::Unknown => {}
			}
		}
	});
}
