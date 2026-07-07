fn main() {
    windows_reactor_setup::as_framework_dependent();

    // Embed the application icon (Windows only). The reactor setup embeds the
    // app manifest via linker args, so this .rc carries only the icon.
    #[cfg(windows)]
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("failed to embed app icon");
}
