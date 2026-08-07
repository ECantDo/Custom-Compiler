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
		window.limit_update_rate(Some(std::time::Duration::from_micros(16_600)));

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
	pub fn is_open(&self) -> bool {
		self.window.is_open()
	}

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