use bytemuck::{Pod, Zeroable};
use modul_core::wgpu;
use modul_core::wgpu::{BufferDescriptor, BufferUsages, Device};
use modul_render::BindGroupLayoutDef;
use crate::chunk::meshing::FaceData;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct PageMetadata {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub chunk_z: i32,
    pub direction_and_lod: u32,
}

pub const PAGE_SIZE: usize = 96;
pub const PAGES_PER_SLAB: usize = 174763; // 128 MB face data per slab

pub struct MetadataBGLayout;

impl BindGroupLayoutDef for MetadataBGLayout {
    const LAYOUT: &'static wgpu::BindGroupLayoutDescriptor<'static> =
        &wgpu::BindGroupLayoutDescriptor {
            label: Some("Metadata BG Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZero::new(
                        std::mem::size_of::<PageMetadata>() as u64,
                    ),
                },
                count: None,
            }],
        };

    const LIBRARY: &'static str = "\
struct PageMetadata {
    chunk_x: i32,
    chunk_y: i32,
    chunk_z: i32,
    direction_and_lod: u32,
};

@group(#BIND_GROUP) @binding(0)
var<storage, read> metadata: array<PageMetadata>;
";
}

pub struct Slab {
    pub face_buffer: wgpu::Buffer,
    pub metadata_buffer: wgpu::Buffer,
    pub metadata_bind_group: wgpu::BindGroup,
    free_list: Vec<u32>,
}

impl Slab {
    fn new(device: &Device, metadata_bg_layout: &wgpu::BindGroupLayout, index: usize) -> Self {
        let face_buffer = device.create_buffer(&BufferDescriptor {
            label: Some(&format!("Face buffer slab {index}")),
            size: PAGES_PER_SLAB as u64 * PAGE_SIZE as u64 * std::mem::size_of::<FaceData>() as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let metadata_buffer = device.create_buffer(&BufferDescriptor {
            label: Some(&format!("Metadata buffer slab {index}")),
            size: PAGES_PER_SLAB as u64 * std::mem::size_of::<PageMetadata>() as u64,
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let metadata_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("Metadata BG slab {index}")),
            layout: metadata_bg_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: metadata_buffer.as_entire_binding(),
            }],
        });

        Self {
            face_buffer,
            metadata_buffer,
            metadata_bind_group,
            free_list: (0..PAGES_PER_SLAB as u32).rev().collect(),
        }
    }

    fn allocate(&mut self) -> Option<u32> {
        self.free_list.pop()
    }

    fn deallocate(&mut self, page: u32) {
        self.free_list.push(page);
    }

    fn used_count(&self) -> usize {
        PAGES_PER_SLAB - self.free_list.len()
    }

    fn is_full(&self) -> bool {
        self.free_list.is_empty()
    }
}

