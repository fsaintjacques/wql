//! Execute a WQL program through a WASM module via wasmtime.

use wasmtime::{Engine, Linker, Memory, Module, Store, TypedFunc};

const WASM_TEMPLATE: &[u8] = include_bytes!("../data/template.wasm");
const WASM_SENTINEL: &[u8; 16] = b"WQLSLOT!WQLSLOT!";
const WASM_SLOT_SIZE: usize = 8192;
const WASM_PROGRAM_OFFSET: usize = 16;

/// A WQL program loaded into a WASM instance, ready to evaluate messages.
pub struct WasmProgram {
    store: Store<()>,
    memory: Memory,
    wql_eval: TypedFunc<(u32, u32, u32, u32), i64>,
    heap_base: u32,
}

/// Result of evaluating a message through the WASM module.
pub struct WasmEvalResult {
    pub matched: bool,
}

impl WasmProgram {
    /// Compile WQL bytecode into a patched WASM module and instantiate it.
    pub fn new(bytecode: &[u8]) -> Result<Self, String> {
        let wasm_bytes = patch_template(bytecode)?;

        let engine = Engine::default();
        let module = Module::new(&engine, &wasm_bytes).map_err(|e| format!("wasm load: {e}"))?;
        let linker = Linker::new(&engine);
        let mut store = Store::new(&engine, ());
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("wasm instantiate: {e}"))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or("wasm module missing 'memory' export")?;

        let wql_eval = instance
            .get_typed_func::<(u32, u32, u32, u32), i64>(&mut store, "wql_eval")
            .map_err(|e| format!("wasm missing wql_eval: {e}"))?;

        #[allow(clippy::cast_sign_loss)]
        let heap_base = instance
            .get_global(&mut store, "__heap_base")
            .ok_or("wasm module missing '__heap_base' export")?
            .get(&mut store)
            .i32()
            .ok_or("__heap_base is not i32")? as u32;

        Ok(Self {
            store,
            memory,
            wql_eval,
            heap_base,
        })
    }

    /// Evaluate the program on input bytes.
    /// Returns the eval result and, if matched with projection, the output bytes.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn eval(&mut self, input: &[u8]) -> Result<(WasmEvalResult, Vec<u8>), String> {
        let in_ptr = self.heap_base;
        let in_len = input.len() as u32;
        let out_ptr = in_ptr + in_len;
        let out_len = in_len * 2 + 256;
        let needed = (out_ptr + out_len) as usize;

        // Grow memory if needed.
        let current = self.memory.data_size(&self.store);
        if needed > current {
            let pages = (needed - current).div_ceil(65536) as u64;
            self.memory
                .grow(&mut self.store, pages)
                .map_err(|e| format!("wasm memory grow: {e}"))?;
        }

        self.memory
            .write(&mut self.store, in_ptr as usize, input)
            .map_err(|e| format!("wasm write input: {e}"))?;

        let result = self
            .wql_eval
            .call(&mut self.store, (in_ptr, in_len, out_ptr, out_len))
            .map_err(|e| format!("wasm eval: {e}"))?;

        if result == -2 {
            return Err("wasm wql_eval returned runtime error (-2)".into());
        }

        if result == -1 {
            return Ok((WasmEvalResult { matched: false }, Vec::new()));
        }

        let output_len = result as usize;
        let mut output = vec![0u8; output_len];
        if output_len > 0 {
            self.memory
                .read(&self.store, out_ptr as usize, &mut output)
                .map_err(|e| format!("wasm read output: {e}"))?;
        }

        Ok((WasmEvalResult { matched: true }, output))
    }
}

fn patch_template(bytecode: &[u8]) -> Result<Vec<u8>, String> {
    let max_program = WASM_SLOT_SIZE - WASM_PROGRAM_OFFSET;
    if bytecode.len() > max_program {
        return Err(format!(
            "program is {} bytes; maximum is {max_program}",
            bytecode.len()
        ));
    }

    let slot_pos = WASM_TEMPLATE
        .windows(WASM_SENTINEL.len())
        .position(|w| w == WASM_SENTINEL)
        .ok_or("sentinel not found in WASM template")?;

    let mut wasm = WASM_TEMPLATE.to_vec();
    let program_start = slot_pos + WASM_PROGRAM_OFFSET;
    wasm[program_start..slot_pos + WASM_SLOT_SIZE].fill(0);
    wasm[program_start..program_start + bytecode.len()].copy_from_slice(bytecode);

    Ok(wasm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_eval_filter() {
        let opts = wql_compiler::CompileOptions {
            schema: None,
            root_message: None,
        };
        let bytecode = wql_compiler::compile("#1 > 10", &opts).unwrap();
        let mut prog = WasmProgram::new(&bytecode).unwrap();

        // field 1 = 42 → matches
        let (result, _) = prog.eval(b"\x08\x2a").unwrap();
        assert!(result.matched);

        // field 1 = 5 → does not match
        let (result, _) = prog.eval(b"\x08\x05").unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn wasm_eval_projection() {
        let opts = wql_compiler::CompileOptions {
            schema: None,
            root_message: None,
        };
        let bytecode = wql_compiler::compile("{ #1 }", &opts).unwrap();
        let mut prog = WasmProgram::new(&bytecode).unwrap();

        // input: field 1 = 42, field 2 = 99 → project keeps only field 1
        let input = b"\x08\x2a\x10\x63";
        let (result, output) = prog.eval(input).unwrap();
        assert!(result.matched);
        assert_eq!(output, b"\x08\x2a");
    }
}
