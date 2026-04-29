# crt0 for the in-tree coreutils.
#
# The Linux/SysV x86-64 process-startup protocol places, at the top of
# the user stack on entry:
#     [%rsp +  0]  argc       (8 bytes)
#     [%rsp +  8]  argv[0..argc-1]   (each 8 bytes)
#     [%rsp + ...] argv[argc] = NULL
#     [%rsp + ...] envp...
# Our `main` follows the ISO C signature: int main(int argc, char **argv).
# We unpack rsp into rdi/rsi and call into C.
#
# After main returns, we exit with its return code.

.section .text
.globl _start
.type _start, @function

_start:
    xor %rbp, %rbp               # mark outermost frame
    mov (%rsp), %rdi             # argc -> rdi
    lea 8(%rsp), %rsi            # argv -> rsi
    and $-16, %rsp               # align for call (SysV requires 16-byte)
    call main
    mov %rax, %rdi               # main's return -> exit code
    mov $60, %rax                # SYS_exit
    syscall
1:  hlt                          # main returned + exit failed somehow
    jmp 1b
