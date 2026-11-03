fn main() {
    let mut extensions = infinity_manager_ui::Extensions::default();
    // Built from this repository, so the developer tools are everyone's.
    infinity_manager_ui::sim_tools::register(&mut extensions, infinity_manager_ui::Gate::Everyone);
    infinity_manager_ui::mount(extensions);
}
