//! Embeds version info and the icon so the service is recognisable in Task
//! Manager, services.msc and file properties.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=res/svc.rc");
        println!("cargo:rerun-if-changed=res/infinity.ico");
        if let Err(e) =
            embed_resource::compile("res/svc.rc", embed_resource::NONE).manifest_optional()
        {
            println!("cargo:warning=service resources not embedded: {e}");
        }
    }
}
