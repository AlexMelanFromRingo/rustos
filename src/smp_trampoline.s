# AP long-mode trampoline (real → protected → long mode).
#
# Linked at physical address 0x8000 — the BSP copies this binary blob to
# that page before firing INIT-SIPI-SIPI.  Each AP starts here in 16-bit
# real mode at CS:IP = 0x800:0x0000 (linear 0x8000) per Intel SDM Vol. 3
# §8.4.4.
#
# Three patched parameter slots near the end of the blob carry per-CPU
# state in from the BSP:
#
#   param_cr3      — physical address of a PML4 that identity-maps the
#                    first 2 MiB (so this code keeps executing after
#                    paging turns on) and contains the kernel's high
#                    virtual mappings (so the AP can call ap_main).
#   param_stack    — kernel-stack top for this AP (RSP after the long-
#                    mode transition).
#   param_entry    — virtual address of the Rust ap_main fn.
#   param_cpu_id   — APIC ID; passed to ap_main as %edi (sysv ABI arg 1).
#
# Once in long mode the AP writes the magic 0xCAFEBABE_DEADBEEF at phys
# 0x9000 so the BSP can confirm "AP made it past CR0.PG", then tail-jumps
# into Rust.

.section .text

# ──── Real mode (16-bit) ──────────────────────────────────────────
.code16
.globl ap_trampoline_start
ap_trampoline_start:
    cli
    cld

    # Clear segment registers — the BIOS / SIPI may have left arbitrary
    # values that would break flat-real-mode addressing.
    xor %ax, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs
    mov %ax, %ss

    # Load our GDT.  Operand-size prefix 0x66 (encoded by `lgdtl`) makes
    # the base a full 32 bits — required so the linker-resolved `gdt`
    # absolute address (0x8000+offset) doesn't get truncated to 24 bits.
    lgdtl gdt_descriptor

    # Set CR0.PE — enter protected mode.
    mov %cr0, %eax
    or  $1, %eax
    mov %eax, %cr0

    # Far jump into 32-bit code.  Selector 0x08 = 32-bit code in our
    # GDT.  The 0x66 prefix (encoded by `ljmpl`) gives a 32-bit offset.
    ljmpl $0x08, $protected_mode

# ──── Protected mode (32-bit) ─────────────────────────────────────
.code32
protected_mode:
    # Load 32-bit data segments.  Selector 0x10 = 32-bit data.
    mov $0x10, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs
    mov %ax, %ss

    # Load CR3 from our patched parameter slot.  At this point paging
    # is still off — we're just preparing the register so that turning
    # paging on in a few lines uses the BSP's (cloned) page table.
    mov param_cr3, %eax
    mov %eax, %cr3

    # Enable PAE in CR4 (bit 5).  PAE is mandatory for long mode.
    mov %cr4, %eax
    or  $0x20, %eax
    mov %eax, %cr4

    # Enable LME (bit 8) and NXE (bit 11) in EFER MSR (0xC0000080).
    # NXE matches the BSP's setting so existing kernel page-table NX
    # bits keep their meaning.
    mov $0xC0000080, %ecx
    rdmsr
    or  $0x100, %eax
    or  $0x800, %eax
    wrmsr

    # Enable paging: set CR0.PG (bit 31).  PE is already set.
    mov %cr0, %eax
    or  $0x80000001, %eax
    mov %eax, %cr0

    # Far jump into 64-bit code.  Selector 0x18 = 64-bit code (L=1).
    ljmpl $0x18, $long_mode

# ──── Long mode (64-bit) ──────────────────────────────────────────
.code64
long_mode:
    # Load 64-bit data segments.  Selector 0x20 = 64-bit data.
    mov $0x20, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs
    mov %ax, %ss

    # Load AP-specific kernel stack top.
    mov param_stack(%rip), %rsp

    # Handshake: write 0xCAFEBABE_DEADBEEF at phys 0x9000 so BSP can
    # confirm we made it.  Identity mapping covers the first 2 MiB so
    # virt 0x9000 == phys 0x9000 here.
    movabs $0xCAFEBABEDEADBEEF, %rax
    movabs $0x9000, %rcx
    mov %rax, (%rcx)

    # Pass apic_id as first arg per the System V x86-64 ABI (%rdi).
    mov param_cpu_id(%rip), %edi

    # Tail-call ap_main.  ap_main is `extern "C" fn(u32) -> !` so we
    # don't need to set up a return path.
    mov param_entry(%rip), %rax
    jmp *%rax

    # Should never get here, but if we do, halt forever.
1:  hlt
    jmp 1b

# ──── GDT (5 entries, 40 bytes) ───────────────────────────────────
# Access byte: P=1 DPL=0 S=1 + type
# Flags nibble: G=1 D L AVL  (D and L both apply per descriptor type)
.balign 16
gdt:
    .quad 0x0000000000000000      # 0x00 null
    .quad 0x00CF9A000000FFFF      # 0x08 32-bit code: G=1 D=1 L=0 type=10
    .quad 0x00CF92000000FFFF      # 0x10 32-bit data: G=1 D=1 type=02
    .quad 0x00AF9A000000FFFF      # 0x18 64-bit code: G=1 D=0 L=1 type=10
    .quad 0x00AF92000000FFFF      # 0x20 64-bit data: G=1 D=0 L=1 type=02
gdt_end:

# ──── GDTR (6 bytes for lgdtl) ────────────────────────────────────
gdt_descriptor:
    .word gdt_end - gdt - 1
    .long gdt

# ──── Patched parameters (BSP fills these before SIPI) ────────────
.balign 16
.globl param_cr3
param_cr3:    .quad 0
.globl param_stack
param_stack:  .quad 0
.globl param_entry
param_entry:  .quad 0
.globl param_cpu_id
param_cpu_id: .long 0
              .long 0           # padding to keep size predictable

.globl ap_trampoline_end
ap_trampoline_end:
