# no_std Rust example for QEMU ARM "virt" machine with Cortex-A53 processor

Run `rustup target add aarch64-unknown-none-softfloat` and `sudo apt install qemu-system-arm gdb-multiarch`

Build and run (the QEMU commands live in `.cargo/config.toml`):

```
cargo qemu        # release build; in another shell: telnet localhost 5678
cargo qemu-debug  # debug build; QEMU halts and waits for GDB on localhost:1234
```

Notes:

- The `-softfloat` target keeps the compiler from using the FP/SIMD registers, like `-mgeneral-regs-only` in C. The FPU is disabled at EL1, and `irq_entry` doesn't save those registers
- Serial output is lost until a telnet client connects; change `wait=off` to `wait=on` in `.cargo/config.toml` to make QEMU wait for the connection before starting
- The ELF is passed straight to `-kernel`, so QEMU loads it at its linked address (0x40080000, after the device tree at the RAM base) and jumps to `_start`
