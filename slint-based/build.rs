fn main() {
    #[cfg(feature = "starter")]
    slint_build::compile_with_config(
        "ui/app-window.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
    #[cfg(feature = "timer")]
    slint_build::compile_with_config(
        "ui/timer-app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
    #[cfg(feature = "light-control")]
    slint_build::compile_with_config(
        "ui/lights-app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
    #[cfg(feature = "microwave-ui")]
    slint_build::compile_with_config(
        "ui/microwave-ui.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}
