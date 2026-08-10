use std::collections::HashMap;
use std::fs;
use std::process::Command;

use super::error::AssemblerError;
use super::operations::opcodes;
use super::operations::pseudo_opcodes;
use super::operations::registers;

/// Port of `Build.runBuildToSchem`. Shells out to the same `.bat` ->
/// Python pipeline as the original - deliberately NOT reimplemented here,
/// per your instructions to leave the MCSchematic side alone. This is as
/// Windows-specific as the original (`cmd /c start ...`); if you ever run
/// the Rust build cross-platform you'll want to branch on `cfg!(windows)`
/// here, but I didn't change that behavior since it wasn't asked for.
pub fn run_build_to_schem(file_path: &str) {
    let directory = std::env::current_dir()
        .map(|d| d.join("src").join("Assembler").join("runBuildToSchem.bat"))
        .unwrap_or_default();

    let result = Command::new("cmd")
        .args(["/c", "start"])
        .arg(directory)
        .arg(file_path)
        .spawn();

    if let Err(e) = result {
        eprintln!("Failed to run .bat\n{e}");
    }
}

/// Port of `Build.build`. Reads a `.as` source file, assembles it, writes
/// the `.bin` output next to it, and returns the assembled machine code.
pub fn build(contents: &String) -> Result<Vec<u32>, AssemblerError> {
	let raw_contents: Vec<String> = contents.lines().map(|s| s.to_string()).collect();
    // let raw_contents = read_file(file_path)?;
    let file_contents = format_file(&raw_contents);
    let labels = get_labels(&file_contents);
    let constants = get_constants(&file_contents)?;

    let mut file_contents = file_contents;
    replace_constants(&mut file_contents, &constants);

    println!("Replaced Constants:");
    for line in &file_contents {
        println!("{line}");
    }

    let instructions = assemble(&file_contents, &labels)?;

    Ok(instructions)
}

/// Port of the private `Build.assemble` method - the heart of the
/// assembler. Mirrors the original's bit-packing logic instruction by
/// instruction; see the inline comments in the Java source for what each
/// case in the switch/match represents (opcode encoding by instruction
/// class).
pub fn assemble(file_contents: &[String], labels: &HashMap<String, i32>) -> Result<Vec<u32>, AssemblerError> {
    println!("{labels:?}");

    let mut instructions: Vec<u32> = Vec::new();
    let mut line_number: i32 = 0;

    for line in file_contents {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("def ") {
            continue;
        }

        let mut parts: Vec<String> = line.split(' ').map(|s| s.to_string()).collect();
        let provided_args = parts.len().saturating_sub(1);

        // A mnemonic existing in the pseudo table isn't enough on its own -
        // "jal" is both a pseudo op (1 operand: `jal offset`) and a real
        // opcode (2 operands: `jal rd, offset`). Only expand as a pseudo
        // op when the operand count actually matches; otherwise let it
        // fall through to the real opcode below.
        if pseudo_opcodes::expected_arg_count(&parts[0]) == Some(provided_args) {
            let converted = pseudo_opcodes::convert(line)?;
            parts = converted.split(' ').map(|s| s.to_string()).collect();
        }

        let mnemonic = &parts[0];

        if mnemonic.ends_with(':') {
            // Skip labels
            continue;
        }

        // (arg count, opcode value)
        let args = opcodes::generate_operation(mnemonic)
            .ok_or_else(|| AssemblerError::UnknownOpcode(format!("Opcode \"{mnemonic}\" does not exist")))?;

        if (parts.len() as i32) - 1 != args.0 {
            return Err(AssemblerError::InvalidArgument(format!(
                "Expected argument count does not match received count\n-> {line}"
            )));
        }

        let immediate_position = opcodes::get_immediate_map(mnemonic);

        let mut instruction_values: Vec<i32> = vec![0; parts.len()];
        let mut current_position: i32 = 1;
        let mut immediate_value: i32 = 0;

        // Convert the words into numbers
        for i in 0..instruction_values.len() {
            let part = &parts[i];
            if (current_position & immediate_position) != 0 {
                match labels.get(part) {
                    Some(&label_line_number) if label_line_number >= 0 => {
                        immediate_value = label_line_number;

						// Jal just goes to the target location; not an offset
						if instruction_values[0] & 0b11111 != 5 {
							immediate_value -= line_number - 1;
						}
                    }
                    _ => {
                        immediate_value = parse_int(part)?;
                    }
                }
            } else if i == 0 {
                instruction_values[i] = args.1;
            } else {
                instruction_values[i] = registers::get_register_value(part);
            }

            current_position <<= 1;
        }

        // Put all the bits into the correct location(s)
        let mut operation_value: i32 = 0;
        operation_value |= instruction_values[0]; // Opcode always takes the first 8 bits

        match operation_value & 0b11111 {
            0 => {
                // Halt - operation_value stays 0
            }
            // Without immediate...
            1 | 2 | 3 => {
                // ALU (add, sub, xor, and, or, ...) / Barrel Shifter / Add with flags
                // RD, first 5 bits of byte 2
                operation_value |= (instruction_values[3] & 0b11111) << 8;
                // RS1, last 3 bits of byte 2, first 2 bits of byte 3
                operation_value |= (instruction_values[1] & 0b11111) << 13;
                // RS2, 5 bits of byte 3, starting 3 bits in
                operation_value |= (instruction_values[2] & 0b11111) << 18;
            }
            25 => {
                // Load Immediate
                // RD, first 5 bits of byte 2
                operation_value |= (instruction_values[2] & 0b11111) << 8;
                // bit 14 & 15 -> first 2 bits of byte 3
                operation_value |= ((immediate_value >> 14) & 0b11) << 16;
                // Remaining imm bits starts 3rd bit into byte 3
                operation_value |= (immediate_value & 0x3FFF) << 18;
            }
			5 => {
				// Jump and link
				// RD, first 5 bits of byte 2
				operation_value |= (instruction_values[1] & 0b11111) << 8;
				// immediate_value += 1;
				// bit 14 & 15 -> first 2 bits of byte 3
				operation_value |= ((immediate_value >> 14) & 0b11) << 16;
				// Remaining imm bits starts 3rd bit into byte 3
				operation_value |= (immediate_value & 0x3FFF) << 18;
			}
			6 => {
				// Jump and link Register
				// RD, first 5 bits of byte 2
				operation_value |= (instruction_values[1] & 0b11111) << 8;

				// RS1
				operation_value |= (instruction_values[3] & 0b11111) << 13;

				// immediate_value += 1;

				// Remaining imm bits starts 3rd bit into byte 3
				operation_value |= (immediate_value & 0x3FFF) << 18;
			}
			code @ (18 | 17 | 7) => {
                // 18: Shifter - immediate needs to be only 4 bits long, then
                // falls through to the same encoding as 17/6/7.
                if code == 18 {
                    immediate_value &= 0xF;
                }
                // ALU with imm / Jump and Link Register / Store/load
                // RD, first 5 bits of byte 2
                operation_value |= (instruction_values[3] & 0b11111) << 8;
                // RS1, last 3 bits of byte 2, first 2 bits of byte 3
                operation_value |= (instruction_values[1] & 0b11111) << 13;
                // Imm bits starts 3rd bit into byte 3
                operation_value |= (immediate_value & 0x3FFF) << 18;
            }
            4 => {
                // Branching
                // RS1, last 3 bits of byte 2, first 2 bits of byte 3
                operation_value |= (instruction_values[1] & 0b11111) << 13;
                // RS2, 5 bits of byte 3, starting 3 bits in
                operation_value |= (instruction_values[2] & 0b11111) << 18;
                // Bits [0:4] take the place of RD
                operation_value |= (immediate_value & 0x1F) << 8;
                // Bits [5:13] take the rest of the instruction
                operation_value |= (immediate_value >> 5) << 23;
            }
            8 => {
                // IO ports
                immediate_value &= 0xF;
                // Imm bits starts 3rd bit into byte 3
                operation_value |= (immediate_value & 0x3FFF) << 18;

                if instruction_values[0] >> 5 == 0 {
                    // in: rd -> bit 0 of byte 2
                    operation_value |= (instruction_values[1] & 0x1F) << 8;
                } else {
                    // out: rs1 -> bit 5 of byte 2
                    operation_value |= (instruction_values[1] & 0x1F) << 13;
                }
            }
            other => {
                return Err(AssemblerError::UnsupportedEncoding(format!(
                    "Unimplemented encoding for function {other}"
                )));
            }
        }

        instructions.push(operation_value as u32);
        line_number += 1;
    }

    Ok(instructions)
}

/// Port of `Build.parseInt`. Accepts decimal, `0x` hex, `0b` binary, and
/// leading-zero octal literals.
pub fn parse_int(s: &str) -> Result<i32, AssemblerError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(AssemblerError::InvalidArgument("Value cannot be empty".to_string()));
    }

    let (digits, radix) = if let Some(rest) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        (rest, 16)
    } else if let Some(rest) = trimmed.strip_prefix("0b").or_else(|| trimmed.strip_prefix("0B")) {
        (rest, 2)
    } else if trimmed.starts_with('0') && trimmed.len() > 1 {
        (&trimmed[1..], 8)
    } else {
        (trimmed, 10)
    };

    i32::from_str_radix(digits, radix)
        .map_err(|_| AssemblerError::InvalidArgument(format!("Value '{digits}' is not a number")))
}

/// Port of `Build.getLabels`.
fn get_labels(file_contents: &[String]) -> HashMap<String, i32> {
    let mut labels: HashMap<String, i32> = HashMap::new();
    let mut address: i32 = 0;

    for line in file_contents {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("def ") {
            continue;
        }
        if let Some(idx) = line.find(':') {
            labels.insert(line[..idx].to_string(), address);
            continue;
        }
        address += 1;
    }

    labels
}

/// Port of `Build.getConstants`. Note: the original calls `System.exit(-1)`
/// on a bad constant line, which would also skip any pending writes/cleanup.
/// Here that's replaced with a returned `Err`, which the caller can act on
/// however it likes (including exiting) - see the doc comment on
/// `AssemblerError` for why.
fn get_constants(file_contents: &[String]) -> Result<HashMap<String, i32>, AssemblerError> {
    let mut constants = HashMap::new();

    for (i, raw_line) in file_contents.iter().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("def ") {
            continue;
        }

        let split: Vec<&str> = line.split(' ').collect();

        if opcodes::operation_exists(split[1]) {
            return Err(AssemblerError::InvalidArgument(format!(
                "Constant \"{}\" is a valid opcode, not a constant; Constants cannot be opcodes\nLine: {} ({})",
                split[1], line, i
            )));
        }

        let value = parse_int(split[2]).map_err(|_| {
            AssemblerError::InvalidArgument(format!(
                "Value {} is not a number, expected a constant\nLine: {} ({})",
                split[2], line, i
            ))
        })?;

        constants.insert(split[1].to_string(), value);
    }

    Ok(constants)
}

/// Port of `Build.replaceConstants`. Mutates `file_contents` in place,
/// substituting every occurrence of a constant's name with its value.
fn replace_constants(file_contents: &mut [String], constants: &HashMap<String, i32>) {
    for (key, value) in constants {
        for line in file_contents.iter_mut() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("def ") {
                continue;
            }

            let parts: Vec<&str> = trimmed.split(' ').map(|s| s).collect();
            if parts.iter().any(|part| part == key) {
                *line = line.replace(key.as_str(), &value.to_string());
            }
        }
    }
}

/// Port of `Build.formatFile`. Strips `//` comments, strips parentheses,
/// and collapses runs of whitespace into single spaces.
pub fn format_file(file_contents: &[String]) -> Vec<String> {
    file_contents
        .iter()
        .map(|line| {
            let line = match line.find("//") {
                Some(idx) => &line[..idx],
                None => line.as_str(),
            };
            let no_parens: String = line.chars().map(|c| if c == '(' || c == ')' { ' ' } else { c }).collect();
            no_parens.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// Port of `Build.readFile`. On failure, the original prints an error and
/// calls `System.exit(-1)`; kept that same fatal behavior here rather than
/// threading it through `Result`, since a missing source file isn't
/// something the rest of the pipeline can meaningfully recover from.
pub fn read_file(file_path: &str) -> Result<Vec<String>, AssemblerError> {
    match fs::read_to_string(file_path) {
        Ok(contents) => Ok(contents.lines().map(|s| s.to_string()).collect()),
        Err(e) => {
            eprintln!("An error occurred. File not found.");
            eprintln!("{e}");
            std::process::exit(-1);
        }
    }
}
