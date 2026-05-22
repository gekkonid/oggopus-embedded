fn main() {
    // linkall.x is provided by esp-hal and defines the interrupt vector table,
    // memory layout, and startup code. Without it, interrupt handler symbols
    // (GPIO, UART0, RTC_CORE, etc.) are undefined.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}
