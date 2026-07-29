#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    apteronotus_app::run_native()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
