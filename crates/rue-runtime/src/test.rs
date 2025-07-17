#[cfg(test)]
mod tests {
    #[test]
    fn test_runtime_generation() {
        let (instructions, labels) = crate::generate_runtime().unwrap();

        // Check that we generated instructions
        assert!(!instructions.is_empty());

        // Check that key runtime functions are labeled
        assert!(labels.contains_key("__rue_exit"));
        assert!(labels.contains_key("__rue_println_i64"));
        assert!(labels.contains_key("__rue_println_i32"));
        assert!(labels.contains_key("__rue_println_bool"));
        assert!(labels.contains_key("__rue_println_unit"));
        assert!(labels.contains_key("__rue_input"));
        assert!(labels.contains_key("__rue_itoa"));
        assert!(labels.contains_key("__rue_atoi"));

        // Check signal handler labels
        assert!(labels.contains_key("__rue_sigfpe_handler"));
        assert!(labels.contains_key("__rue_sigsegv_handler"));

        // Verify we have a reasonable number of instructions (runtime is substantial)
        assert!(instructions.len() > 100);
    }

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(crate::syscalls::numbers::SYS_READ, 0);
        assert_eq!(crate::syscalls::numbers::SYS_WRITE, 1);
        assert_eq!(crate::syscalls::numbers::SYS_EXIT, 60);
    }
}
