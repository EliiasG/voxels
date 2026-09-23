use bytemuck::{Pod, Zeroable};
use modul_core::wgpu;
use modul_render::BindGroupLayoutDef;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct FaceData {
    x: u8,
    y: u8,
    z: u8,
    w: u8,
    h: u8,
    /// 4 corners * 2 bits
    ao: u8,
    material: u16,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct PageMetadata {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub chunk_z: i32,
    pub direction_and_lod: u32,
}

pub struct Slab {
    pub face_buffer: wgpu::Buffer,
    pub metadata_buffer: wgpu::Buffer,
    pub metadata_bind_group: wgpu::BindGroup,
    free_list: Vec<u32>,
}

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