.section .text
.global _start

    jmp .L0
__rue_newline:
    movabsq $10, %rax
__rue_true_str:
    movabsq $1702195828, %rax
__rue_false_str:
    movabsq $435728179558, %rax
__rue_unit_str:
    movabsq $10536, %rax
__rue_input_buffer:
.L0:
__rue_exit:
    movabsq $60, %rax
    syscall
__rue_itoa:
    pushq %rbx
    pushq %rcx
    pushq %rdx
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    movq %rsi, %r8
    movq %rdi, %rbx
    movabsq $0, %r9
    cmpq $0, %rbx
    jl .L8
    jmp .L9
.L8:
    movabsq $45, %rax
    movb %al, (%r8)
    addq $1, %r8
    addq $1, %r9
    movabsq $-9223372036854775808, %rax
    cmpq %rax, %rbx
    jne .L15
    movabsq $57, %rax
    movb %al, (%r8)
    movabsq $50, %rax
    movb %al, 1(%r8)
    movabsq $50, %rax
    movb %al, 2(%r8)
    movabsq $51, %rax
    movb %al, 3(%r8)
    movabsq $51, %rax
    movb %al, 4(%r8)
    movabsq $55, %rax
    movb %al, 5(%r8)
    movabsq $50, %rax
    movb %al, 6(%r8)
    movabsq $48, %rax
    movb %al, 7(%r8)
    movabsq $51, %rax
    movb %al, 8(%r8)
    movabsq $54, %rax
    movb %al, 9(%r8)
    movabsq $56, %rax
    movb %al, 10(%r8)
    movabsq $53, %rax
    movb %al, 11(%r8)
    movabsq $52, %rax
    movb %al, 12(%r8)
    movabsq $55, %rax
    movb %al, 13(%r8)
    movabsq $55, %rax
    movb %al, 14(%r8)
    movabsq $53, %rax
    movb %al, 15(%r8)
    movabsq $56, %rax
    movb %al, 16(%r8)
    movabsq $48, %rax
    movb %al, 17(%r8)
    movabsq $56, %rax
    movb %al, 18(%r8)
    addq $19, %r9
    jmp .L14
.L15:
    movabsq $0, %rax
    subq %rbx, %rax
    movq %rax, %rbx
.L9:
    movq %r8, %r10
.L10:
    movq %rbx, %rax
    movabsq $0, %rdx
    movabsq $10, %rcx
    idivq %rcx
    addq $48, %rdx
    movb %dl, (%r10)
    addq $1, %r10
    addq $1, %r9
    movq %rax, %rbx
    cmpq $0, %rbx
    jne .L10
.L11:
    movq %r8, %r11
    subq $1, %r10
.L12:
    cmpq %r10, %r11
    jge .L13
    movb (%r11), %al
    movb (%r10), %bl
    movb %bl, (%r11)
    movb %al, (%r10)
    addq $1, %r11
    subq $1, %r10
    jmp .L12
.L13:
.L14:
    movq %r9, %rax
    popq %r11
    popq %r10
    popq %r9
    popq %r8
    popq %rdx
    popq %rcx
    popq %rbx
    ret
__rue_println_i64:
    subq $32, %rsp
    movq %rsp, %rsi
    call __rue_itoa
    movabsq $1, %rdi
    movq %rsp, %rsi
    movq %rax, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_128
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_128:
    movabsq $10, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $1, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_141
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_141:
    addq $40, %rsp
    ret
__rue_println_i32:
    movq %rdi, %rax
    movabsq $32, %rcx
    shlq %cl, %rax
    sarq %cl, %rax
    movq %rax, %rdi
    call __rue_println_i64
    ret
__rue_println_bool:
    cmpq $0, %rdi
    je .L21
    movabsq $1702195828, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $4, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_167
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_167:
    addq $8, %rsp
    jmp .L22
.L21:
    movabsq $435728179558, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $5, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_183
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_183:
    addq $8, %rsp
.L22:
    movabsq $10, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $1, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_198
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_198:
    addq $8, %rsp
    ret
__rue_println_unit:
    movabsq $10536, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $2, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_214
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_214:
    addq $8, %rsp
    movabsq $10, %rax
    pushq %rax
    movabsq $1, %rdi
    movq %rsp, %rsi
    movabsq $1, %rdx
    movabsq $1, %rax
    syscall
    cmpq $0, %rax
    jge syscall_success_228
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
syscall_success_228:
    addq $8, %rsp
    ret
__rue_atoi:
    pushq %rbx
    pushq %rcx
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    movq %rsi, %r8
    movq %rdx, %r9
.L30:
    cmpq $0, %r9
    je .L31
    movb (%r8), %al
    cmpq $32, %rax
    je .L32
    cmpq $9, %rax
    je .L32
    cmpq $10, %rax
    je .L32
    cmpq $13, %rax
    je .L32
    jmp .L31
.L32:
    addq $1, %r8
    subq $1, %r9
    jmp .L30
.L31:
    cmpq $0, %r9
    je .L37
    movabsq $1, %r10
.L33:
    movb (%r8), %al
    cmpq $45, %rax
    je .L34
    cmpq $43, %rax
    jne .L35
    addq $1, %r8
    subq $1, %r9
    jmp .L35
.L34:
    movabsq $-1, %r10
    addq $1, %r8
    subq $1, %r9
.L35:
    movabsq $0, %rbx
.L36:
    cmpq $0, %r9
    je .L38
    movb (%r8), %al
    cmpq $48, %rax
    jl .L38
    cmpq $57, %rax
    jg .L38
    subq $48, %rax
    movabsq $922337203685477580, %rcx
    cmpq %rcx, %rbx
    jle .L40
    movabsq $0, %rax
    jmp .L39
.L40:
    imulq $10, %rbx
    addq %rax, %rbx
    addq $1, %r8
    subq $1, %r9
    jmp .L36
.L37:
    movabsq $0, %rax
    jmp .L39
.L38:
    movq %rbx, %rax
    imulq %r10, %rax
.L39:
    popq %r11
    popq %r10
    popq %r9
    popq %r8
    popq %rcx
    popq %rbx
    ret
__rue_input:
    subq $1024, %rsp
    movabsq $0, %rdi
    movq %rsp, %rsi
    movabsq $1024, %rdx
    movabsq $0, %rax
    syscall
    cmpq $0, %rax
    jge .L42
    movabsq $252, %rdi
    movabsq $60, %rax
    syscall
.L42:
    movq %rsp, %rsi
    movq %rax, %rdx
    call __rue_atoi
    addq $1024, %rsp
    ret
__rue_to_i32:
    movq %rdi, %rax
    ret
__rue_to_i64:
    movslq %edi, %rax
    ret
__rue_sigfpe_handler:
    movabsq $250, %rdi
    movabsq $60, %rax
    syscall
__rue_sigsegv_handler:
    movabsq $251, %rdi
    movabsq $60, %rax
    syscall
__rue_setup_signal_handlers:
    subq $304, %rsp
    movq %rsp, %rdi
    movabsq $0, %rax
    movabsq $152, %rcx
    cld
    rep stosb
    leaq __rue_sigfpe_handler(%rip), %rax
    movq %rax, (%rsp)
    movabsq $67108864, %rax
    movq %rax, 136(%rsp)
    movabsq $8, %rdi
    movq %rsp, %rsi
    movabsq $0, %rdx
    movabsq $8, %r10
    movabsq $13, %rax
    syscall
    leaq __rue_sigsegv_handler(%rip), %rax
    movq %rax, (%rsp)
    movabsq $11, %rdi
    movq %rsp, %rsi
    movabsq $0, %rdx
    movabsq $8, %r10
    movabsq $13, %rax
    syscall
    addq $304, %rsp
    ret
test_while_false:
    pushq %rbp
    movq %rsp, %rbp
    subq $0, %rsp
    movq %rdi, %rcx
.L49:
    movq %rcx, %rdx
    movl $100, %rsi
    cmpq %rsi, %rdx
    setg %dil
    movzbq %dil, %rdi
    cmpq $0, %rdi
    jne .L50
    jmp .L51
.L50:
    movl $42, %r8
    jmp .L49
.L51:
    movl $0, %r9
    movl $10, %r10
    movq %r10, %rax
    movq %rbp, %rsp
    popq %rbp
    ret
test_while_nested:
    pushq %rbp
    movq %rsp, %rbp
    subq $120, %rsp
    movq %rdi, %rcx
    movq %rcx, %rdx
    movl $5, %rsi
    cmpq %rsi, %rdx
    setg %dil
    movzbq %dil, %rdi
    cmpq $0, %rdi
    jne .L53
    jmp .L57
.L53:
.L54:
    movq %rcx, %r8
    movl $3, %r9
    cmpq %r9, %r8
    setle %r10b
    movzbq %r10b, %r10
    cmpq $0, %r10
    jne .L55
    jmp .L56
.L55:
    movq %rcx, %r11
    movq %rdx, -8(%rbp)
    movl $1, %rdx
    movq %rsi, -16(%rbp)
    movq %r11, %rsi
    addq %rdx, %rsi
    jmp .L54
.L56:
    movq %rdi, -24(%rbp)
    movl $0, %rdi
    movq %r8, -32(%rbp)
    movl $20, %r8
    movq %r9, -40(%rbp)
    movq %r8, %r9
    jmp .L61
.L57:
.L58:
    movq %r10, -48(%rbp)
    movq %rcx, %r10
    movq %r11, -56(%rbp)
    movl $10, %r11
    cmpq %r11, %r10
    movq %rsi, -64(%rbp)
    setg %sil
    movzbq %sil, %rsi
    cmpq $0, %rsi
    jne .L59
    jmp .L60
.L59:
    movq %rdx, -72(%rbp)
    movq %rcx, %rdx
    movq %rdi, -80(%rbp)
    movl $1, %rdi
    movq %r8, -88(%rbp)
    movq %rdx, %r8
    subq %rdi, %r8
    jmp .L58
.L60:
    movq %r9, -96(%rbp)
    movl $0, %r9
    movq %r10, -104(%rbp)
    movl $30, %r10
    movq %r11, -112(%rbp)
    movq -96(%rbp), %r11
    movq %r10, %r11
.L61:
    movq %rsi, -120(%rbp)
    movq %r11, %rsi
    movq %rsi, %rax
    movq %rbp, %rsp
    popq %rbp
    ret
main:
    pushq %rbp
    movq %rsp, %rbp
    subq $32, %rsp
    movl $5, %rcx
    movq %rcx, -8(%rbp)
    movq -8(%rbp), %rdx
    movq %rdx, %rdi
    call test_while_false
    movq %rax, %rsi
    movq %rsi, %rdi
    movl $7, %r8
    movq %rdx, -8(%rbp)
    movq %rsi, -16(%rbp)
    movq %rdi, -24(%rbp)
    movq %r8, -32(%rbp)
    movq -32(%rbp), %r9
    movq %r9, %rdi
    call test_while_nested
    movq %rax, %r10
    movq -24(%rbp), %r11
    movq %r11, %rcx
    addq %r10, %rcx
    movq %rcx, %rax
    movq %rbp, %rsp
    popq %rbp
    ret
_start:
    subq $8, %rsp
    call main
    addq $8, %rsp
    movq %rax, %rcx
    movl $60, %rdx
    movq %rdx, %rax
    movq %rcx, %rdi
    syscall
    movq %rax, %rsi
