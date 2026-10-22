use wgpu::{Backends, Features, Instance, InstanceDescriptor};

fn main() {
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(instance.enumerate_adapters(Backends::PRIMARY));
    if adapters.is_empty() {
        println!("no adapters");
    }
    for adapter in adapters {
        let info = adapter.get_info();
        let features = adapter.features();
        let limits = adapter.limits();
        println!(
            "{} [{:?}, {:?}] driver {} {}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        );
        for (name, feature) in [
            ("SHADER_INT64", Features::SHADER_INT64),
            (
                "SHADER_INT64_ATOMIC_MIN_MAX",
                Features::SHADER_INT64_ATOMIC_MIN_MAX,
            ),
            (
                "SHADER_INT64_ATOMIC_ALL_OPS",
                Features::SHADER_INT64_ATOMIC_ALL_OPS,
            ),
            ("SUBGROUP", Features::SUBGROUP),
            ("TIMESTAMP_QUERY", Features::TIMESTAMP_QUERY),
        ] {
            println!("  {name:30} {}", features.contains(feature));
        }
        println!(
            "  max_storage_buffer_binding_size {} MiB",
            limits.max_storage_buffer_binding_size >> 20
        );
        println!(
            "  max_buffer_size                 {} MiB",
            limits.max_buffer_size >> 20
        );
        println!(
            "  max_compute_workgroup_size_x    {}",
            limits.max_compute_workgroup_size_x
        );
        println!(
            "  max_compute_invocations_per_wg  {}",
            limits.max_compute_invocations_per_workgroup
        );
    }
}
