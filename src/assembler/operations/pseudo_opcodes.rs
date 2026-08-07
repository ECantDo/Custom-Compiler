use std::collections::HashMap;

use once_cell::sync::Lazy;

use crate::assembler::error::AssemblerError;

/// Port of `Assembler.Operations.PseudoOpcodes.opcodeMap`.
/// mnemonic -> template tokens (the real opcode + operand placeholders).
pub static OPCODE_MAP: Lazy<HashMap<&'static str, &'static [&'static str]>> = Lazy::new(|| {
    HashMap::from([
        ("mv", ["add", "zero", "rs", "rd"].as_slice()),
        ("nop", ["add", "zero", "zero", "zero"].as_slice()),
        ("neg", ["sub", "zero", "rs", "rd"].as_slice()),
        ("beqz", ["beq", "rs", "zero", "imm"].as_slice()),
        ("bnez", ["bne", "rs", "zero", "imm"].as_slice()),
        ("blez", ["bge", "zero", "rs", "offset"].as_slice()),
        ("bgez", ["bge", "rs", "zero", "offset"].as_slice()),
        ("bltz", ["blt", "rs", "zero", "offset"].as_slice()),
        ("bgtz", ["blt", "zero", "rs", "offset"].as_slice()),
        ("bgt", ["blt", "rt", "rs", "offset"].as_slice()),
        ("ble", ["bge", "rt", "rs", "offset"].as_slice()),
        ("bgtu", ["bltu", "rt", "rs", "offset"].as_slice()),
        ("bleu", ["bgeu", "rt", "rs", "offset"].as_slice()),
        ("j", ["jal", "zero", "offset"].as_slice()),
        ("jal", ["jal", "ra", "offset"].as_slice()),
        ("ret", ["jalr", "zero", "0", "ra"].as_slice()),
    ])
});

/// Port of `Assembler.Operations.PseudoOpcodes.convertionMap` (their typo,
/// not mine - kept the Rust identifier spelled correctly since nothing
/// outside this file reads the name directly).
static CONVERSION_MAP: Lazy<HashMap<&'static str, &'static [&'static str]>> = Lazy::new(|| {
    HashMap::from([
        ("mv", ["rs", "rd"].as_slice()),
        ("nop", [].as_slice()),
        ("neg", ["rs", "rd"].as_slice()),
        ("beqz", ["rs", "imm"].as_slice()),
        ("bnez", ["rs", "imm"].as_slice()),
        ("blez", ["rs", "imm"].as_slice()),
        ("bgez", ["rs", "imm"].as_slice()),
        ("bltz", ["rs", "imm"].as_slice()),
        ("bgtz", ["rs", "imm"].as_slice()),
        ("bgt", ["rs", "rt", "imm"].as_slice()),
        ("ble", ["rs", "rt", "imm"].as_slice()),
        ("bgtu", ["rs", "rt", "imm"].as_slice()),
        ("bleu", ["rs", "rt", "imm"].as_slice()),
        ("j", ["offset"].as_slice()),
        ("jal", ["offset"].as_slice()),
        ("ret", [].as_slice()),
    ])
});

/// Port of `PseudoOpcodes.convert`. Expands a pseudo-instruction line into
/// its real-instruction equivalent, e.g. `"mv t0 t1"` -> `"add zero t0 t1"`.
pub fn convert(line: &str) -> Result<String, AssemblerError> {
    let line_parts: Vec<&str> = line.trim().split_whitespace().collect();
    let mnemonic = line_parts[0];

    let pseudo_parts = OPCODE_MAP.get(mnemonic).ok_or_else(|| {
        AssemblerError::InvalidPseudoOpcode(format!(
            "Cannot convert \"{line}\", as it is not a pseudo opcode."
        ))
    })?;
    // Java's convertionMap.get() would NPE on a missing key here too, since
    // every entry in opcodeMap has a matching entry in convertionMap - so a
    // plain unwrap mirrors that same "this should never happen" assumption.
    let conversion_parts = CONVERSION_MAP
        .get(mnemonic)
        .expect("opcodeMap/convertionMap out of sync - every pseudo opcode needs both entries");

    let operands = &line_parts[1..];

    let mut operand_map: HashMap<&str, &str> = HashMap::new();
    for (i, part) in conversion_parts.iter().enumerate() {
        operand_map.insert(part, operands[i]);
    }

    let output: Vec<&str> = pseudo_parts
        .iter()
        .map(|part| *operand_map.get(part).unwrap_or(part))
        .collect();

    Ok(output.join(" "))
}

pub fn operation_exists(operation: &str) -> bool {
    OPCODE_MAP.contains_key(operation.to_lowercase().as_str())
}

/// Number of operands the pseudo-opcode `operation` expects, or `None` if
/// it isn't a pseudo opcode at all.
pub fn expected_arg_count(operation: &str) -> Option<usize> {
    CONVERSION_MAP.get(operation.to_lowercase().as_str()).map(|parts| parts.len())
}
