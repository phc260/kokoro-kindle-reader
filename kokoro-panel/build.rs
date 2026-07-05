// Compile the Slint UI (ui/panel.slint) into Rust, styled with the Fluent theme for
// a modern Windows look. `slint::include_modules!()` in main.rs pulls in the result.
fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
    slint_build::compile_with_config("ui/panel.slint", config).expect("compile panel.slint");
}
