// Integration tests for the linker module
//
// These tests verify that the linker can properly combine user code
// with runtime libraries and produce valid executable output.

#[cfg(test)]
mod tests {
    use crate::linker::{Linker, create_user_object};
    use std::collections::HashMap;

    #[test]
    fn test_link_user_code_with_empty_runtime() {
        // Create simple user code
        let user_code = vec![
            0x48, 0x89, 0xe5, // mov rbp, rsp
            0xc3, // ret
        ];

        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);

        // Create linker and add user code
        let mut linker = Linker::new();
        let result = linker.add_user_code_object(&user_code, &symbols);
        assert!(result.is_ok());

        // Link should succeed even with just user code
        let link_result = linker.link();
        assert!(link_result.is_ok());

        let linked = link_result.unwrap();
        assert!(!linked.text_section.is_empty());
        assert!(linked.symbols.contains_symbol("main"));
    }

    #[test]
    fn test_create_user_object_file() {
        let code = vec![
            0x55, // push rbp
            0x48, 0x89, 0xe5, // mov rbp, rsp
            0x48, 0x31, 0xc0, // xor rax, rax
            0x5d, // pop rbp
            0xc3, // ret
        ];

        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);
        symbols.insert("helper".to_string(), 7);

        let object_data = create_user_object(&code, &symbols, &[], 0);
        assert!(object_data.is_ok());

        let data = object_data.unwrap();
        // Verify ELF magic number
        assert!(data.len() >= 4);
        assert_eq!(&data[0..4], b"\x7fELF");
    }

    #[test]
    fn test_builder_pattern() {
        let user_code = vec![0x90; 16]; // NOPs
        let mut symbols = HashMap::new();
        symbols.insert("test_func".to_string(), 0);

        // Test builder pattern
        let result = Linker::new()
            .with_user_code(&user_code, &symbols)
            .and_then(|mut linker| linker.link());

        assert!(result.is_ok());
    }

    #[test]
    fn test_symbol_resolution() {
        // Create user code that would reference external symbols
        let user_code = vec![
            0xe8, 0x00, 0x00, 0x00, 0x00, // call relative (needs relocation)
            0xc3, // ret
        ];

        let mut symbols = HashMap::new();
        symbols.insert("caller".to_string(), 0);

        let mut linker = Linker::new();
        assert!(linker.add_user_code_object(&user_code, &symbols).is_ok());

        let link_result = linker.link();
        assert!(link_result.is_ok());

        let linked = link_result.unwrap();
        // Verify the symbol table has our function
        assert!(linked.symbols.contains_symbol("caller"));
    }

    #[test]
    fn test_multiple_symbols() {
        let code = vec![0x90; 64]; // Larger code section

        let mut symbols = HashMap::new();
        symbols.insert("func1".to_string(), 0);
        symbols.insert("func2".to_string(), 16);
        symbols.insert("func3".to_string(), 32);
        symbols.insert("_start".to_string(), 48);

        let object_result = create_user_object(&code, &symbols, &[], 0);
        assert!(object_result.is_ok());

        // Parse the created object to verify symbols
        let object_data = object_result.unwrap();
        let parsed = crate::linker::ObjectFile::parse("test.o".to_string(), &object_data);
        assert!(parsed.is_ok());

        let obj = parsed.unwrap();
        // Should have our symbols
        let symbol_names: Vec<String> = obj.symbols.iter().map(|s| s.name.clone()).collect();
        assert!(symbol_names.contains(&"func1".to_string()));
        assert!(symbol_names.contains(&"_start".to_string()));
    }

    #[test]
    fn test_with_data_and_bss_sections() {
        let code = vec![0x90; 32];
        let data = vec![0x42, 0x43, 0x44, 0x45]; // Some data
        let bss_size = 128;

        let mut symbols = HashMap::new();
        symbols.insert("main".to_string(), 0);

        let object_result = create_user_object(&code, &symbols, &data, bss_size);
        assert!(object_result.is_ok());

        // Verify it creates a valid ELF
        let obj_data = object_result.unwrap();
        assert!(obj_data.len() > 100); // Should be a reasonable size
        assert_eq!(&obj_data[0..4], b"\x7fELF");
    }
}
