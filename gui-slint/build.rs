fn main() {
    // The app is dark-only: pin the std-widgets to the dark Fluent variant so
    // LineEdit/ComboBox/CheckBox don't follow a light system theme.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app-window.slint", config)
        .expect("failed to compile Slint UI");
}
