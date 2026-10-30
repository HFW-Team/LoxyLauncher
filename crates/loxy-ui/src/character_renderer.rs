use image::RgbaImage;
use slint::{Rgba8Pixel, SharedPixelBuffer};

#[derive(Clone, Copy)]
pub enum ArmModel {
    Classic, // 4px wide
    Slim,    // 3px wide (Alex)
}

struct Vertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

struct Quad {
    v: [Vertex; 4],
    normal: [f32; 3],
    is_cape: bool,
}

pub struct Character3DScene {
    pub yaw: f32,   // in radians
    pub pitch: f32, // in radians
    pub model: ArmModel,
}

impl Default for Character3DScene {
    fn default() -> Self {
        Self {
            yaw: -0.4,   // slightly turned to show 3/4 view
            pitch: 0.15, // slightly looking down
            model: ArmModel::Classic,
        }
    }
}

fn add_box_quads(
    quads: &mut Vec<Quad>,
    min: [f32; 3],
    max: [f32; 3],
    // u, v, w, h for 6 faces: [top, bottom, right, front, left, back]
    uvs: [[f32; 4]; 6],
    is_cape: bool,
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;

    // Face 0: Top (y = max)
    let [u, v, w, h] = uvs[0];
    quads.push(Quad {
        v: [
            Vertex { pos: [x0, y1, z0], uv: [u, v] },
            Vertex { pos: [x1, y1, z0], uv: [u + w, v] },
            Vertex { pos: [x1, y1, z1], uv: [u + w, v + h] },
            Vertex { pos: [x0, y1, z1], uv: [u, v + h] },
        ],
        normal: [0.0, 1.0, 0.0],
        is_cape,
    });

    // Face 1: Bottom (y = min)
    let [u, v, w, h] = uvs[1];
    quads.push(Quad {
        v: [
            Vertex { pos: [x0, y0, z1], uv: [u, v] },
            Vertex { pos: [x1, y0, z1], uv: [u + w, v] },
            Vertex { pos: [x1, y0, z0], uv: [u + w, v + h] },
            Vertex { pos: [x0, y0, z0], uv: [u, v + h] },
        ],
        normal: [0.0, -1.0, 0.0],
        is_cape,
    });

    // Face 2: Right (x = min)
    let [u, v, w, h] = uvs[2];
    quads.push(Quad {
        v: [
            Vertex { pos: [x0, y1, z0], uv: [u, v] },
            Vertex { pos: [x0, y1, z1], uv: [u + w, v] },
            Vertex { pos: [x0, y0, z1], uv: [u + w, v + h] },
            Vertex { pos: [x0, y0, z0], uv: [u, v + h] },
        ],
        normal: [-1.0, 0.0, 0.0],
        is_cape,
    });

    // Face 3: Front (z = max)
    let [u, v, w, h] = uvs[3];
    quads.push(Quad {
        v: [
            Vertex { pos: [x0, y1, z1], uv: [u, v] },
            Vertex { pos: [x1, y1, z1], uv: [u + w, v] },
            Vertex { pos: [x1, y0, z1], uv: [u + w, v + h] },
            Vertex { pos: [x0, y0, z1], uv: [u, v + h] },
        ],
        normal: [0.0, 0.0, 1.0],
        is_cape,
    });

    // Face 4: Left (x = max)
    let [u, v, w, h] = uvs[4];
    quads.push(Quad {
        v: [
            Vertex { pos: [x1, y1, z1], uv: [u, v] },
            Vertex { pos: [x1, y1, z0], uv: [u + w, v] },
            Vertex { pos: [x1, y0, z0], uv: [u + w, v + h] },
            Vertex { pos: [x1, y0, z1], uv: [u, v + h] },
        ],
        normal: [1.0, 0.0, 0.0],
        is_cape,
    });

    // Face 5: Back (z = min)
    let [u, v, w, h] = uvs[5];
    quads.push(Quad {
        v: [
            Vertex { pos: [x1, y1, z0], uv: [u, v] },
            Vertex { pos: [x0, y1, z0], uv: [u + w, v] },
            Vertex { pos: [x0, y0, z0], uv: [u + w, v + h] },
            Vertex { pos: [x1, y0, z0], uv: [u, v + h] },
        ],
        normal: [0.0, 0.0, -1.0],
        is_cape,
    });
}

pub fn render_minecraft_character(
    width: u32,
    height: u32,
    scene: &Character3DScene,
    skin_img: &RgbaImage,
    cape_img: Option<&RgbaImage>,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let mut z_buffer = vec![f32::NEG_INFINITY; (width * height) as usize];

    // Clear background to transparent
    let buf_slice = buffer.make_mut_bytes();
    for pixel in buf_slice.chunks_exact_mut(4) {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 0;
    }

    let mut quads: Vec<Quad> = Vec::with_capacity(80);

    // 1. HEAD (8x8x8)
    add_box_quads(
        &mut quads,
        [-4.0, 24.0, -4.0],
        [4.0, 32.0, 4.0],
        [
            [8.0, 0.0, 8.0, 8.0],   // Top
            [16.0, 0.0, 8.0, 8.0],  // Bottom
            [0.0, 8.0, 8.0, 8.0],   // Right
            [8.0, 8.0, 8.0, 8.0],   // Front
            [16.0, 8.0, 8.0, 8.0],  // Left
            [24.0, 8.0, 8.0, 8.0],  // Back
        ],
        false,
    );

    // HEAD OUTER (HAT) Layer
    add_box_quads(
        &mut quads,
        [-4.5, 23.5, -4.5],
        [4.5, 32.5, 4.5],
        [
            [40.0, 0.0, 8.0, 8.0],
            [48.0, 0.0, 8.0, 8.0],
            [32.0, 8.0, 8.0, 8.0],
            [40.0, 8.0, 8.0, 8.0],
            [48.0, 8.0, 8.0, 8.0],
            [56.0, 8.0, 8.0, 8.0],
        ],
        false,
    );

    // 2. TORSO (8x12x4)
    add_box_quads(
        &mut quads,
        [-4.0, 12.0, -2.0],
        [4.0, 24.0, 2.0],
        [
            [20.0, 16.0, 8.0, 4.0],
            [28.0, 16.0, 8.0, 4.0],
            [16.0, 20.0, 4.0, 12.0],
            [20.0, 20.0, 8.0, 12.0],
            [28.0, 20.0, 4.0, 12.0],
            [32.0, 20.0, 8.0, 12.0],
        ],
        false,
    );

    // TORSO OUTER (JACKET)
    add_box_quads(
        &mut quads,
        [-4.3, 11.7, -2.3],
        [4.3, 24.3, 2.3],
        [
            [20.0, 32.0, 8.0, 4.0],
            [28.0, 32.0, 8.0, 4.0],
            [16.0, 36.0, 4.0, 12.0],
            [20.0, 36.0, 8.0, 12.0],
            [28.0, 36.0, 4.0, 12.0],
            [32.0, 36.0, 8.0, 12.0],
        ],
        false,
    );

    // 3. RIGHT ARM
    let (arm_w, arm_uv_w) = match scene.model {
        ArmModel::Classic => (4.0, 4.0),
        ArmModel::Slim => (3.0, 3.0),
    };

    add_box_quads(
        &mut quads,
        [-4.0 - arm_w, 12.0, -2.0],
        [-4.0, 24.0, 2.0],
        [
            [44.0, 16.0, arm_uv_w, 4.0],
            [48.0, 16.0, arm_uv_w, 4.0],
            [40.0, 20.0, 4.0, 12.0],
            [44.0, 20.0, arm_uv_w, 12.0],
            [44.0 + arm_uv_w, 20.0, 4.0, 12.0],
            [48.0 + arm_uv_w, 20.0, arm_uv_w, 12.0],
        ],
        false,
    );

    // RIGHT SLEEVE
    add_box_quads(
        &mut quads,
        [-4.3 - arm_w, 11.7, -2.3],
        [-3.7, 24.3, 2.3],
        [
            [44.0, 32.0, arm_uv_w, 4.0],
            [48.0, 32.0, arm_uv_w, 4.0],
            [40.0, 36.0, 4.0, 12.0],
            [44.0, 36.0, arm_uv_w, 12.0],
            [44.0 + arm_uv_w, 36.0, 4.0, 12.0],
            [48.0 + arm_uv_w, 36.0, arm_uv_w, 12.0],
        ],
        false,
    );

    // 4. LEFT ARM
    add_box_quads(
        &mut quads,
        [4.0, 12.0, -2.0],
        [4.0 + arm_w, 24.0, 2.0],
        [
            [36.0, 48.0, arm_uv_w, 4.0],
            [40.0, 48.0, arm_uv_w, 4.0],
            [32.0, 52.0, 4.0, 12.0],
            [36.0, 52.0, arm_uv_w, 12.0],
            [36.0 + arm_uv_w, 52.0, 4.0, 12.0],
            [40.0 + arm_uv_w, 52.0, arm_uv_w, 12.0],
        ],
        false,
    );

    // LEFT SLEEVE
    add_box_quads(
        &mut quads,
        [3.7, 11.7, -2.3],
        [4.3 + arm_w, 24.3, 2.3],
        [
            [52.0, 48.0, arm_uv_w, 4.0],
            [56.0, 48.0, arm_uv_w, 4.0],
            [48.0, 52.0, 4.0, 12.0],
            [52.0, 52.0, arm_uv_w, 12.0],
            [52.0 + arm_uv_w, 52.0, 4.0, 12.0],
            [56.0 + arm_uv_w, 52.0, arm_uv_w, 12.0],
        ],
        false,
    );

    // 5. RIGHT LEG (4x12x4)
    add_box_quads(
        &mut quads,
        [-4.0, 0.0, -2.0],
        [0.0, 12.0, 2.0],
        [
            [4.0, 16.0, 4.0, 4.0],
            [8.0, 16.0, 4.0, 4.0],
            [0.0, 20.0, 4.0, 12.0],
            [4.0, 20.0, 4.0, 12.0],
            [8.0, 20.0, 4.0, 12.0],
            [12.0, 20.0, 4.0, 12.0],
        ],
        false,
    );

    // RIGHT PANT LEG
    add_box_quads(
        &mut quads,
        [-4.3, -0.3, -2.3],
        [0.3, 12.3, 2.3],
        [
            [4.0, 32.0, 4.0, 4.0],
            [8.0, 32.0, 4.0, 4.0],
            [0.0, 36.0, 4.0, 12.0],
            [4.0, 36.0, 4.0, 12.0],
            [8.0, 36.0, 4.0, 12.0],
            [12.0, 36.0, 4.0, 12.0],
        ],
        false,
    );

    // 6. LEFT LEG (4x12x4)
    add_box_quads(
        &mut quads,
        [0.0, 0.0, -2.0],
        [4.0, 12.0, 2.0],
        [
            [20.0, 48.0, 4.0, 4.0],
            [24.0, 48.0, 4.0, 4.0],
            [16.0, 52.0, 4.0, 12.0],
            [20.0, 52.0, 4.0, 12.0],
            [24.0, 52.0, 4.0, 12.0],
            [28.0, 52.0, 4.0, 12.0],
        ],
        false,
    );

    // LEFT PANT LEG
    add_box_quads(
        &mut quads,
        [-0.3, -0.3, -2.3],
        [4.3, 12.3, 2.3],
        [
            [4.0, 48.0, 4.0, 4.0],
            [8.0, 48.0, 4.0, 4.0],
            [0.0, 52.0, 4.0, 12.0],
            [4.0, 52.0, 4.0, 12.0],
            [8.0, 52.0, 4.0, 12.0],
            [12.0, 52.0, 4.0, 12.0],
        ],
        false,
    );

    // 7. CAPE (if equipped)
    if cape_img.is_some() {
        // Cape hangs behind back at z = -2.2, width 10 (x: -5..5), height 16 (y: 8..24)
        add_box_quads(
            &mut quads,
            [-5.0, 8.0, -3.2],
            [5.0, 24.0, -2.2],
            [
                [1.0, 0.0, 10.0, 1.0],   // Top
                [11.0, 0.0, 10.0, 1.0],  // Bottom
                [0.0, 1.0, 1.0, 16.0],   // Right edge
                [11.0, 1.0, 10.0, 16.0], // Front (inner face against back)
                [21.0, 1.0, 1.0, 16.0],  // Left edge
                [1.0, 1.0, 10.0, 16.0],  // Back (outer face seen from behind)
            ],
            true,
        );
    }

    // Camera transform constants
    let cos_yaw = scene.yaw.cos();
    let sin_yaw = scene.yaw.sin();
    let cos_pitch = scene.pitch.cos();
    let sin_pitch = scene.pitch.sin();

    let light_dir = [0.3f32, 0.7f32, 0.65f32];
    let light_len = (light_dir[0] * light_dir[0]
        + light_dir[1] * light_dir[1]
        + light_dir[2] * light_dir[2])
        .sqrt();
    let l_norm = [
        light_dir[0] / light_len,
        light_dir[1] / light_len,
        light_dir[2] / light_len,
    ];

    let center_x = width as f32 / 2.0;
    let center_y = height as f32 / 2.0;
    // Scale for character: 32 units tall, centered at y = 17.5
    let scale = (height as f32) / 38.0;

    let (skin_w, skin_h) = skin_img.dimensions();
    let (cape_w, cape_h) = cape_img
        .map(|c| c.dimensions())
        .unwrap_or((64, 32));

    struct ProjectedQuad<'a> {
        quad: &'a Quad,
        pts: [[f32; 3]; 4],
        avg_z: f32,
        light: f32,
    }

    let mut projected: Vec<ProjectedQuad> = Vec::with_capacity(quads.len());

    for quad in &quads {
        // Rotate normal
        let [nx0, ny0, nz0] = quad.normal;
        let nx1 = nx0 * cos_yaw + nz0 * sin_yaw;
        let nz1 = -nx0 * sin_yaw + nz0 * cos_yaw;
        let ny2 = ny0 * cos_pitch - nz1 * sin_pitch;
        let nz2 = ny0 * sin_pitch + nz1 * cos_pitch;
        let nx2 = nx1;

        // Backface culling: cull any face facing away from the camera
        if nz2 <= 0.001 {
            continue;
        }

        let mut pts = [[0.0f32; 3]; 4];
        let mut sum_z = 0.0;

        for (i, v) in quad.v.iter().enumerate() {
            let [x, y, z] = v.pos;
            // Rotate Yaw (around Y)
            let x1 = x * cos_yaw + z * sin_yaw;
            let z1 = -x * sin_yaw + z * cos_yaw;
            // Rotate Pitch (around X)
            let y2 = y * cos_pitch - z1 * sin_pitch;
            let z2 = y * sin_pitch + z1 * cos_pitch;

            pts[i] = [x1, y2, z2];
            sum_z += z2;
        }

        let dot = (nx2 * l_norm[0] + ny2 * l_norm[1] + nz2 * l_norm[2]).max(0.0);
        let light = 0.55 + 0.45 * dot;

        projected.push(ProjectedQuad {
            quad,
            pts,
            avg_z: sum_z / 4.0,
            light,
        });
    }

    // Sort by depth (farthest first)
    projected.sort_by(|a, b| {
        a.avg_z
            .partial_cmp(&b.avg_z)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let w_i32 = width as i32;
    let h_i32 = height as i32;

    // Rasterize each projected quad
    for pq in projected {
        let quad = pq.quad;
        let p = pq.pts;
        // Vertically centered at y = 17.5
        let scr = [
            [
                center_x + p[0][0] * scale,
                center_y - (p[0][1] - 17.5) * scale,
                p[0][2],
            ],
            [
                center_x + p[1][0] * scale,
                center_y - (p[1][1] - 17.5) * scale,
                p[1][2],
            ],
            [
                center_x + p[2][0] * scale,
                center_y - (p[2][1] - 17.5) * scale,
                p[2][2],
            ],
            [
                center_x + p[3][0] * scale,
                center_y - (p[3][1] - 17.5) * scale,
                p[3][2],
            ],
        ];

        let tex: &RgbaImage = if quad.is_cape {
            if let Some(c) = cape_img {
                c
            } else {
                skin_img
            }
        } else {
            skin_img
        };
        let (t_w, t_h) = if quad.is_cape {
            (cape_w, cape_h)
        } else {
            (skin_w, skin_h)
        };

        // Render two triangles: (0, 1, 2) and (0, 2, 3)
        for (i0, i1, i2) in [(0, 1, 2), (0, 2, 3)] {
            let p0 = scr[i0];
            let p1 = scr[i1];
            let p2 = scr[i2];
            let uv0 = quad.v[i0].uv;
            let uv1 = quad.v[i1].uv;
            let uv2 = quad.v[i2].uv;

            let min_x = (p0[0].min(p1[0]).min(p2[0]).floor() as i32).clamp(0, w_i32 - 1);
            let max_x = (p0[0].max(p1[0]).max(p2[0]).ceil() as i32).clamp(0, w_i32 - 1);
            let min_y = (p0[1].min(p1[1]).min(p2[1]).floor() as i32).clamp(0, h_i32 - 1);
            let max_y = (p0[1].max(p1[1]).max(p2[1]).ceil() as i32).clamp(0, h_i32 - 1);

            let denom = (p1[1] - p2[1]) * (p0[0] - p2[0]) + (p2[0] - p1[0]) * (p0[1] - p2[1]);
            if denom.abs() < 0.0001 {
                continue;
            }

            for y in min_y..=max_y {
                let fy = y as f32 + 0.5;
                for x in min_x..=max_x {
                    let fx = x as f32 + 0.5;

                    let w0 = ((p1[1] - p2[1]) * (fx - p2[0]) + (p2[0] - p1[0]) * (fy - p2[1]))
                        / denom;
                    let w1 = ((p2[1] - p0[1]) * (fx - p2[0]) + (p0[0] - p2[0]) * (fy - p2[1]))
                        / denom;
                    let w2 = 1.0 - w0 - w1;

                    if w0 >= -0.001 && w1 >= -0.001 && w2 >= -0.001 {
                        let z = w0 * p0[2] + w1 * p1[2] + w2 * p2[2];
                        let idx = (y as usize) * (width as usize) + (x as usize);

                        if z > z_buffer[idx] - 0.001 {
                            let u = w0 * uv0[0] + w1 * uv1[0] + w2 * uv2[0];
                            let v = w0 * uv0[1] + w1 * uv1[1] + w2 * uv2[1];

                            let tu = (u.floor() as u32).clamp(0, t_w.saturating_sub(1));
                            let tv = (v.floor() as u32).clamp(0, t_h.saturating_sub(1));

                            let pixel = tex.get_pixel(tu, tv);
                            let a = pixel[3];
                            if a < 20 {
                                continue;
                            }

                            // Write Z
                            z_buffer[idx] = z;

                            let r = ((pixel[0] as f32 * pq.light).clamp(0.0, 255.0)) as u8;
                            let g = ((pixel[1] as f32 * pq.light).clamp(0.0, 255.0)) as u8;
                            let b = ((pixel[2] as f32 * pq.light).clamp(0.0, 255.0)) as u8;

                            let b_slice = buffer.make_mut_bytes();
                            let p_off = idx * 4;
                            if a == 255 {
                                b_slice[p_off] = r;
                                b_slice[p_off + 1] = g;
                                b_slice[p_off + 2] = b;
                                b_slice[p_off + 3] = 255;
                            } else {
                                // Alpha blend
                                let alpha_f = a as f32 / 255.0;
                                let inv_a = 1.0 - alpha_f;
                                b_slice[p_off] = (r as f32 * alpha_f + b_slice[p_off] as f32 * inv_a) as u8;
                                b_slice[p_off + 1] = (g as f32 * alpha_f + b_slice[p_off + 1] as f32 * inv_a) as u8;
                                b_slice[p_off + 2] = (b as f32 * alpha_f + b_slice[p_off + 2] as f32 * inv_a) as u8;
                                b_slice[p_off + 3] = (b_slice[p_off + 3].max(a)) as u8;
                            }
                        }
                    }
                }
            }
        }
    }

    buffer
}
