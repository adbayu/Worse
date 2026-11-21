fn main() {
    let automation = std::env::var_os("CARGO_FEATURE_AUTOMATION").is_some();
    println!("cargo:rerun-if-changed=lang");
    println!("cargo:rerun-if-changed=ui");
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        .with_debug_info(automation);
    if let Err(error) = slint_build::compile_with_config("ui/app.slint", config) {
        println!("cargo:warning=slint compile failed: {error}");
        std::process::exit(1);
    }
}
