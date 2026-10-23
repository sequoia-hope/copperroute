//! The cost field on the GPU: `relax.wgsl` dispatched until nothing improves, in bounded
//! batches so no single submission holds the GPU long enough to stall the desktop.

use wgpu::util::DeviceExt;

use crate::grid::RouteGrid;
use crate::route::FieldSolver;
use crate::search::UNREACHED;

/// Relaxation sweeps per submission; the changed flag is read back between submissions.
const DISPATCHES_PER_BATCH: u32 = 64;
const WORKGROUP: u32 = 256;
const MAX_GROUPS_X: u32 = 65_535;

pub struct GpuSolver {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    pub adapter_name: String,
    pub dispatches: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    nx: u32,
    ny: u32,
    layers: u32,
    via_cost: u32,
    via_allowed: u32,
    _pad: [u32; 3],
}

impl Params {
    fn bytes(&self) -> Vec<u8> {
        [
            self.nx,
            self.ny,
            self.layers,
            self.via_cost,
            self.via_allowed,
            0,
            0,
            0,
        ]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
    }
}

impl GpuSolver {
    /// The first discrete or integrated adapter on Vulkan, Metal or DX12 (asking for every
    /// backend can hide a discrete GPU's Vulkan adapter behind its GL twin); `None` without one.
    pub fn new() -> Option<GpuSolver> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        descriptor.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .ok()?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("copper-gpu"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .ok()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("relax"),
            source: wgpu::ShaderSource::Wgsl(include_str!("relax.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("relax"),
            layout: None,
            module: &module,
            entry_point: Some("relax"),
            compilation_options: Default::default(),
            cache: None,
        });
        Some(GpuSolver {
            device,
            queue,
            pipeline,
            adapter_name,
            dispatches: 0,
        })
    }

    fn read_back(&self, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let bytes = slice
            .get_mapped_range()
            .map(|view| view.to_vec())
            .unwrap_or_default();
        staging.unmap();
        bytes
    }
}

impl FieldSolver for GpuSolver {
    fn solve(&mut self, grid: &RouteGrid, sources: &[usize], via_cost: u32) -> Vec<u32> {
        let nodes = grid.blocked.len();
        let as_u32 = |flags: &[bool]| -> Vec<u8> {
            flags
                .iter()
                .flat_map(|b| u32::from(*b).to_le_bytes())
                .collect()
        };
        let mut initial = vec![UNREACHED; nodes];
        for &s in sources {
            if !grid.blocked[s] {
                initial[s] = 0;
            }
        }
        let initial_bytes: Vec<u8> = initial.iter().flat_map(|v| v.to_le_bytes()).collect();
        let params = Params {
            nx: grid.nx as u32,
            ny: grid.ny as u32,
            layers: grid.layers.len() as u32,
            via_cost,
            via_allowed: u32::from(grid.via.is_some()),
            _pad: [0; 3],
        };
        let storage = |label, contents: &[u8], extra: wgpu::BufferUsages| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage: wgpu::BufferUsages::STORAGE | extra,
                })
        };
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("params"),
                contents: &params.bytes(),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let blocked = storage(
            "blocked",
            &as_u32(&grid.blocked),
            wgpu::BufferUsages::empty(),
        );
        let via_blocked = storage(
            "via_blocked",
            &as_u32(&grid.via_blocked),
            wgpu::BufferUsages::empty(),
        );
        let cost = storage("cost", &initial_bytes, wgpu::BufferUsages::COPY_SRC);
        let penalties: Vec<u8> = if grid.penalty.is_empty() {
            vec![0u8; nodes * 4]
        } else {
            grid.penalty.iter().flat_map(|v| v.to_le_bytes()).collect()
        };
        let penalty = storage("penalty", &penalties, wgpu::BufferUsages::empty());
        let changed = storage(
            "changed",
            &[0u8; 4],
            wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        );
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("relax"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: blocked.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: via_blocked.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: cost.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: changed.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: penalty.as_entire_binding(),
                },
            ],
        });
        let groups = (nodes as u32).div_ceil(WORKGROUP);
        let (gx, gy) = if groups <= MAX_GROUPS_X {
            (groups, 1)
        } else {
            (MAX_GROUPS_X, groups.div_ceil(MAX_GROUPS_X))
        };
        loop {
            self.queue.write_buffer(&changed, 0, &[0u8; 4]);
            let mut encoder = self.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                for _ in 0..DISPATCHES_PER_BATCH {
                    pass.dispatch_workgroups(gx, gy, 1);
                }
            }
            self.queue.submit([encoder.finish()]);
            self.dispatches += u64::from(DISPATCHES_PER_BATCH);
            let flag = self.read_back(&changed, 4);
            if flag.iter().all(|b| *b == 0) {
                break;
            }
        }
        self.read_back(&cost, (nodes * 4) as u64)
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}
