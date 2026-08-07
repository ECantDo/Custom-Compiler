use std::collections::HashMap;

use once_cell::sync::Lazy;

use super::pseudo_opcodes;

/// Port of `Assembler.Operations.Opcodes.opcodeMap`.
/// mnemonic -> (argument count, opcode value).
pub static OPCODE_MAP: Lazy<HashMap<&'static str, (i32, i32)>> = Lazy::new(|| {
    HashMap::from([
        ("hlt", (0, 0)),
        ("add", (3, 1)),
        ("sub", (3, 33)),
        ("xor", (3, 65)),
        ("or", (3, 97)),
        ("and", (3, 129)),
        ("xnor", (3, 161)),
        ("nor", (3, 193)),
        ("nand", (3, 225)),
        ("sl", (3, 2)),
        ("sra", (3, 34)),
        ("sr", (3, 66)),
        ("addi", (3, 17)),
        ("addf", (3, 3)),
        ("subi", (3, 49)),
        ("li", (2, 25)),
        ("xori", (3, 81)),
        ("ori", (3, 113)),
        ("andi", (3, 145)),
        ("xnori", (3, 177)),
        ("nori", (3, 209)),
        ("nandi", (3, 241)),
        ("sli", (3, 18)),
        ("srai", (3, 50)),
        ("sri", (3, 82)),
        ("beq", (3, 4)),
        ("bne", (3, 36)),
        ("blt", (3, 68)),
        ("bge", (3, 100)),
        ("bltu", (3, 132)),
        ("bgeu", (3, 164)),
        ("jal", (2, 5)),
        ("jalr", (3, 6)),
        ("lod", (3, 39)),
        ("str", (3, 7)),
        ("in", (2, 8)),
        ("out", (2, 40)),
    ])
});

/// Port of `Opcodes.immediateMap`. The original TODO ("figure out what the
/// heck I was thinking here") is preserved below because I have no more
/// insight into these bit patterns than you did when you wrote it.
static IMMEDIATE_MAP: Lazy<HashMap<&'static str, i32>> = Lazy::new(|| {
    HashMap::from([
        ("hlt", 0),
        ("add", 0),
        ("sub", 0),
        ("xor", 0),
        ("or", 0),
        ("and", 0),
        ("xnor", 0),
        ("nor", 0),
        ("nand", 0),
        ("sl", 0),
        ("sra", 0),
        ("sr", 0),
        ("addi", 0b0100),
        ("addf", 0),
        ("subi", 0b0010),
        ("li", 0b010),
        ("xori", 0b0100),
        ("ori", 0b0100),
        ("andi", 0b0100),
        ("xnori", 0b0100),
        ("nori", 0b0100),
        ("nandi", 0b0100),
        ("sli", 0b0100),
        ("srai", 0b0100),
        ("sri", 0b0100),
        ("beq", 0b1000),
        ("bne", 0b1000),
        ("blt", 0b1000),
        ("bge", 0b1000),
        ("bltu", 0b1000),
        ("bgeu", 0b1000),
        ("jal", 0b100),
        ("jalr", 0b0100),
        ("lod", 0b0100),
        ("str", 0b0100),
        ("in", 0b100), // in rd, port  (matches out's argument order: register first, port second)
        ("out", 0b100),
    ])
});

/// Port of `Opcodes.generateOperation`. Returns `(arg_count, opcode_value)`.
pub fn generate_operation(operation: &str) -> Option<(i32, i32)> {
    OPCODE_MAP.get(operation.to_lowercase().as_str()).copied()
}

/// Port of `Opcodes.operationExists`.
pub fn operation_exists(operation: &str) -> bool {
    let op = operation.to_lowercase();
    if OPCODE_MAP.contains_key(op.as_str()) {
        return true;
    }
    pseudo_opcodes::OPCODE_MAP.contains_key(op.as_str())
}

/// Port of `Opcodes.getImmediateMap`. Java would NPE (auto-unboxing a null
/// `Integer`) if called with a mnemonic missing from the map; `.expect`
/// mirrors that same "this is a programmer error, not recoverable" stance.
pub fn get_immediate_map(operation: &str) -> i32 {
    let op = operation.to_lowercase();
    *IMMEDIATE_MAP
        .get(op.as_str())
        .unwrap_or_else(|| panic!("no immediate-position entry for opcode \"{op}\""))
}
