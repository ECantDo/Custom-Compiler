use crate::cpu::{UnsignedWord, Word};

mod config;
mod screen;

use config::{PeripheralConfig, ScreenKind};
pub use screen::{MinifbColorScreen, MinifbScreen};

/// Port of `CPU.Peripherals.IOInterface`.
pub trait IoInterface {
	fn io_input(&mut self, data: UnsignedWord);
	fn io_output(&mut self) -> Word;
}

/// Port of `CPU.IO`.
pub struct IoPorts {
	io_ports: [Option<Box<dyn IoInterface>>; Self::NUM],
}

impl IoPorts {
	const NUM: usize = 16;

	/// No devices attached. Java has no equivalent - its constructor
	/// always wires up port 0 - but this is handy for headless
	/// runs/tests, or for building your own port map from scratch.
	pub fn empty() -> Self {
		Self { io_ports: Default::default() }
	}

	/// Attaches a real display, picked by `peripherals.cfg` (see
	/// [`PeripheralConfig`]). Historically Java hardcoded
	/// `new JavaScreen(16, 64)` on port 0; that is still the default when
	/// no config file is present.
	///
	/// * `screen = bw`    - monochrome lamp display on port 0.
	/// * `screen = color` - RGB444 display: colour shift-register port on 0,
	///   instruction port on 1.
	///
	/// Returns a `Result` (unlike Java) since opening a real window can
	/// fail - no display available, etc. - or the config can be malformed.
	pub fn new() -> Result<Self, String> {
		let config = PeripheralConfig::load_default()?;
		let mut io = Self::empty();

		match config.screen {
			ScreenKind::Bw => {
				let screen = MinifbScreen::new(16, 64).map_err(|e| e.to_string())?;
				io.attach(0, Box::new(screen));
			}
			ScreenKind::Color => {
				let screen = MinifbColorScreen::new(16, 64).map_err(|e| e.to_string())?;
				io.attach(0, screen.buffer_port());
				io.attach(1, screen.instruction_port());
			}
		}

		Ok(io)
	}

	/// Not present in the Java original, which only ever populates port 0
	/// in the constructor. Lets you wire up any port to any device.
	pub fn attach(&mut self, port: usize, device: Box<dyn IoInterface>) {
		self.io_ports[port & 0xF] = Some(device);
	}

	/// Port of `IO.ioOutput`.
	pub fn output(&mut self, data: UnsignedWord, port: UnsignedWord) {
		let port = (port & 0xF) as usize; // 4 bit for port
		let data = data & 0xFFFF; // 16 bit being sent - a no-op at this width, kept for parity with the Java mask

		match self.io_ports[port].as_mut() {
			Some(device) => device.io_input(data),
			None => panic!("Cannot output to port {port} because nothing exists for that port"),
		}
	}

	/// Port of `IO.ioInput`.
	pub fn input(&mut self, port: UnsignedWord) -> Word {
		let port = (port & 0xF) as usize; // 4 bit for port
		match self.io_ports[port].as_mut() {
			// Java: `(byte) ioOutput()` - truncate to the low 8 bits, then
			// sign-extend back out. `Word as i8 as Word` does the same.
			Some(device) => device.io_output() as i8 as Word,
			None => 0,
		}
	}
}

impl Default for IoPorts {
	fn default() -> Self {
		Self::empty()
	}
}