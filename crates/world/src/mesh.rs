//! Greedy meshing with per-vertex ambient occlusion (plan T-007).
//!
//! # Why greedy
//!
//! A naive per-block mesher emits 6 quads per solid block. A 16x96x16 terrain
//! column holds roughly 10,000 solid blocks, so that is 60,000 quads — 240,000
//! vertices and 720,000 indices, which a phone will not survive. Greedy meshing
//! merges coplanar, same-material faces into rectangles: a flat 16x16 grass
//! field becomes **one** quad instead of 256.
//!
//! # Why ambient occlusion and not shadows
//!
//! Plan §4.9 forbids destruction effects, which risks the world reading as flat
//! cardboard. Per-vertex AO is the cheapest way to restore depth: four integer
//! comparisons per corner at mesh time and **zero** cost at render time,
//! because it is baked into the vertex attribute and multiplied into the final
//! colour by the shader.
//!
//! # Vertex format
//!
//! Exactly 12 bytes, asserted by [`tests::vertex_stride_is_twelve_bytes`]:
//!
//! ```text
//! offset  size  field
//!   0      3    pos      local block coordinates, 0..=16 (0..=96 on Y)
//!   3      1    normal   face index 0..=5 (see [`Face`])
//!   4      1    tile     index into the 16x16 texture atlas
//!   5      1    ao       ambient occlusion, 0 (dark) ..= 3 (open)
//!   6      1    u        quad corner along the first in-plane axis
//!   7      1    v        quad corner along the second in-plane axis
//!   8      1    quad_w   merged quad width, so the shader can tile the texture
//!   9      1    quad_h   merged quad height
//!  10      1    flags    bit 0 = translucent (water / cutout) pass
//!  11      1    _pad     keeps the struct 4-byte aligned
//! ```
//!
//! Positions are `u8` rather than `f32` because a column is only 96 blocks
//! tall: the client adds the column origin in the vertex shader, so float
//! precision never enters the pipeline.

use std::sync::OnceLock;

use crate::block::{registry, BlockId, BlockRegistry};
use crate::chunk::{Column, CHUNK_X, CHUNK_Y, CHUNK_Z};

/// Bytes per vertex. The renderer's buffer layout depends on this.
pub const VERTEX_STRIDE: usize = 12;

/// Indices per quad.
pub const QUAD_INDICES: usize = 6;

/// Largest in-plane extent any mask can have (Y x X or Y x Z).
const MAX_MASK: usize = CHUNK_Y * CHUNK_X;

/// The six face directions, in the order used by [`Vertex::normal`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Face {
    /// -X
    NegX = 0,
    /// +X
    PosX = 1,
    /// -Y
    NegY = 2,
    /// +Y
    PosY = 3,
    /// -Z
    NegZ = 4,
    /// +Z
    PosZ = 5,
}

impl Face {
    /// Axis this face is perpendicular to: 0 = X, 1 = Y, 2 = Z.
    #[inline]
    pub const fn axis(self) -> usize {
        (self as usize) / 2
    }

    /// `+1` for a positive face, `-1` for a negative one.
    #[inline]
    pub const fn sign(self) -> i32 {
        if (self as usize) & 1 == 1 {
            1
        } else {
            -1
        }
    }
}

/// One vertex. See the module docs for the byte layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(C)]
pub struct Vertex {
    /// Local block coordinate; each component is `0..=16` (`0..=96` on Y).
    pub pos: [u8; 3],
    /// Face index, see [`Face`].
    pub normal: u8,
    /// Texture atlas tile.
    pub tile: u8,
    /// Ambient occlusion, `0..=3`.
    pub ao: u8,
    /// Corner along the first in-plane axis.
    pub u: u8,
    /// Corner along the second in-plane axis.
    pub v: u8,
    /// Merged quad extent along the first in-plane axis.
    pub quad_w: u8,
    /// Merged quad extent along the second in-plane axis.
    pub quad_h: u8,
    /// Bit 0 marks a quad for the translucent pass.
    pub flags: u8,
    /// Padding to keep the struct 4-byte aligned.
    pub _pad: u8,
}

/// A meshed column, ready to upload.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    /// Interleaved vertices, [`VERTEX_STRIDE`] bytes each.
    pub vertices: Vec<Vertex>,
    /// Triangle indices into `vertices`.
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Number of quads (four vertices each).
    #[inline]
    pub fn quad_count(&self) -> usize {
        self.vertices.len() / 4
    }

    /// True when there is nothing to draw.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// Vertices of quad `i`.
    pub fn quad(&self, i: usize) -> &[Vertex] {
        &self.vertices[i * 4..i * 4 + 4]
    }

    /// Vertices that carry the given normal.
    pub fn vertices_with_normal(&self, face: Face) -> impl Iterator<Item = &Vertex> {
        self.vertices.iter().filter(move |v| v.normal == face as u8)
    }

    /// The vertex data as raw bytes, in upload order.
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertices.len() * VERTEX_STRIDE);
        for v in &self.vertices {
            out.extend_from_slice(&v.pos);
            out.push(v.normal);
            out.push(v.tile);
            out.push(v.ao);
            out.push(v.u);
            out.push(v.v);
            out.push(v.quad_w);
            out.push(v.quad_h);
            out.push(v.flags);
            out.push(v._pad);
        }
        debug_assert_eq!(out.len(), self.vertices.len() * VERTEX_STRIDE);
        out
    }

    fn push_quad(&mut self, verts: [Vertex; 4], flip: bool) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&verts);
        // Split along the diagonal that keeps the AO gradient smooth. When the
        // corner AO values are anti-correlated the default diagonal produces a
        // visible crease, so the other one is used instead.
        if flip {
            self.indices.extend_from_slice(&[
                base + 1,
                base + 2,
                base + 3,
                base + 1,
                base + 3,
                base,
            ]);
        } else {
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// Everything that has to match for two adjacent quads to be merged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FaceKey {
    tile: u8,
    ao: [u8; 4],
}

/// Mesh a column in isolation.
///
/// Blocks outside the column are treated as air, so a column meshed on its own
/// renders faces on all four vertical borders. That is correct at the edge of
/// the render distance; interior columns use [`mesh_with_neighbours`].
pub fn mesh_column(col: &Column) -> Mesh {
    let y_max = col.highest_solid_y();
    mesh_with(y_max, |x, y, z| {
        if x < 0
            || y < 0
            || z < 0
            || x >= CHUNK_X as i32
            || y >= CHUNK_Y as i32
            || z >= CHUNK_Z as i32
        {
            return BlockId(0);
        }
        col.get(x as usize, y as usize, z as usize)
    })
}

/// Mesh a column using its four horizontal neighbours for border faces.
///
/// `None` means "open air", which is the correct answer at the edge of the
/// world. Diagonal neighbours are not stitched in v1: treating them as air can
/// only add a hidden face, never remove a visible one, so it is safe.
pub fn mesh_with_neighbours(
    col: &Column,
    neg_x: Option<&Column>,
    pos_x: Option<&Column>,
    neg_z: Option<&Column>,
    pos_z: Option<&Column>,
) -> Mesh {
    mesh_with(col.highest_solid_y(), |x, y, z| {
        if y < 0 || y >= CHUNK_Y as i32 {
            return BlockId(0);
        }
        let (cx, lx) = if x < 0 {
            (-1, x + CHUNK_X as i32)
        } else if x >= CHUNK_X as i32 {
            (1, x - CHUNK_X as i32)
        } else {
            (0, x)
        };
        let (cz, lz) = if z < 0 {
            (-1, z + CHUNK_Z as i32)
        } else if z >= CHUNK_Z as i32 {
            (1, z - CHUNK_Z as i32)
        } else {
            (0, z)
        };
        let neighbour = match (cx, cz) {
            (0, 0) => return col.get(lx as usize, y as usize, lz as usize),
            (-1, 0) => neg_x,
            (1, 0) => pos_x,
            (0, -1) => neg_z,
            (0, 1) => pos_z,
            _ => None,
        };
        match neighbour {
            Some(n) => n.get(lx as usize, y as usize, lz as usize),
            None => BlockId(0),
        }
    })
}

/// Should a face of `owner` be drawn when the cell in front of it is `open`?
///
/// * air never draws.
/// * an opaque block draws unless the cell in front is also opaque.
/// * a non-opaque block (water, leaves) draws only against air, so a body of
///   water has one surface instead of 16,000 internal faces.
#[inline]
fn face_is_visible(r: &BlockRegistry, owner: BlockId, open: BlockId) -> bool {
    if owner.0 == 0 {
        return false;
    }
    if r.is_opaque(owner) {
        !r.is_opaque(open)
    } else {
        open.0 == 0
    }
}

/// Mesh using `sample`, skipping everything above `y_max` (the highest
/// non-air cell in this column).
///
/// The skip is not a micro-optimisation: a terrain column is ~40% solid and the
/// other 60% is empty sky, and every empty cell costs two block lookups per
/// face direction. Clamping the sweep to the occupied range roughly halves the
/// mesh time, which is what takes this from "just inside the 1.2 ms budget" to
/// "comfortably inside it".
fn mesh_with(y_max: usize, sample: impl Fn(i32, i32, i32) -> BlockId) -> Mesh {
    let r = registry();
    let dims = [CHUNK_X, CHUNK_Y, CHUNK_Z];
    let y_max = y_max.min(CHUNK_Y - 1);
    let mut mesh = Mesh::default();
    let mut mask: Vec<Option<FaceKey>> = vec![None; MAX_MASK];

    for face in [
        Face::NegX,
        Face::PosX,
        Face::NegY,
        Face::PosY,
        Face::NegZ,
        Face::PosZ,
    ] {
        let d = face.axis();
        let u = (d + 1) % 3;
        let v = (d + 2) % 3;
        let sign = face.sign();
        let (du, dv) = (dims[u], dims[v]);
        let cells = du * dv;

        // Clamp whichever in-plane axis is Y, plus the slice range for the Y
        // face, so empty sky is never visited.
        let (ui_max, vi_max, s_max) = match d {
            0 => (y_max, dv - 1, dims[d] - 1),
            1 => (du - 1, dv - 1, y_max),
            _ => (du - 1, y_max, dims[d] - 1),
        };

        // `s` is the cell index on the negative side of plane `s + 1`, so it
        // has to start at -1: the -X/-Y/-Z face of the block at cell 0 lives on
        // plane 0, and starting at 0 silently drops the entire -X wall.
        for s in -1..=s_max as i32 {
            mask[..cells].fill(None);
            let mut any = false;

            for vi in 0..=vi_max {
                for ui in 0..=ui_max {
                    // `p` is the cell on the negative side of the plane at
                    // `s + 1`, `q` the cell on the positive side.
                    let mut p = [0i32; 3];
                    p[d] = s;
                    p[u] = ui as i32;
                    p[v] = vi as i32;
                    let mut q = p;
                    q[d] += 1;

                    let (owner_pos, open_pos) = if sign > 0 { (p, q) } else { (q, p) };
                    let owner = sample(owner_pos[0], owner_pos[1], owner_pos[2]);
                    let open = sample(open_pos[0], open_pos[1], open_pos[2]);
                    if !face_is_visible(r, owner, open) {
                        continue;
                    }

                    mask[ui * dv + vi] = Some(FaceKey {
                        tile: tile_of(r, owner),
                        ao: corner_ao(&sample, r, &open_pos, u, v),
                    });
                    any = true;
                }
            }

            if any {
                emit_greedy(
                    &mut mesh,
                    &mut mask[..cells],
                    du,
                    dv,
                    face,
                    (s + 1) as usize,
                    u,
                    v,
                );
            }
        }
    }

    mesh
}

#[inline]
fn tile_of(r: &BlockRegistry, id: BlockId) -> u8 {
    let tile = r.def(id).tile;
    debug_assert!(tile < 256, "the 16x16 atlas has at most 256 tiles");
    tile as u8
}

/// Ambient occlusion for the four corners of a face.
///
/// `open` is the air cell immediately in front of the face. Corner order
/// matches the quad vertex order used by [`push_face`]:
/// `[(-1,-1), (+1,-1), (+1,+1), (-1,+1)]` in `(u, v)` space.
fn corner_ao(
    sample: &impl Fn(i32, i32, i32) -> BlockId,
    r: &BlockRegistry,
    open: &[i32; 3],
    u: usize,
    v: usize,
) -> [u8; 4] {
    const DIRS: [(i32, i32); 4] = [(-1, -1), (1, -1), (1, 1), (-1, 1)];
    let mut out = [3u8; 4];

    for (k, (du, dv)) in DIRS.iter().enumerate() {
        let mut side1 = *open;
        side1[u] += du;
        let mut side2 = *open;
        side2[v] += dv;
        let mut corner = *open;
        corner[u] += du;
        corner[v] += dv;

        let s1 = r.is_opaque(sample(side1[0], side1[1], side1[2])) as u8;
        let s2 = r.is_opaque(sample(side2[0], side2[1], side2[2])) as u8;
        let c = r.is_opaque(sample(corner[0], corner[1], corner[2])) as u8;
        // The classic voxel rule: two touching sides seal the corner entirely,
        // otherwise each occupied neighbour darkens it by one step.
        out[k] = if s1 == 1 && s2 == 1 {
            0
        } else {
            3 - (s1 + s2 + c)
        };
    }
    out
}

/// Greedy-merge the mask into rectangles and emit them.
#[allow(clippy::too_many_arguments)]
fn emit_greedy(
    mesh: &mut Mesh,
    mask: &mut [Option<FaceKey>],
    du: usize,
    dv: usize,
    face: Face,
    plane: usize,
    u: usize,
    v: usize,
) {
    for ui in 0..du {
        let mut vi = 0;
        while vi < dv {
            let Some(key) = mask[ui * dv + vi] else {
                vi += 1;
                continue;
            };

            // Grow along v first, then along u while every cell of the strip
            // matches. This is the standard greedy order and it produces the
            // fewest quads for axis-aligned terrain.
            let mut v_end = vi + 1;
            while v_end < dv && mask[ui * dv + v_end] == Some(key) {
                v_end += 1;
            }
            let mut u_end = ui + 1;
            'grow: while u_end < du {
                for vi2 in vi..v_end {
                    if mask[u_end * dv + vi2] != Some(key) {
                        break 'grow;
                    }
                }
                u_end += 1;
            }

            for ui2 in ui..u_end {
                for vi2 in vi..v_end {
                    mask[ui2 * dv + vi2] = None;
                }
            }

            push_face(mesh, face, plane, u, v, ui, u_end, vi, v_end, &key);
            vi = v_end;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_face(
    mesh: &mut Mesh,
    face: Face,
    plane_idx: usize,
    u: usize,
    v: usize,
    u0: usize,
    u1: usize,
    v0: usize,
    v1: usize,
    key: &FaceKey,
) {
    let plane = plane_idx as u8;
    let quad_w = (u1 - u0) as u8;
    let quad_h = (v1 - v0) as u8;
    let flags = u8::from(key.tile == water_tile());

    // Corner order: (-u,-v), (+u,-v), (+u,+v), (-u,+v).
    let corners = [
        (u0 as u8, v0 as u8),
        (u1 as u8, v0 as u8),
        (u1 as u8, v1 as u8),
        (u0 as u8, v1 as u8),
    ];

    let mut verts = [Vertex::default(); 4];
    for (i, (cu, cv)) in corners.iter().enumerate() {
        let mut pos = [0u8; 3];
        pos[face.axis()] = plane;
        pos[u] = *cu;
        pos[v] = *cv;
        verts[i] = Vertex {
            pos,
            normal: face as u8,
            tile: key.tile,
            ao: key.ao[i],
            u: *cu,
            v: *cv,
            quad_w,
            quad_h,
            flags,
            _pad: 0,
        };
    }

    // Winding: with u = (d+1)%3 and v = (d+2)%3 the basis (u, v, d) is
    // right-handed, so the counter-clockwise order above faces +d. A negative
    // face needs the reverse order.
    if face.sign() < 0 {
        verts.swap(1, 3);
    }

    // Flip the diagonal when the AO values are anti-correlated, which is
    // exactly when the default diagonal would make the gradient crease.
    let flip = key.ao[0] + key.ao[2] < key.ao[1] + key.ao[3];
    mesh.push_quad(verts, flip);
}

fn water_tile() -> u8 {
    static CELL: OnceLock<u8> = OnceLock::new();
    *CELL.get_or_init(|| {
        let r = registry();
        r.id_of("water").map(|id| tile_of(r, id)).unwrap_or(u8::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> BlockId {
        registry().id_of(name).expect("block must exist")
    }

    #[test]
    fn vertex_stride_is_twelve_bytes() {
        assert_eq!(
            core::mem::size_of::<Vertex>(),
            VERTEX_STRIDE,
            "the renderer hard-codes a 12-byte stride; do not add fields without \
             updating VERTEX_STRIDE and the shader"
        );
    }

    #[test]
    fn empty_column_produces_empty_mesh() {
        let col = Column::new();
        let mesh = mesh_column(&col);
        assert!(mesh.is_empty());
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn greedy_reduces_quads() {
        // A flat 16x16 plane at y = 0: top and bottom merge into one quad each
        // and the four sides into one quad each — six quads, where a naive
        // mesher would emit 256 top + 256 bottom + 64 sides.
        let mut col = Column::new();
        let grass = id("grass");
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                col.set(x, 0, z, grass);
            }
        }
        let mesh = mesh_column(&col);
        assert_eq!(
            mesh.quad_count(),
            6,
            "a flat 16x16 plane must merge into 6 quads, got {}",
            mesh.quad_count()
        );
        assert_eq!(
            mesh.vertices_with_normal(Face::PosY).count(),
            4,
            "the top face must be a single merged quad"
        );
        assert_eq!(mesh.indices.len(), mesh.quad_count() * QUAD_INDICES);
    }

    #[test]
    fn single_block_has_six_faces_and_full_ao() {
        let mut col = Column::new();
        col.set(8, 50, 8, id("stone"));
        let mesh = mesh_column(&col);
        assert_eq!(mesh.quad_count(), 6, "an isolated block has six faces");
        for v in &mesh.vertices {
            assert_eq!(v.ao, 3, "an exposed block must have fully open AO");
        }
    }

    #[test]
    fn ao_corners() {
        let stone = id("stone");

        // A free-standing block: every corner is open.
        let mut free = Column::new();
        free.set(20, 60, 20, stone);
        for v in &mesh_column(&free).vertices {
            assert_eq!(v.ao, 3);
        }

        // A concave corner: floor plus two walls meeting at (4, 4).
        let mut col = Column::new();
        for z in 4..12 {
            for x in 4..12 {
                col.set(x, 40, z, stone);
            }
        }
        for y in 41..46 {
            for z in 4..12 {
                col.set(4, y, z, stone);
            }
            for x in 4..12 {
                col.set(x, y, 4, stone);
            }
        }
        let mesh = mesh_column(&col);

        let darkest = mesh
            .vertices_with_normal(Face::PosY)
            .map(|v| v.ao)
            .min()
            .expect("the floor must have top faces");
        assert_eq!(
            darkest, 0,
            "the floor cell in the corner of two walls must be fully occluded"
        );

        for v in &mesh.vertices {
            assert!(v.ao <= 3, "AO must be 0..=3");
        }
    }

    #[test]
    fn mesh_is_deterministic() {
        let g = crate::gen::WorldGen::new(0x1234_5678, 2);
        let col = g.column(4, 4);
        let a = mesh_column(&col).as_bytes();
        let b = mesh_column(&col).as_bytes();
        assert_eq!(a, b, "meshing the same column twice must be byte-identical");
        assert!(!a.is_empty());
        assert_eq!(a.len() % VERTEX_STRIDE, 0);
    }

    #[test]
    fn positions_stay_inside_the_column() {
        let g = crate::gen::WorldGen::new(0xABCD, 2);
        let col = g.column(2, 2);
        for v in &mesh_column(&col).vertices {
            assert!(v.pos[0] <= CHUNK_X as u8, "x out of range: {}", v.pos[0]);
            assert!(v.pos[1] <= CHUNK_Y as u8, "y out of range: {}", v.pos[1]);
            assert!(v.pos[2] <= CHUNK_Z as u8, "z out of range: {}", v.pos[2]);
            assert!(v.normal <= Face::PosZ as u8);
            assert!(v.quad_w as usize <= CHUNK_Y);
            assert!(v.quad_h as usize <= CHUNK_Y);
        }
    }

    #[test]
    fn neighbour_stitching_hides_border_faces() {
        // Two identical solid columns side by side: the shared wall must be
        // culled, which is the entire point of mesh_with_neighbours.
        let stone = id("stone");
        let mut a = Column::new();
        let mut b = Column::new();
        for z in 0..CHUNK_Z {
            for y in 0..4 {
                for x in 0..CHUNK_X {
                    a.set(x, y, z, stone);
                    b.set(x, y, z, stone);
                }
            }
        }
        let alone = mesh_column(&a).quad_count();
        let stitched = mesh_with_neighbours(&a, None, Some(&b), None, None).quad_count();
        assert!(
            stitched < alone,
            "stitching must remove the +X wall ({stitched} !< {alone})"
        );
        assert!(stitched > 0);
    }

    #[test]
    fn water_has_one_surface_not_thousands_of_faces() {
        let mut col = Column::new();
        let water = id("water");
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                for y in 20..24 {
                    col.set(x, y, z, water);
                }
            }
        }
        let mesh = mesh_column(&col);
        assert!(
            mesh.vertices.iter().all(|v| v.flags & 1 == 1),
            "every water quad must be flagged for the translucent pass"
        );
        // A 4-deep 16x16 body of water: top, bottom and four sides.
        assert_eq!(mesh.quad_count(), 6, "internal water faces must be culled");
    }
}
