use derivative::Derivative;
use failure::format_err;
use rendy::hal;
use rendy::{
    command::QueueId,
    factory::{BufferState, Factory, ImageState},
    memory::MemoryUsageValue,
    mesh::PosNormTangTex,
    resource::{Buffer, BufferInfo, Escape},
    texture::{
        image::{ImageTextureConfig, Repr},
        Texture, TextureBuilder,
    },
};

use std::{collections::HashMap, fs::File, io::Read, path::Path};

use crate::Backend;

#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct MaterialFactors {
    pub albedo: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
}

pub struct MaterialData<B: hal::Backend> {
    pub factors: MaterialFactors,
    pub albedo: Texture<B>,
    pub normal: Texture<B>,
    pub metallic_roughness: Texture<B>,
    pub ao: Texture<B>,
    pub emissive: Texture<B>,
    pub emissive_factor_buffer: Escape<Buffer<B>>,
}

#[derive(Default)]
pub struct MaterialStorage<B: hal::Backend>(pub Vec<MaterialData<B>>);
pub type MaterialHandle = usize;

pub struct Primitive<B: hal::Backend> {
    pub mesh_data: rendy::mesh::Mesh<B>,
    pub mesh_handle: MeshHandle,
    pub mat: MaterialHandle,
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct PrimitiveStorage<B: hal::Backend>(pub Vec<Primitive<B>>);
pub type PrimitiveHandle = usize;

#[derive(Default)]
pub struct Mesh {
    pub primitives: Vec<PrimitiveHandle>,
    pub max_instances: u16,
}

#[derive(Default)]
pub struct MeshStorage(pub Vec<Mesh>);
pub type MeshHandle = usize;

#[derive(Default)]
pub struct MeshHandleMap(pub HashMap<String, MeshHandle>);

pub struct GltfBuffers(pub Vec<Vec<u8>>);

impl GltfBuffers {
    pub fn load_from_gltf<P: AsRef<Path>>(
        base_path: P,
        gltf: &gltf::Gltf,
    ) -> Result<Self, failure::Error> {
        use gltf::buffer::Source;
        let mut buffers = vec![];
        for (_index, buffer) in gltf.buffers().enumerate() {
            let data = match buffer.source() {
                Source::Uri(uri) => {
                    if uri.starts_with("data:") {
                        unimplemented!();
                    } else {
                        let mut file = File::open(base_path.as_ref().join(uri))?;
                        let mut data: Vec<u8> = Vec::with_capacity(file.metadata()?.len() as usize);
                        file.read_to_end(&mut data)?;
                        data
                    }
                }
                Source::Bin => unimplemented!(),
            };

            assert!(data.len() >= buffer.length());
            buffers.push(data);
        }
        Ok(GltfBuffers(buffers))
    }

    /// Obtain the contents of a loaded buffer.
    pub fn buffer(&self, buffer: &gltf::Buffer<'_>) -> Option<&[u8]> {
        self.0.get(buffer.index()).map(Vec::as_slice)
    }

    /// Obtain the contents of a loaded buffer view.
    #[allow(unused)]
    pub fn view(&self, view: &gltf::buffer::View<'_>) -> Option<&[u8]> {
        self.buffer(&view.buffer()).map(|data| {
            let begin = view.offset();
            let end = begin + view.length();
            &data[begin..end]
        })
    }
}

pub fn load_gltf_mesh<P: AsRef<Path>, B: hal::Backend>(
    mesh: &gltf::Mesh<'_>,
    max_instances: u16,
    generate_mips: bool,
    base_dir: P,
    buffers: &GltfBuffers,
    base_mesh_index: usize,
    base_material_index: usize,
    material_storage: &mut Vec<Option<MaterialData<B>>>,
    primitive_storage: &mut Vec<Option<Primitive<B>>>,
    mesh_storage: &mut Vec<Option<Mesh>>,
    factory: &mut Factory<B>,
    queue: QueueId,
) -> Result<MeshHandle, failure::Error> {
    let mesh_idx = base_mesh_index + mesh.index();
    if let Some(_) = mesh_storage[mesh_idx] {
        Ok(mesh_idx as MeshHandle)
    } else {
        let mut primitives = Vec::new();

        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buf_id| buffers.buffer(&buf_id));

            let indices = reader
                .read_indices()
                .ok_or(format_err!("Mesh primitive does not contain indices"))?
                .into_u32()
                .collect::<Vec<u32>>();

            let positions = reader
                .read_positions()
                .ok_or(format_err!("Primitive does not have positions"))?;
            let normals = reader
                .read_normals()
                .ok_or(format_err!("Primitive does not have normals"))?;
            let tangents = reader
                .read_tangents()
                .ok_or(format_err!("Primitive does not have tangents"))?;
            let uvs = reader
                .read_tex_coords(0)
                .ok_or(format_err!("Primitive does not have tex coords"))?
                .into_f32();

            let vertices = positions
                .zip(normals.zip(tangents.zip(uvs)))
                .map(|(pos, (norm, (tang, uv)))| PosNormTangTex {
                    position: pos.into(),
                    normal: norm.into(),
                    tangent: tang.into(),
                    tex_coord: uv.into(),
                })
                .collect::<Vec<_>>();

            let prim_mesh = rendy::mesh::Mesh::<Backend>::builder()
                .with_indices(&indices[..])
                .with_vertices(&vertices[..])
                .build(queue, factory)?;

            let material = primitive.material();
            let mat_idx = base_material_index
                + material
                    .index()
                    .ok_or(format_err!("Default material unimplemented"))?;

            if let None = material_storage[mat_idx] {
                let pbr_met_rough = material.pbr_metallic_roughness();

                let factors = MaterialFactors {
                    albedo: pbr_met_rough.base_color_factor(),
                    metallic: pbr_met_rough.metallic_factor(),
                    roughness: pbr_met_rough.roughness_factor(),
                };

                let state = ImageState {
                    queue,
                    stage: hal::pso::PipelineStage::FRAGMENT_SHADER,
                    access: hal::image::Access::SHADER_READ,
                    layout: hal::image::Layout::ShaderReadOnlyOptimal,
                };

                let albedo = load_gltf_texture(
                    &base_dir,
                    pbr_met_rough
                        .base_color_texture()
                        .ok_or(format_err!("Material has no base color texture"))?
                        .texture(),
                    true,
                    generate_mips,
                )?
                .build(state, factory)?;

                let metallic_roughness = load_gltf_texture(
                    &base_dir,
                    pbr_met_rough
                        .metallic_roughness_texture()
                        .ok_or(format_err!("Material has no metallic_roughness texture"))?
                        .texture(),
                    false,
                    generate_mips,
                )?
                .build(state, factory)?;

                let normal = load_gltf_texture(
                    &base_dir,
                    material
                        .normal_texture()
                        .ok_or(format_err!("Material has no normal texture"))?
                        .texture(),
                    false,
                    generate_mips,
                )?
                .build(state, factory)?;

                let ao = load_gltf_texture(
                    &base_dir,
                    material
                        .occlusion_texture()
                        .ok_or(format_err!("Material has no occlusion texture"))?
                        .texture(),
                    false,
                    generate_mips,
                )?
                .build(state, factory)?;

                let emissive = if let Some(emissive_info) = material.emissive_texture() {
                    load_gltf_texture(&base_dir, emissive_info.texture(), true, generate_mips)?
                        .build(state, factory)?
                } else {
                    rendy::texture::TextureBuilder::new()
                        .with_data(vec![rendy::texture::pixel::Rgb8Srgb { repr: [0, 0, 0] }])
                        .with_data_width(1)
                        .with_data_height(1)
                        .with_kind(hal::image::Kind::D2(1, 1, 1, 1))
                        .with_view_kind(hal::image::ViewKind::D2)
                        .build(state, factory)?
                };

                let emissive_factor = material.emissive_factor();
                let emissive_factor_buffer = factory.create_buffer(
                    BufferInfo {
                        size: std::mem::size_of::<[f32; 3]>() as u64,
                        usage: hal::buffer::Usage::UNIFORM | hal::buffer::Usage::TRANSFER_DST,
                    },
                    MemoryUsageValue::Data,
                )?;

                unsafe {
                    factory.upload_buffer(
                        &emissive_factor_buffer,
                        0,
                        &emissive_factor,
                        None,
                        BufferState {
                            queue,
                            stage: hal::pso::PipelineStage::FRAGMENT_SHADER,
                            access: hal::buffer::Access::SHADER_READ,
                        },
                    )?;
                }

                material_storage[mat_idx] = Some(MaterialData {
                    factors,
                    albedo,
                    metallic_roughness,
                    normal,
                    ao,
                    emissive,
                    emissive_factor_buffer,
                });
            }

            primitive_storage.push(Some(Primitive {
                mesh_data: prim_mesh,
                mesh_handle: mesh_idx,
                mat: mat_idx as MaterialHandle,
            }));

            primitives.push(primitive_storage.len() - 1);
        }

        mesh_storage[mesh_idx] = Some(Mesh {
            primitives,
            max_instances,
        });

        Ok(mesh_idx as MeshHandle)
    }
}

fn load_gltf_texture<P>(
    base_dir: P,
    texture: gltf::Texture<'_>,
    srgb: bool,
    generate_mips: bool,
) -> Result<TextureBuilder<'static>, failure::Error>
where
    P: AsRef<Path>,
{
    match texture.source().source() {
        gltf::image::Source::View { .. } => unimplemented!(),
        gltf::image::Source::Uri { uri, .. } => {
            let path = base_dir.as_ref().join(uri);
            log::info!("Loading image: {:#?}", path);
            rendy::texture::image::load_from_image(
                std::io::BufReader::new(File::open(path)?),
                ImageTextureConfig {
                    repr: match srgb {
                        true => Repr::Srgb,
                        false => Repr::Unorm,
                    },
                    generate_mips,
                    ..Default::default()
                },
            )
            .map_err(|e| e.into())
        }
    }
}
