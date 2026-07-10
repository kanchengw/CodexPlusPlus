fn main() {
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_toolkit_path(r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64");
        resource.set_icon("../codex-plus-manager/src-tauri/icons/icon.ico");
        resource.set_manifest(include_str!(
            "../codex-plus-manager/src-tauri/windows-app-manifest.xml"
        ));
        resource.compile().expect("compile launcher icon resource");
    }
}
