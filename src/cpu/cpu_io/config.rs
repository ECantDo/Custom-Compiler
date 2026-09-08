//! Loads `peripherals.cfg` - the emulator's IO peripheral wiring.
//!
//! Format is one `key = value` per line, `#` starts a comment (whole-line
//! or trailing), blank lines are ignored. Same working-directory-relative
//! convention as `registers.cfg`.

use std::fs;
use std::path::Path;

/// Default location, relative to the process working directory.
pub const DEFAULT_PATH: &str = "peripherals.cfg";

/// Which display the emulator attaches on start-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenKind {
	/// 64x64 monochrome redstone lamp display on port 0.
	#[default]
	Bw,
	/// 64x64 RGB444 display: colour shift-register port on 0, instruction
	/// port on 1.
	Color,
}

#[derive(Debug, Clone, Default)]
pub struct PeripheralConfig {
	pub screen: ScreenKind,
}

impl PeripheralConfig {
	/// Load from [`DEFAULT_PATH`]. A missing file is not an error - it just
	/// yields the defaults (b/w screen), preserving the historical wiring.
	pub fn load_default() -> Result<Self, String> {
		match fs::read_to_string(DEFAULT_PATH) {
			Ok(text) => Self::parse(&text),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
			Err(e) => Err(format!("could not read `{DEFAULT_PATH}`: {e}")),
		}
	}

	/// Read and parse the config at `path`.
	#[allow(dead_code)]
	pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
		let path = path.as_ref();
		let text = fs::read_to_string(path)
			.map_err(|e| format!("could not read peripheral config `{}`: {e}", path.display()))?;
		Self::parse(&text)
	}

	pub fn parse(text: &str) -> Result<Self, String> {
		let mut config = Self::default();

		for (lineno, raw) in text.lines().enumerate() {
			let lineno = lineno + 1;

			let line = raw.split('#').next().unwrap_or("").trim();
			if line.is_empty() {
				continue;
			}

			let (key, value) = line
				.split_once('=')
				.ok_or_else(|| format!("line {lineno}: expected `key = value`, got `{line}`"))?;
			let key = key.trim();
			let value = value.trim();

			match key {
				"screen" => {
					config.screen = match value {
						"bw" | "b/w" | "mono" => ScreenKind::Bw,
						"color" | "colour" | "rgb" => ScreenKind::Color,
						other => {
							return Err(format!(
								"line {lineno}: unknown screen `{other}` (expected `bw` or `color`)"
							))
						}
					};
				}
				other => return Err(format!("line {lineno}: unknown key `{other}`")),
			}
		}

		Ok(config)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn empty_and_comment_only_yields_defaults() {
		let cfg = PeripheralConfig::parse("# just a comment\n\n   \n").unwrap();
		assert_eq!(cfg.screen, ScreenKind::Bw);
	}

	#[test]
	fn parses_screen_kinds_with_aliases_and_trailing_comments() {
		assert_eq!(
			PeripheralConfig::parse("screen = color").unwrap().screen,
			ScreenKind::Color
		);
		assert_eq!(
			PeripheralConfig::parse("screen=rgb  # the new hotness").unwrap().screen,
			ScreenKind::Color
		);
		assert_eq!(
			PeripheralConfig::parse("screen = bw").unwrap().screen,
			ScreenKind::Bw
		);
	}

	#[test]
	fn rejects_unknown_keys_and_values() {
		assert!(PeripheralConfig::parse("screen = plasma").is_err());
		assert!(PeripheralConfig::parse("scren = bw").is_err());
		assert!(PeripheralConfig::parse("screen bw").is_err());
	}
}
