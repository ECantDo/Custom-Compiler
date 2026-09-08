use std::cell::RefCell;
use std::rc::Rc;

use minifb::{Window, WindowOptions};

use super::IoInterface;
use crate::cpu::{UnsignedWord, Word};

/// Port of `CPU.Peripherals.Screen.ScreenInterface`.
pub trait ScreenInterface: IoInterface {
	fn put_pixel(&mut self, x: usize, y: usize, color: bool);
	fn clear(&mut self);
	fn update(&mut self);
}

pub const PIXEL_OFF: u32 = 0x0045_260b;
pub const PIXEL_ON: u32 = 0x0097_743e;

/// Port of `CPU.Peripherals.Screen.JavaScreen`, targeting a real desktop
/// window via `minifb` instead of Swing. `minifb` has no automatic
/// repaint the way Swing's `paintComponent` does, so this rasterizes the
/// grid into a pixel buffer and pushes it to the window on every
/// `update()` call - which lines up with how the original already worked
/// (it calls `panel.repaint()` at the end of `update()`, not on a timer).
pub struct MinifbScreen {
	rows: usize,
	columns: usize,
	cell_size: usize,

	grid_colors_buffer: Vec<Vec<u32>>,
	grid_colors: Vec<Vec<u32>>,

	pixel_buffer: Vec<u32>,
	window: Window,
}

impl MinifbScreen {
	/// # Arguments
	/// * `cell_square_width` - pixel width/height of each drawn "pixel".
	/// * `grid_square_width` - number of rows and columns (grid is square).
	pub fn new(cell_square_width: usize, grid_square_width: usize) -> Result<Self, minifb::Error> {
		let cell_size = cell_square_width;
		let columns = grid_square_width;
		let rows = grid_square_width;

		let window_width = columns * cell_size;
		let window_height = rows * cell_size;

		let mut window = Window::new(
			"Redstone Lamp Display",
			window_width,
			window_height,
			WindowOptions {
				resize: false,
				..WindowOptions::default()
			},
		)?;
		// Cap polling between explicit update() calls instead of spinning.
		window.set_target_fps(60);

		let mut screen = Self {
			rows,
			columns,
			cell_size,
			grid_colors_buffer: vec![vec![PIXEL_OFF; columns]; rows],
			grid_colors: vec![vec![PIXEL_OFF; columns]; rows],
			pixel_buffer: vec![PIXEL_OFF; window_width * window_height],
			window,
		};

		// Last to be called, matching the Java constructor's ordering.
		screen.set_all(false);
		screen.update();

		Ok(screen)
	}

	/// Whether the window is still open. Java relied on `EXIT_ON_CLOSE` to
	/// kill the whole process on close; `minifb` has no such hook, so
	/// callers should check this and react (e.g. stop the CPU loop).
	// pub fn is_open(&self) -> bool {
	// 	self.window.is_open()
	// }

	fn set_all(&mut self, color: bool) {
		let color_value = if color { PIXEL_ON } else { PIXEL_OFF };
		for row in self.grid_colors_buffer.iter_mut() {
			for cell in row.iter_mut() {
				*cell = color_value;
			}
		}
	}

	fn rasterize(&mut self) {
		let window_width = self.columns * self.cell_size;
		for x in 0..self.rows {
			for y in 0..self.columns {
				let color = self.grid_colors[x][y];
				for dy in 0..self.cell_size {
					let py = x * self.cell_size + dy;
					let row_start = py * window_width + y * self.cell_size;
					for dx in 0..self.cell_size {
						self.pixel_buffer[row_start + dx] = color;
					}
				}
			}
		}
	}
}

impl ScreenInterface for MinifbScreen {
	/// Matches the Java original's (swapped) indexing: `gridColorsBuffer[y][x]`.
	fn put_pixel(&mut self, x: usize, y: usize, color: bool) {
		self.grid_colors_buffer[y][x] = if color { PIXEL_ON } else { PIXEL_OFF };
	}

	fn clear(&mut self) {
		self.set_all(false);
	}

	/// Moves the buffer into the live grid and pushes a frame to the window.
	fn update(&mut self) {
		for x in 0..self.grid_colors.len() {
			self.grid_colors[x].copy_from_slice(&self.grid_colors_buffer[x]);
		}
		self.rasterize();

		let window_width = self.columns * self.cell_size;
		let window_height = self.rows * self.cell_size;
		// Best-effort: if the window's already gone, there's nothing to draw to.
		let _ = self
			.window
			.update_with_buffer(&self.pixel_buffer, window_width, window_height);
	}
}

impl IoInterface for MinifbScreen {
	fn io_input(&mut self, data: UnsignedWord) {
		let data = data as i32;
		let x_coord = (data & 0x3F) as usize;
		let y_coord = ((data >> 8) & 0x3F) as usize;
		let opcode = (data >> 14) & 0b11;

		match opcode {
			0b00 => self.put_pixel(x_coord, y_coord, true),
			0b01 => self.put_pixel(x_coord, y_coord, false),
			0b10 => self.clear(),
			_ => self.update(), // 0b11, "always true as there is no other option"
		}
	}

	fn io_output(&mut self) -> Word {
		0
	}
}

// ---------------------------------------------------------------------------
// RGB colour display
// ---------------------------------------------------------------------------
//
// A colour sibling of `MinifbScreen`. Same 64x64 lamp grid, same instruction
// word layout, but each lamp can show an arbitrary colour instead of just
// on/off. Colours are 4 bits per channel (RGB444), fed in through a separate
// IO port that behaves as a 12-bit shift register.
//
// Unlike the b/w screen there is no back buffer here yet - every draw op
// paints straight to the window. `Load Buffer` (op `0b11`) latches the shift
// register into the active draw colour instead of pushing a frame.
//
// Wiring (see `MinifbColorScreen`):
//   * colour port       - `io_input` shifts the low nibble of the word in:
//                         `shift = (shift << 4) | (word & 0xF)`, keeping 12
//                         bits. Write R, then G, then B to load `0xRGB`.
//   * instruction port  - identical word layout to `MinifbScreen`:
//                             byte 0        : X coordinate (bits 0..5)
//                             byte 1 low    : Y coordinate (bits 8..13)
//                             byte 1 top 2  : op (bits 14..15)
//                         ops: 00 pixel on (active colour) · 01 pixel off
//                              10 clear screen · 11 load buffer

/// Packed value for an "off" lamp: black.
pub const COLOR_PIXEL_OFF: u32 = 0x0000_0000;

/// Expand a 4-bit channel (0..=15) to 8 bits, linearly: `0x0 -> 0`, `0xF -> 255`.
fn channel4_to_8(c: u32) -> u32 {
	(c & 0xF) * 17
}

/// Turn a 12-bit `0xRGB` value (4 bits per channel) into a packed `0x00RRGGBB`.
fn rgb444_to_u32(rgb444: u16) -> u32 {
	let rgb444 = rgb444 as u32;
	let r = channel4_to_8(rgb444 >> 8);
	let g = channel4_to_8(rgb444 >> 4);
	let b = channel4_to_8(rgb444);
	(r << 16) | (g << 8) | b
}

struct ColorScreenCore {
	rows: usize,
	columns: usize,
	cell_size: usize,

	grid: Vec<Vec<u32>>,
	pixel_buffer: Vec<u32>,
	window: Window,

	/// 12-bit RGB444 shift register, filled a nibble at a time via the
	/// colour port.
	shift: u16,
	/// Packed `0x00RRGGBB` colour used by "pixel on". Latched from `shift`
	/// by the `Load Buffer` op.
	active_color: u32,
}

impl ColorScreenCore {
	fn new(cell_square_width: usize, grid_square_width: usize) -> Result<Self, minifb::Error> {
		let cell_size = cell_square_width;
		let columns = grid_square_width;
		let rows = grid_square_width;

		let window_width = columns * cell_size;
		let window_height = rows * cell_size;

		let mut window = Window::new(
			"RGB Lamp Display",
			window_width,
			window_height,
			WindowOptions {
				resize: false,
				..WindowOptions::default()
			},
		)?;
		window.set_target_fps(60);

		let mut screen = Self {
			rows,
			columns,
			cell_size,
			grid: vec![vec![COLOR_PIXEL_OFF; columns]; rows],
			pixel_buffer: vec![COLOR_PIXEL_OFF; window_width * window_height],
			window,
			shift: 0,
			active_color: 0x00FF_FFFF,
		};

		screen.present();
		Ok(screen)
	}

	/// Colour port: shift the low nibble of `word` into the RGB444 register.
	fn shift_in(&mut self, word: UnsignedWord) {
		self.shift = ((self.shift << 4) | (word & 0xF)) & 0x0FFF;
	}

	/// Instruction port: same word layout as [`MinifbScreen::io_input`].
	fn instruction(&mut self, data: UnsignedWord) {
		let data = data as i32;
		let x_coord = (data & 0x3F) as usize;
		let y_coord = ((data >> 8) & 0x3F) as usize;
		let opcode = (data >> 14) & 0b11;

		match opcode {
			0b00 => {
				self.put_pixel(x_coord, y_coord, true);
				self.present();
			}
			0b01 => {
				self.put_pixel(x_coord, y_coord, false);
				self.present();
			}
			0b10 => {
				self.clear();
				self.present();
			}
			// 0b11 - Load Buffer: latch the shift register into the draw colour.
			_ => self.active_color = rgb444_to_u32(self.shift),
		}
	}

	/// Matches the b/w screen's swapped indexing: `grid[y][x]`.
	fn put_pixel(&mut self, x: usize, y: usize, on: bool) {
		self.grid[y][x] = if on { self.active_color } else { COLOR_PIXEL_OFF };
	}

	fn clear(&mut self) {
		for row in self.grid.iter_mut() {
			for cell in row.iter_mut() {
				*cell = COLOR_PIXEL_OFF;
			}
		}
	}

	fn rasterize(&mut self) {
		let window_width = self.columns * self.cell_size;
		for x in 0..self.rows {
			for y in 0..self.columns {
				let color = self.grid[x][y];
				for dy in 0..self.cell_size {
					let py = x * self.cell_size + dy;
					let row_start = py * window_width + y * self.cell_size;
					for dx in 0..self.cell_size {
						self.pixel_buffer[row_start + dx] = color;
					}
				}
			}
		}
	}

	/// Rasterize the grid and push a frame to the window.
	fn present(&mut self) {
		self.rasterize();

		let window_width = self.columns * self.cell_size;
		let window_height = self.rows * self.cell_size;
		let _ = self
			.window
			.update_with_buffer(&self.pixel_buffer, window_width, window_height);
	}
}

/// A 64x64 RGB444 lamp display. Spans two IO ports - a colour shift register
/// and an instruction port - so it hands out one [`IoInterface`] per port,
/// both backed by the same window.
///
/// ```ignore
/// let screen = MinifbColorScreen::new(16, 64)?;
/// io.attach(0, screen.buffer_port());
/// io.attach(1, screen.instruction_port());
/// ```
pub struct MinifbColorScreen {
	core: Rc<RefCell<ColorScreenCore>>,
}

impl MinifbColorScreen {
	/// # Arguments
	/// * `cell_square_width` - pixel width/height of each drawn lamp.
	/// * `grid_square_width` - number of rows and columns (grid is square).
	pub fn new(cell_square_width: usize, grid_square_width: usize) -> Result<Self, minifb::Error> {
		let core = ColorScreenCore::new(cell_square_width, grid_square_width)?;
		Ok(Self { core: Rc::new(RefCell::new(core)) })
	}

	/// The colour shift-register port. Each `io_input` shifts one 4-bit
	/// channel in (`R`, then `G`, then `B`).
	pub fn buffer_port(&self) -> Box<dyn IoInterface> {
		Box::new(ColorBufferPort { core: Rc::clone(&self.core) })
	}

	/// The instruction port. Word layout matches the b/w screen.
	pub fn instruction_port(&self) -> Box<dyn IoInterface> {
		Box::new(ColorInstructionPort { core: Rc::clone(&self.core) })
	}
}

struct ColorBufferPort {
	core: Rc<RefCell<ColorScreenCore>>,
}

impl IoInterface for ColorBufferPort {
	fn io_input(&mut self, data: UnsignedWord) {
		self.core.borrow_mut().shift_in(data);
	}

	fn io_output(&mut self) -> Word {
		0
	}
}

struct ColorInstructionPort {
	core: Rc<RefCell<ColorScreenCore>>,
}

impl IoInterface for ColorInstructionPort {
	fn io_input(&mut self, data: UnsignedWord) {
		self.core.borrow_mut().instruction(data);
	}

	fn io_output(&mut self) -> Word {
		0
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn channel_expansion_is_linear_over_the_full_range() {
		assert_eq!(channel4_to_8(0x0), 0);
		assert_eq!(channel4_to_8(0x8), 136);
		assert_eq!(channel4_to_8(0xF), 255);
		// Only the low nibble matters.
		assert_eq!(channel4_to_8(0x1F0 | 0xF), 255);
	}

	#[test]
	fn rgb444_packs_into_rrggbb() {
		assert_eq!(rgb444_to_u32(0x000), 0x0000_0000);
		assert_eq!(rgb444_to_u32(0xFFF), 0x00FF_FFFF);
		assert_eq!(rgb444_to_u32(0xF00), 0x00FF_0000);
		assert_eq!(rgb444_to_u32(0x0F0), 0x0000_FF00);
		assert_eq!(rgb444_to_u32(0x00F), 0x0000_00FF);
		// R=1 G=8 B=15 -> 17, 136, 255
		assert_eq!(rgb444_to_u32(0x18F), 0x0011_88FF);
	}

	#[test]
	fn shift_register_keeps_last_three_nibbles() {
		// Reproduce the shift formula the colour port uses.
		let mut shift: u16 = 0;
		let write = |shift: &mut u16, word: u16| {
			*shift = ((*shift << 4) | (word & 0xF)) & 0x0FFF;
		};

		write(&mut shift, 0x1); // R
		write(&mut shift, 0x8); // G
		write(&mut shift, 0xF); // B
		assert_eq!(shift, 0x18F);

		// A fourth write pushes the oldest nibble out.
		write(&mut shift, 0x2);
		assert_eq!(shift, 0x8F2);

		// High bits of the word are ignored.
		write(&mut shift, 0xAB3);
		assert_eq!(shift, 0xF23);
	}
}